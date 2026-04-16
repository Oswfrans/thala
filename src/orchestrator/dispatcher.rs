//! Dispatcher — builds execution context and launches runs.
//!
//! The dispatcher consumes DispatchReady events from the scheduler and:
//!   1. Loads the TaskSpec from the state store (or fetches from Beads).
//!   2. Creates or updates the TaskRecord.
//!   3. Routes to an execution backend via BackendRouter.
//!   4. Builds the LaunchRequest (prompt, model, workspace).
//!   5. For remote backends: pushes the task branch to origin first.
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
use crate::core::run::TaskRun;
use crate::core::task::TaskRecord;
use crate::core::transitions::{apply_transition, Transition};
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

        // Transition to Dispatching.
        record = apply_transition(&record, Transition::BeginDispatching)?;
        self.store.upsert_task(&record).await?;

        // Determine backend — honour a reroute hint from a human action if set.
        let backend_kind = if let Some(hint) = record.reroute_hint.clone() {
            tracing::info!(
                task_id = %task_id,
                backend = %hint.as_str(),
                "Using reroute hint from human action"
            );
            hint
        } else {
            self.router
                .route(&record.spec, &self.workflow, record.attempt)
        };
        let backend = self.router.backend(&backend_kind);

        // Clear the reroute hint so it is not re-applied on subsequent retries.
        if record.reroute_hint.is_some() {
            record.reroute_hint = None;
            self.store.upsert_task(&record).await?;
        }

        // Create the run record.
        let run_id = RunId::new_v4();
        let mut run = TaskRun::new(
            run_id.clone(),
            task_id.clone(),
            record.attempt + 1,
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

        // For remote backends: push a task branch to origin before spawning.
        let remote_branch = if backend.is_local() {
            None
        } else {
            let branch = format!("task/{}", task_id.as_str());
            let github_token =
                std::env::var(&self.workflow.execution.github_token_env).unwrap_or_default();

            self.repo
                .push_branch(&self.config.workspace_root, &branch, &github_token)
                .await?;

            run.remote_branch = Some(branch.clone());
            Some(branch)
        };

        // Determine model before building the prompt (template may reference it).
        let model = record
            .spec
            .model_override
            .clone()
            .unwrap_or_else(|| self.workflow.models.worker.clone());

        // Build the prompt using the Tera template from WORKFLOW.md when available,
        // falling back to a minimal format when the template is absent or fails.
        let prompt = if let Some(template) = &self.config.prompt_template {
            let builder = PromptBuilder::new(template);
            match builder.render(&record, &self.workflow, &model) {
                Ok(rendered) => rendered,
                Err(e) => {
                    tracing::error!(
                        task_id = %task_id,
                        "Tera template render failed — skipping dispatch: {e}"
                    );
                    return Err(e.into());
                }
            }
        } else {
            fallback_prompt(&record)
        };

        // Build callback URL.
        let callback_url = self
            .workflow
            .execution
            .callback_base_url
            .as_deref()
            .map(|base| format!("{base}/api/worker/callback"));

        let github_token = std::env::var(&self.workflow.execution.github_token_env).ok();

        // Build and send the launch request.
        let req = LaunchRequest {
            run_id: run_id.as_str().to_string(),
            task_id: task_id.as_str().to_string(),
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
        };

        let launched = backend.launch(req).await?;

        // Update the run with handle and worktree info.
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
