//! Reconciler — restores consistent state after a restart.
//!
//! After a Thala restart, the StateStore holds persisted task and run records,
//! but the execution backends may have changed state while Thala was offline.
//!
//! The reconciler:
//!   1. Loads all non-terminal task records from the StateStore.
//!   2. For each record, fetches the canonical task spec from Beads.
//!   3. Checks the backend state for any active runs.
//!   4. Resolves discrepancies (run finished while offline, backend cleaned up, etc.).
//!   5. Emits events so the rest of the orchestrator can resume from the correct state.
//!
//! The reconciler runs once at startup, before the main loops begin.

use std::sync::Arc;

use crate::core::events::OrchestratorEvent;
use crate::core::run::RunStatus;
use crate::core::task::TaskStatus;
use crate::core::transitions::{apply_run_transition, apply_transition, RunTransition, Transition};
use crate::ports::backend_router::BackendRouter;
use crate::ports::state_store::StateStore;
use crate::ports::task_source::TaskSource;

// ── Reconciler ────────────────────────────────────────────────────────────────

pub struct Reconciler {
    store: Arc<dyn StateStore>,
    source: Arc<dyn TaskSource>,
    router: Arc<dyn BackendRouter>,
    events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
}

impl Reconciler {
    pub fn new(
        store: Arc<dyn StateStore>,
        source: Arc<dyn TaskSource>,
        router: Arc<dyn BackendRouter>,
        events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            store,
            source,
            router,
            events_tx,
        }
    }

    /// Run reconciliation once at startup.
    /// Returns the number of tasks recovered.
    pub async fn reconcile(&self) -> anyhow::Result<usize> {
        tracing::info!("Starting reconciliation");

        let active_tasks = self.store.active_tasks().await?;
        let mut recovered = 0;

        for record in &active_tasks {
            match self.reconcile_task(record).await {
                Ok(did_recover) => {
                    if did_recover {
                        recovered += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        task_id = %record.spec.id,
                        "Failed to reconcile task: {e}"
                    );
                }
            }
        }

        tracing::info!(
            total = active_tasks.len(),
            recovered,
            "Reconciliation complete"
        );
        Ok(recovered)
    }

    async fn reconcile_task(&self, record: &crate::core::task::TaskRecord) -> anyhow::Result<bool> {
        // Verify the task still exists in Beads (it may have been deleted).
        let spec = self.source.fetch_by_id(record.spec.id.as_str()).await?;
        if spec.is_none() {
            tracing::warn!(
                task_id = %record.spec.id,
                "Task no longer exists in Beads — marking resolved"
            );
            let mut updated = record.clone();
            // Only resolve if not already terminal.
            if !updated.status.is_terminal() {
                updated = apply_transition(&updated, Transition::HumanResolved)?;
                self.store.upsert_task(&updated).await?;
            }
            return Ok(true);
        }

        // For tasks with active runs, reconcile the run state.
        if let Some(run_id) = &record.active_run_id {
            if let Some(run) = self.store.get_run(run_id).await? {
                return self.reconcile_run(record, &run).await;
            }
        }

        // Tasks in Dispatching or Running with no run record are inconsistent.
        match record.status {
            TaskStatus::Dispatching | TaskStatus::Running => {
                tracing::warn!(
                    task_id = %record.spec.id,
                    status = %record.status.as_str(),
                    "Task in active status with no run — re-queuing as Ready"
                );
                let mut updated = record.clone();
                updated.status = TaskStatus::Ready;
                updated.active_run_id = None;
                updated.touch();
                self.store.upsert_task(&updated).await?;
                let _ = self
                    .events_tx
                    .send(OrchestratorEvent::dispatch_ready(updated.spec.id.clone()))
                    .await;
                return Ok(true);
            }
            _ => {}
        }

        Ok(false)
    }

    async fn reconcile_run(
        &self,
        record: &crate::core::task::TaskRecord,
        run: &crate::core::run::TaskRun,
    ) -> anyhow::Result<bool> {
        if run.status.is_terminal() {
            // Run is already terminal — task state should reflect that.
            // If the task is still marked Running, something failed mid-transition.
            if matches!(record.status, TaskStatus::Running) {
                tracing::warn!(
                    task_id = %record.spec.id,
                    run_id = %run.run_id,
                    run_status = %run.status.as_str(),
                    "Task still Running but run is terminal — reconciling"
                );
                let transition = match run.status {
                    RunStatus::Completed => Transition::RunCompleted,
                    RunStatus::Failed | RunStatus::Cancelled => Transition::RunFailed {
                        reason: "run was terminal at restart".into(),
                    },
                    RunStatus::TimedOut => Transition::RunStalled {
                        reason: "run timed out before restart".into(),
                    },
                    _ => unreachable!(),
                };

                let mut updated = record.clone();
                updated = apply_transition(&updated, transition)?;
                self.store.upsert_task(&updated).await?;

                // Emit appropriate event.
                match &updated.status {
                    TaskStatus::Validating => {
                        let _ = self
                            .events_tx
                            .send(OrchestratorEvent::run_completed(
                                record.spec.id.clone(),
                                run.run_id.clone(),
                            ))
                            .await;
                    }
                    _ => {
                        let _ = self
                            .events_tx
                            .send(OrchestratorEvent::run_failed(
                                record.spec.id.clone(),
                                run.run_id.clone(),
                                "reconciled after restart",
                            ))
                            .await;
                    }
                }
                return Ok(true);
            }
        } else {
            // Run is not terminal — check if the backend still has it.
            let Some(handle) = &run.handle else {
                return Ok(false);
            };

            let backend = self.router.backend(&run.backend);
            match backend
                .observe(handle, run.last_observation_cursor.as_deref())
                .await
            {
                Ok(obs) => {
                    if !obs.is_alive {
                        // Worker died while Thala was offline.
                        tracing::warn!(
                            task_id = %record.spec.id,
                            run_id = %run.run_id,
                            "Worker not alive after restart — treating as completed"
                        );
                        let updated_run =
                            apply_run_transition(run, RunTransition::CompletionSignaled)?;
                        self.store.upsert_run(&updated_run).await?;

                        let _ = self
                            .events_tx
                            .send(OrchestratorEvent::run_completed(
                                record.spec.id.clone(),
                                run.run_id.clone(),
                            ))
                            .await;
                        return Ok(true);
                    }
                    // Worker is still alive — nothing to do, monitor will pick it up.
                }
                Err(e) => {
                    tracing::warn!(
                        task_id = %record.spec.id,
                        run_id = %run.run_id,
                        "Failed to observe run during reconciliation: {e}"
                    );
                }
            }
        }

        Ok(false)
    }
}
