//! Dispatcher — builds execution context and launches runs.
//!
//! The dispatcher consumes DispatchReady events from the scheduler and:
//!   1. Loads the TaskSpec from the state store (or fetches from Beads).
//!   2. Creates or updates the TaskRecord.
//!   3. Routes to an execution backend via BackendRouter.
//!   4. Builds the LaunchRequest (prompt, model, workspace).
//!   5. For remote backends: pushes a per-run task branch to origin first.
//!   6. Launches the run via ExecutionBackend.
//!   7. Persists the TaskRun and updated TaskRecord.
//!   8. Writes "in_progress" back to Beads via TaskSink.
//!   9. Emits RunLaunched.

use std::path::PathBuf;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::core::events::OrchestratorEvent;
use crate::core::ids::{RunId, TaskId};
use crate::core::run::{RunStatus, TaskRun};
use crate::core::task::{TaskRecord, TaskStatus};
use crate::core::transitions::{apply_run_transition, apply_transition, RunTransition, Transition};
use crate::core::workflow::WorkflowConfig;
use crate::orchestrator::prompt_builder::{fallback_prompt, PromptBuilder};
use crate::ports::backend_router::BackendRouter;
use crate::ports::execution::LaunchRequest;
use crate::ports::repo::RepoProvider;
use crate::ports::state_store::StateStore;
use crate::ports::task_sink::TaskSink;
use crate::ports::task_source::TaskSource;

// ── DispatcherConfig ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    pub workspace_root: PathBuf,
    pub product: String,

    /// Tera template body from WORKFLOW.md (everything after the front-matter).
    /// If absent, a fallback prompt is used.
    pub prompt_template: Option<String>,
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

pub struct Dispatcher {
    config: DispatcherConfig,
    workflow: WorkflowConfig,
    source: Arc<dyn TaskSource>,
    sink: Arc<dyn TaskSink>,
    store: Arc<dyn StateStore>,
    router: Arc<dyn BackendRouter>,
    repo: Arc<dyn RepoProvider>,
    events_tx: mpsc::Sender<OrchestratorEvent>,
}

