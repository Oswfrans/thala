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

use std::path::Path;
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
        let observation = backend
            .observe(handle, run.last_observation_cursor.as_deref())
            .await?;

        match &run.status {
            RunStatus::Launching => {
                if let Some(terminal_status) = &observation.terminal_status {
                    self.handle_observed_terminal(run, terminal_status).await?;
                    return Ok(());
                }

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
                if let Some(terminal_status) = &observation.terminal_status {
                    self.handle_observed_terminal(run, terminal_status).await?;
                    return Ok(());
                }

                // If output changed, apply the cursor transition and store; otherwise
                // clone the original so downstream code always has an owned run.
                let cursor_changed = run
                    .last_observation_cursor
                    .as_deref()
                    .is_none_or(|prev| prev != observation.cursor);

                let run = if cursor_changed {
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
                            run_id: updated.run_id.clone(),
                            at: chrono::Utc::now(),
                        })
                        .await;
                    updated
                } else {
                    run.clone()
                };

                if let Some(signal) = self.read_local_signal(&run).await? {
                    self.handle_local_signal(&run, &signal).await?;
                    return Ok(());
                }

                if self.is_stalled(&run) {
                    tracing::warn!(
                        run_id = %run.run_id,
                        task_id = %run.task_id,
                        "Run stalled — transitioning to TimedOut"
                    );
                    let updated = apply_run_transition(&run, RunTransition::StallTimeout)?;
                    self.store.upsert_run(&updated).await?;
                    self.handle_run_timeout(&run.task_id, &run.run_id).await?;
                    return Ok(());
                }

                // Worker gone without a signal file — treat as crash.
                if !observation.is_alive {
                    tracing::warn!(
                        run_id = %run.run_id,
                        task_id = %run.task_id,
                        "Worker process died without signal file — treating as crash"
                    );
                    let reason = "worker process exited without writing signal file";
                    let updated = apply_run_transition(
                        &run,
                        RunTransition::FailureSignaled {
                            reason: reason.into(),
                        },
                    )?;
                    self.store.upsert_run(&updated).await?;
                    self.handle_run_failed(&run.task_id, &run.run_id, reason)
                        .await?;
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

    async fn handle_observed_terminal(
        &self,
        run: &TaskRun,
        terminal_status: &RunStatus,
    ) -> anyhow::Result<()> {
        let active_run;
        let run = if run.status == RunStatus::Launching
            && matches!(
                terminal_status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::TimedOut
            ) {
            // Fast polling backends can finish before the monitor observes the
            // intermediate Active state. Preserve the reducer invariant by
            // activating first, then applying the terminal transition.
            active_run = apply_run_transition(run, RunTransition::Activated)?;
            self.store.upsert_run(&active_run).await?;

            let _ = self
                .events_tx
                .send(OrchestratorEvent::RunStatusChanged {
                    run_id: active_run.run_id.clone(),
                    from: RunStatus::Launching,
                    to: RunStatus::Active,
                    at: chrono::Utc::now(),
                })
                .await;

            &active_run
        } else {
            run
        };

        match terminal_status {
            RunStatus::Completed => {
                self.complete_run(run).await?;
            }
            RunStatus::Failed => {
                let reason = "worker reported failure during polling";
                let updated = apply_run_transition(
                    run,
                    RunTransition::FailureSignaled {
                        reason: reason.into(),
                    },
                )?;
                self.store.upsert_run(&updated).await?;
                self.handle_run_failed(&run.task_id, &run.run_id, reason)
                    .await?;
            }
            RunStatus::Cancelled => {
                let updated = apply_run_transition(run, RunTransition::Cancelled)?;
                self.store.upsert_run(&updated).await?;
            }
            RunStatus::TimedOut => {
                let updated = apply_run_transition(run, RunTransition::StallTimeout)?;
                self.store.upsert_run(&updated).await?;
                self.handle_run_timeout(&run.task_id, &run.run_id).await?;
            }
            RunStatus::Launching | RunStatus::Active => {}
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

    async fn emit_run_completed(
        &self,
        task_id: &crate::core::ids::TaskId,
        run_id: &crate::core::ids::RunId,
    ) -> anyhow::Result<()> {
        let _ = self
            .events_tx
            .send(OrchestratorEvent::run_completed(
                task_id.clone(),
                run_id.clone(),
            ))
            .await;

        Ok(())
    }

    async fn complete_run(&self, run: &TaskRun) -> anyhow::Result<()> {
        if let Err(e) = self.run_after_run_hook(run).await {
            let reason = format!("after_run hook failed: {e}");
            tracing::warn!(run_id = %run.run_id, task_id = %run.task_id, "{reason}");
            let updated = apply_run_transition(
                run,
                RunTransition::FailureSignaled {
                    reason: reason.clone(),
                },
            )?;
            self.store.upsert_run(&updated).await?;
            self.handle_run_failed(&run.task_id, &run.run_id, &reason)
                .await?;
            return Ok(());
        }

        let updated = apply_run_transition(run, RunTransition::CompletionSignaled)?;
        self.store.upsert_run(&updated).await?;
        self.emit_run_completed(&run.task_id, &run.run_id).await
    }

    async fn handle_local_signal(&self, run: &TaskRun, signal: &str) -> anyhow::Result<()> {
        if let Some(reason) = signal.trim().strip_prefix("FAILED:") {
            let reason = reason.trim();
            let updated = apply_run_transition(
                run,
                RunTransition::FailureSignaled {
                    reason: reason.to_string(),
                },
            )?;
            self.store.upsert_run(&updated).await?;
            self.handle_run_failed(&run.task_id, &run.run_id, reason)
                .await?;
            return Ok(());
        }

        if signal.trim() == "DONE" {
            tracing::info!(
                run_id = %run.run_id,
                task_id = %run.task_id,
                "Worker completion signal found"
            );
            self.complete_run(run).await?;
            return Ok(());
        }

        tracing::warn!(
            run_id = %run.run_id,
            task_id = %run.task_id,
            signal = signal.trim(),
            "Unrecognised signal file content — expected DONE or FAILED: <reason>"
        );
        Ok(())
    }

    async fn read_local_signal(&self, run: &TaskRun) -> anyhow::Result<Option<String>> {
        let Some(signal_path) = signal_path(run) else {
            return Ok(None);
        };

        match tokio::fs::read_to_string(&signal_path).await {
            Ok(signal) => Ok(Some(signal)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn run_after_run_hook(&self, run: &TaskRun) -> anyhow::Result<()> {
        let Some(hook) = self
            .workflow
            .hooks
            .after_run
            .as_deref()
            .filter(|h| !h.trim().is_empty())
        else {
            return Ok(());
        };
        let Some(worktree_path) = &run.worktree_path else {
            return Ok(());
        };

        let output = tokio::process::Command::new("sh")
            .args(["-lc", hook])
            .current_dir(worktree_path)
            .output()
            .await?;

        if output.status.success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    async fn handle_run_failed(
        &self,
        task_id: &crate::core::ids::TaskId,
        run_id: &crate::core::ids::RunId,
        reason: &str,
    ) -> anyhow::Result<()> {
        if let Some(mut record) = self.store.get_task(task_id).await? {
            record = apply_transition(
                &record,
                Transition::RunFailed {
                    reason: reason.to_string(),
                },
            )?;
            self.store.upsert_task(&record).await?;
        }

        let _ = self
            .events_tx
            .send(OrchestratorEvent::run_failed(
                task_id.clone(),
                run_id.clone(),
                reason,
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

fn signal_path(run: &TaskRun) -> Option<std::path::PathBuf> {
    run.worktree_path.as_ref().map(|wt| {
        Path::new(wt)
            .join(".thala")
            .join("signals")
            .join(format!("{}.signal", run.task_id.as_str()))
    })
}
