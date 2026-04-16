//! Monitor — polls active runs and drives lifecycle transitions.
//!
//! The monitor runs on a separate interval from the scheduler and dispatcher.
//! Each tick it:
//!   1. Loads all active (non-terminal) runs from the StateStore.
//!   2. For each Active run:
//!      a. Polls the backend for an activity snapshot.
//!      b. Updates last_activity_at if output changed.
//!      c. Checks for stall timeout → TimedOut.
//!      d. Checks for completion signal (callback or signal file).
//!   3. For each Launching run:
//!      a. Polls the backend to see if the worker is now alive.
//!   4. Emits events for any state changes.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use crate::core::events::OrchestratorEvent;
use crate::core::ids::TaskId;
use crate::core::run::{RunStatus, TaskRun};
use crate::core::transitions::{apply_run_transition, apply_transition, RunTransition, Transition};
use crate::core::workflow::WorkflowConfig;
use crate::ports::backend_router::BackendRouter;
use crate::ports::state_store::StateStore;

// ── MonitorConfig ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// How often the monitor polls active runs.
    pub poll_interval: Duration,

    /// Milliseconds without output change before declaring a run stalled.
    pub stall_timeout_ms: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(15),
            stall_timeout_ms: 300_000,
        }
    }
}

// ── Monitor ───────────────────────────────────────────────────────────────────

pub struct Monitor {
    config: MonitorConfig,
    workflow: WorkflowConfig,
    store: Arc<dyn StateStore>,
    router: Arc<dyn BackendRouter>,
    events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
}