impl Dispatcher {
    pub fn new(
        config: DispatcherConfig,
        workflow: WorkflowConfig,
        source: Arc<dyn TaskSource>,
        sink: Arc<dyn TaskSink>,
        store: Arc<dyn StateStore>,
        router: Arc<dyn BackendRouter>,
        repo: Arc<dyn RepoProvider>,
        events_tx: mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            workflow,
            source,
            sink,
            store,
            router,
            repo,
            events_tx,
        }
    }

    /// Handle one DispatchReady event.
    pub async fn dispatch(&self, task_id: TaskId) -> anyhow::Result<()> {
        // Load or create the task record.
        let mut record = self.load_or_create_record(&task_id).await?;

        // Extract and clear reroute hint before transitioning so both changes
        // land in the same upsert below.
        let reroute_hint = record.reroute_hint.take();

        // Transition to Dispatching. Retry and human-recovery paths can already
        // be in Dispatching; in that case the dispatcher owns the existing claim.
        if record.status != TaskStatus::Dispatching {
            record = apply_transition(&record, Transition::BeginDispatching)?;
        }
        // Always upsert here: persists both the status change and the cleared hint.
        self.store.upsert_task(&record).await?;

        let next_attempt = record.attempt + 1;

        // Determine backend — honour a reroute hint from a human action if set.
        let backend_kind = if let Some(hint) = reroute_hint {
            tracing::info!(
                task_id = %task_id,
                backend = %hint.as_str(),
                "Using reroute hint from human action"
            );
            hint
        } else {
            self.router
                .route(&record.spec, &self.workflow, next_attempt)
        };
        let backend = self.router.backend(&backend_kind);

        // Determine model before building the prompt (template may reference it).
        let model = record
            .spec
            .model_override
            .clone()
            .unwrap_or_else(|| self.workflow.models.worker.clone());

        // Build the prompt before any remote side effects. A bad WORKFLOW.md
        // template should fail this dispatch without pushing task branches or
        // launching workers.
        let prompt = if let Some(template) = &self.config.prompt_template {
            let builder = PromptBuilder::new(template);
            match builder.render(&record, &self.workflow, &model, next_attempt) {
                Ok(rendered) => rendered,
                Err(e) => {
                    tracing::error!(
                        task_id = %task_id,
                        "Tera template render failed — skipping dispatch: {e}"
                    );
                    self.mark_dispatch_failed(&task_id, format!("template render failed: {e}"))
                        .await?;
                    return Err(e.into());
                }
            }
        } else {
            fallback_prompt(&record)
        };

        // Create the run record.
        let run_id = RunId::new_v4();
        let mut run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            next_attempt,
            backend_kind.clone(),
        );

        // Generate per-run callback token for remote backends.
        let callback_token = if backend.is_local() {
            None
        } else {
            let token = uuid::Uuid::new_v4().to_string();
            run.callback_token_hash = Some(sha256_hex(token.as_bytes()));
            Some(token)
        };

        // For remote backends: push a per-run task branch to origin before
        // spawning. Retries must not reuse `task/<id>` because the remote
        // worker pushes its output there, leaving the local ref stale.
        let remote_branch = if backend.is_local() {
            None
        } else {
            let branch = remote_run_branch(&task_id, next_attempt, &run_id);
            let github_token =
                std::env::var(&self.workflow.execution.github_token_env).unwrap_or_default();

            if let Err(e) = self
                .repo
                .push_branch(&self.config.workspace_root, &branch, &github_token)
                .await
            {
                self.mark_dispatch_failed(&task_id, format!("failed to push branch: {e}"))
                    .await?;
                return Err(e.into());
            }

            run.remote_branch = Some(branch.clone());
            Some(branch)
        };

        // Build callback URL.
        let callback_url = self
            .workflow
            .execution
            .callback_base_url
            .as_deref()
            .map(|base| format!("{base}/api/worker/callback"));

        let github_token = std::env::var(&self.workflow.execution.github_token_env).ok();

        // Persist the launch attempt before starting remote work. Fast remote
        // failures can call back before `launch()` returns; the callback server
        // must be able to find the run and validate its token.
        self.store.upsert_run(&run).await?;

        // Build and send the launch request.
        let req = LaunchRequest {
            run_id: run_id.as_str().to_string(),
            task_id: task_id.as_str().to_string(),
            attempt: run.attempt,
            product: self.config.product.clone(),
            prompt,
            model,
            workspace_root: self.config.workspace_root.clone(),
            remote_branch,
            callback_url,
            callback_token,
            github_repo: (!self.workflow.github_repo.is_empty())
                .then(|| self.workflow.github_repo.clone()),
            github_token,
            after_create_hook: self.workflow.hooks.after_create.clone(),
            before_run_hook: self.workflow.hooks.before_run.clone(),
            after_run_hook: self.workflow.hooks.after_run.clone(),
            // before_cleanup is Thala-side only — not forwarded to the remote worker
        };

        let launched = match backend.launch(req).await {
            Ok(launched) => launched,
            Err(e) => {
                run = apply_run_transition(
                    &run,
                    RunTransition::FailureSignaled {
                        reason: format!("backend launch failed: {e}"),
                    },
                )?;
                self.store.upsert_run(&run).await?;
                self.mark_dispatch_failed(&task_id, format!("backend launch failed: {e}"))
                    .await?;
                return Err(e.into());
            }
        };

        // Update the run with handle and worktree info. Reload first because a
        // fast remote worker can complete and call back before `launch()`
        // returns.
        run = self.store.get_run(&run_id).await?.unwrap_or(run);
        let terminal_status = run.status.is_terminal().then_some(run.status.clone());
        run.handle = Some(launched.handle.clone());
        run.worktree_path = launched
            .worktree_path
            .map(|p| p.to_string_lossy().to_string());
        if let Some(rb) = &launched.remote_branch {
            run.remote_branch = Some(rb.clone());
        }
        self.store.upsert_run(&run).await?;

        // Transition task to Running.
        record = apply_transition(
            &record,
            Transition::RunLaunched {
                run_id: run_id.clone(),
            },
        )?;
        self.store.upsert_task(&record).await?;

        if let Some(status) = terminal_status {
            match status {
                RunStatus::Completed => {
                    record = apply_transition(&record, Transition::RunCompleted)?;
                    self.store.upsert_task(&record).await?;
                    let _ = self
                        .events_tx
                        .send(OrchestratorEvent::run_completed(
                            task_id.clone(),
                            run_id.clone(),
                        ))
                        .await;
                }
                RunStatus::Failed | RunStatus::TimedOut => {
                    let reason = "worker completed before launch returned".to_string();
                    record = match status {
                        RunStatus::TimedOut => apply_transition(
                            &record,
                            Transition::RunStalled {
                                reason: reason.clone(),
                            },
                        )?,
                        _ => apply_transition(
                            &record,
                            Transition::RunFailed {
                                reason: reason.clone(),
                            },
                        )?,
                    };
                    self.store.upsert_task(&record).await?;
                    let _ = self
                        .events_tx
                        .send(OrchestratorEvent::run_failed(
                            task_id.clone(),
                            run_id.clone(),
                            reason,
                        ))
                        .await;
                }
                RunStatus::Cancelled | RunStatus::Launching | RunStatus::Active => {}
            }
        }

        // Write in_progress back to Beads.
        if let Err(e) = self.sink.mark_in_progress(task_id.as_str()).await {
            tracing::warn!(task_id = %task_id, "Failed to update Beads to in_progress: {e}");
        }

        // Emit event.
        let _ = self
            .events_tx
            .send(OrchestratorEvent::run_launched(
                task_id,
                run_id,
                backend_kind,
            ))
            .await;

        Ok(())
    }

    async fn mark_dispatch_failed(&self, task_id: &TaskId, reason: String) -> anyhow::Result<()> {
        if let Some(record) = self.store.get_task(task_id).await? {
            if record.status == TaskStatus::Dispatching {
                let updated = apply_transition(
                    &record,
                    Transition::DispatchFailed {
                        reason: reason.clone(),
                    },
                )?;
                self.store.upsert_task(&updated).await?;
            }
        }

        let _ = self
            .events_tx
            .send(OrchestratorEvent::RunLaunchFailed {
                task_id: task_id.clone(),
                reason,
                at: chrono::Utc::now(),
            })
            .await;

        Ok(())
    }

    async fn load_or_create_record(&self, task_id: &TaskId) -> anyhow::Result<TaskRecord> {
        if let Some(existing) = self.store.get_task(task_id).await? {
            // Re-dispatch after a previous attempt.
            Ok(existing)
        } else {
            // First time seeing this task — fetch spec from Beads and create record.
            let spec = self
                .source
                .fetch_by_id(task_id.as_str())
                .await?
                .ok_or_else(|| anyhow::anyhow!("Task {} not found in Beads", task_id))?;
            let mut record = TaskRecord::new(spec);
            // Mark it ready before dispatching.
            record = apply_transition(&record, Transition::MarkReady)?;
            self.store.upsert_task(&record).await?;
            Ok(record)
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn remote_run_branch(task_id: &TaskId, attempt: u32, run_id: &RunId) -> String {
    format!(
        "task/{}-attempt-{}-{}",
        task_id.as_str(),
        attempt,
        run_id.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_run_branch_is_unique_per_attempt_and_run() {
        let task_id = TaskId::new("bd-123");
        let run_id_1 = RunId::from("11111111-1111-4111-8111-111111111111");
        let run_id_2 = RunId::from("22222222-2222-4222-8222-222222222222");

        assert_eq!(
            remote_run_branch(&task_id, 1, &run_id_1),
            "task/bd-123-attempt-1-11111111-1111-4111-8111-111111111111"
        );
        assert_ne!(
            remote_run_branch(&task_id, 1, &run_id_1),
            remote_run_branch(&task_id, 1, &run_id_2)
        );
        assert_ne!(
            remote_run_branch(&task_id, 1, &run_id_1),
            remote_run_branch(&task_id, 2, &run_id_1)
        );
    }
}