impl Monitor {
    pub fn new(
        config: MonitorConfig,
        workflow: WorkflowConfig,
        store: Arc<dyn StateStore>,
        router: Arc<dyn BackendRouter>,
        events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            workflow,
            store,
            router,
            events_tx,
        }
    }

    /// Run the monitor loop. This task runs until the process exits.
    pub async fn run(self) {
        tracing::info!(
            poll_interval_ms = self.config.poll_interval.as_millis(),
            stall_timeout_ms = self.config.stall_timeout_ms,
            "Monitor starting"
        );

        loop {
            if let Err(e) = self.tick().await {
                tracing::error!("Monitor tick failed: {e}");
            }
            sleep(self.config.poll_interval).await;
        }
    }

    async fn tick(&self) -> anyhow::Result<()> {
        let active_runs = self.store.active_runs().await?;

        for run in active_runs {
            if let Err(e) = self.observe_run(&run).await {
                tracing::warn!(
                    run_id = %run.run_id,
                    task_id = %run.task_id,
                    "Monitor failed to observe run: {e}"
                );
            }
        }

        Ok(())
    }

    async fn observe_run(&self, run: &TaskRun) -> anyhow::Result<()> {
        let Some(handle) = &run.handle else {
            // Still launching — no handle yet.
            if run.status == RunStatus::Launching {
                tracing::debug!(run_id = %run.run_id, "Run is launching, no handle yet");
            }
            return Ok(());
        };

        let backend = self.router.backend(&run.backend);
        let observation = backend.observe(handle).await?;

        match &run.status {
            RunStatus::Launching => {
                if observation.is_alive {
                    // Worker is now alive — transition to Active.
                    let updated = apply_run_transition(run, RunTransition::Activated)?;
                    self.store.upsert_run(&updated).await?;

                    let _ = self
                        .events_tx
                        .send(OrchestratorEvent::RunStatusChanged {
                            run_id: run.run_id.clone(),
                            from: RunStatus::Launching,
                            to: RunStatus::Active,
                            at: chrono::Utc::now(),
                        })
                        .await;
                }
            }

            RunStatus::Active => {
                // Check if output changed.
                let cursor_changed = run
                    .last_observation_cursor
                    .as_deref()
                    .is_none_or(|prev| prev != observation.cursor); // No previous cursor → treat as changed.

                if cursor_changed {
                    let updated = apply_run_transition(
                        run,
                        RunTransition::ActivityObserved {
                            cursor: observation.cursor.clone(),
                        },
                    )?;
                    self.store.upsert_run(&updated).await?;

                    let _ = self
                        .events_tx
                        .send(OrchestratorEvent::RunActivityObserved {
                            run_id: run.run_id.clone(),
                            at: chrono::Utc::now(),
                        })
                        .await;
                }

                // Check for stall timeout.
                if self.is_stalled(run) {
                    tracing::warn!(
                        run_id = %run.run_id,
                        task_id = %run.task_id,
                        "Run stalled — transitioning to TimedOut"
                    );
                    let updated = apply_run_transition(run, RunTransition::StallTimeout)?;
                    self.store.upsert_run(&updated).await?;
                    self.handle_run_timeout(&run.task_id, &run.run_id).await?;
                    return Ok(());
                }

                // Check for worker process gone (not alive, no completion signal).
                if !observation.is_alive {
                    // Distinguish clean exit (signal file present) from crash.
                    // The signal file is written by the worker at `.thala/signals/<task-id>.signal`
                    // inside the worktree. Its presence means the worker completed cleanly.
                    let clean_exit = run.worktree_path.as_ref().is_some_and(|wt| {
                        let signal_path = std::path::Path::new(wt)
                            .join(".thala")
                            .join("signals")
                            .join(format!("{}.signal", run.task_id.as_str()));
                        signal_path.exists()
                    }); // remote backends signal via callback, not file

                    if clean_exit {
                        tracing::info!(
                            run_id = %run.run_id,
                            task_id = %run.task_id,
                            "Worker exited cleanly (signal file found)"
                        );
                        let updated = apply_run_transition(run, RunTransition::CompletionSignaled)?;
                        self.store.upsert_run(&updated).await?;
                        self.handle_run_completed(&run.task_id, &run.run_id).await?;
                    } else {
                        tracing::warn!(
                            run_id = %run.run_id,
                            task_id = %run.task_id,
                            "Worker process died without signal file — treating as crash"
                        );
                        let updated = apply_run_transition(
                            run,
                            RunTransition::FailureSignaled {
                                reason: "worker process exited without writing signal file".into(),
                            },
                        )?;
                        self.store.upsert_run(&updated).await?;
                        self.handle_run_failed(&run.task_id, &run.run_id).await?;
                    }
                }
            }

            // Terminal runs should not appear in active_runs, but guard anyway.
            s if s.is_terminal() => {
                tracing::debug!(
                    run_id = %run.run_id,
                    status = %s.as_str(),
                    "Terminal run returned by active_runs — skipping"
                );
            }

            _ => {}
        }

        Ok(())
    }

    fn is_stalled(&self, run: &TaskRun) -> bool {
        let Some(last_activity) = run.last_activity_at else {
            // No activity ever observed. Check if stall timeout exceeded from run start.
            let elapsed = chrono::Utc::now()
                .signed_duration_since(run.started_at)
                .num_milliseconds();
            return u64::try_from(elapsed.max(0)).unwrap_or(0) > self.config.stall_timeout_ms;
        };

        let elapsed = chrono::Utc::now()
            .signed_duration_since(last_activity)
            .num_milliseconds();

        u64::try_from(elapsed.max(0)).unwrap_or(0) > self.config.stall_timeout_ms
    }

    async fn handle_run_completed(
        &self,
        task_id: &crate::core::ids::TaskId,
        run_id: &crate::core::ids::RunId,
    ) -> anyhow::Result<()> {
        // Update task status to Validating.
        if let Some(mut record) = self.store.get_task(task_id).await? {
            record = apply_transition(&record, Transition::RunCompleted)?;
            self.store.upsert_task(&record).await?;
        }

        let _ = self
            .events_tx
            .send(OrchestratorEvent::run_completed(
                task_id.clone(),
                run_id.clone(),
            ))
            .await;

        Ok(())
    }

    async fn handle_run_failed(
        &self,
        task_id: &crate::core::ids::TaskId,
        run_id: &crate::core::ids::RunId,
    ) -> anyhow::Result<()> {
        if let Some(mut record) = self.store.get_task(task_id).await? {
            record = apply_transition(
                &record,
                Transition::RunFailed {
                    reason: "worker crashed (no signal file)".into(),
                },
            )?;
            self.store.upsert_task(&record).await?;
        }

        let _ = self
            .events_tx
            .send(OrchestratorEvent::run_failed(
                task_id.clone(),
                run_id.clone(),
                "worker crashed (no signal file)",
            ))
            .await;

        Ok(())
    }

    async fn handle_run_timeout(
        &self,
        task_id: &TaskId,
        run_id: &crate::core::ids::RunId,
    ) -> anyhow::Result<()> {
        if let Some(mut record) = self.store.get_task(task_id).await? {
            record = apply_transition(
                &record,
                Transition::RunStalled {
                    reason: "stall timeout exceeded".into(),
                },
            )?;
            self.store.upsert_task(&record).await?;
        }

        // Emit RunTimedOut so the engine can create an escalation ticket.
        let _ = self
            .events_tx
            .send(OrchestratorEvent::RunTimedOut {
                task_id: task_id.clone(),
                run_id: run_id.clone(),
                at: chrono::Utc::now(),
            })
            .await;

        Ok(())
    }
}
