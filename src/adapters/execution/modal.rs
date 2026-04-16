//! ModalBackend — serverless containers on Modal via the `modal` CLI.
//!
//! How it works:
//!   1. `launch()` calls `modal run thala_worker.py` with env vars encoding
//!      the task prompt, callback URL, and GitHub branch.
//!   2. `observe()` calls `modal app logs` and tracks a cursor for stall detection.
//!   3. `cancel()` calls `modal app stop`.
//!   4. Completion is signaled by a callback POST to Thala's gateway (not polling).
//!
//! Runtime state that must be persisted (stored in TaskRun):
//!   - job_handle.job_id: Modal app/call ID (e.g. "ap-abc123" or "fc-xyz789")
//!   - remote_branch: the branch pushed to origin before spawning
//!   - callback_token_hash: SHA-256 of the per-run bearer token

use std::path::Path;

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, WorkerHandle};
use crate::ports::execution::{ExecutionBackend, LaunchRequest, LaunchedRun};

// ── ModalConfig ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ModalConfig {
    /// The Modal app name or entrypoint (e.g. "thala_worker").
    pub app_name: String,

    /// Docker image reference pushed to Modal's registry.
    pub image: String,

    /// Modal environment/workspace to run in.
    pub environment: Option<String>,
}

// ── ModalBackend ──────────────────────────────────────────────────────────────

pub struct ModalBackend {
    config: ModalConfig,
}

impl ModalBackend {
    pub fn new(config: ModalConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl ExecutionBackend for ModalBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Modal
    }

    fn is_local(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "modal"
    }

    async fn launch(&self, req: LaunchRequest) -> Result<LaunchedRun, ThalaError> {
        // Validate required remote fields.
        let remote_branch = req.remote_branch.as_deref().ok_or_else(|| {
            ThalaError::backend("modal", "remote_branch is required for Modal backend")
        })?;

        let callback_url = req.callback_url.as_deref().ok_or_else(|| {
            ThalaError::backend("modal", "callback_url is required for Modal backend")
        })?;

        let github_repo = req.github_repo.as_deref().ok_or_else(|| {
            ThalaError::backend("modal", "github_repo is required for Modal backend")
        })?;

        let github_token = req.github_token.as_deref().ok_or_else(|| {
            ThalaError::backend("modal", "github_token is required for Modal backend")
        })?;

        // Build the `modal run` command.
        // `app_name` is the Python module entrypoint (e.g. "thala_worker.py").
        // Env vars carry all task context; the worker reads them at startup.
        let mut cmd = tokio::process::Command::new("modal");
        cmd.args(["run", &self.config.app_name]);

        // Pass config as environment variables.
        cmd.env("THALA_RUN_ID", &req.run_id)
            .env("THALA_TASK_ID", &req.task_id)
            .env("THALA_PROMPT_B64", base64_encode(&req.prompt))
            .env("THALA_MODEL", &req.model)
            .env("THALA_BRANCH", remote_branch)
            .env("THALA_CALLBACK_URL", callback_url)
            .env("GITHUB_REPO", github_repo)
            .env("GITHUB_TOKEN", github_token);

        if let Some(token) = &req.callback_token {
            cmd.env("THALA_CALLBACK_TOKEN", token);
        }

        if let Some(env) = &self.config.environment {
            cmd.env("MODAL_ENVIRONMENT", env);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| ThalaError::backend("modal", format!("modal run failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ThalaError::backend(
                "modal",
                format!("modal run exited non-zero: {}", stderr.trim()),
            ));
        }

        // Parse the app/function-call ID from stdout.
        // Modal CLI prints a line containing "ap-<id>" (app) or "fc-<id>" (function call).
        // Falls back to run_id if neither prefix is found (e.g. older CLI versions).
        let stdout = String::from_utf8_lossy(&output.stdout);
        let job_id = parse_modal_job_id(&stdout).unwrap_or_else(|| req.run_id.clone());

        tracing::info!(
            task_id = %req.task_id,
            job_id = %job_id,
            branch = %remote_branch,
            "Modal worker spawned"
        );

        Ok(LaunchedRun {
            handle: WorkerHandle {
                job_id,
                backend: ExecutionBackendKind::Modal,
            },
            worktree_path: None,
            remote_branch: Some(remote_branch.to_string()),
        })
    }

    async fn observe(&self, handle: &WorkerHandle) -> Result<RunObservation, ThalaError> {
        // Fetch the last 20 lines of logs from the running Modal app/function call.
        // The log cursor is the SHA-256 of the output, changing only when new output arrives.
        // This is used purely for stall detection; completion is signaled via callback.
        let mut cmd = tokio::process::Command::new("modal");
        cmd.args(["app", "logs", &handle.job_id, "--tail", "20"]);

        if let Some(env) = &self.config.environment {
            cmd.env("MODAL_ENVIRONMENT", env);
        }

        match cmd.output().await {
            Ok(output) if output.status.success() => {
                let log_text = String::from_utf8_lossy(&output.stdout).to_string();
                let cursor = hash_string(&log_text);

                tracing::debug!(job_id = %handle.job_id, "Modal: observed log output");

                Ok(RunObservation {
                    cursor,
                    is_alive: true,
                    observed_at: chrono::Utc::now(),
                })
            }
            Ok(output) => {
                // Non-zero exit typically means the app has stopped.
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::debug!(
                    job_id = %handle.job_id,
                    stderr = %stderr.trim(),
                    "Modal: app logs returned non-zero — assuming stopped"
                );
                Ok(RunObservation {
                    cursor: "stopped".into(),
                    is_alive: false,
                    observed_at: chrono::Utc::now(),
                })
            }
            Err(e) => {
                // If the `modal` CLI is unavailable, return alive=true so we don't
                // accidentally crash a live worker. The stall timeout will eventually
                // fire if there is genuinely no progress.
                tracing::warn!(job_id = %handle.job_id, "Failed to run `modal app logs`: {e}");
                Ok(RunObservation {
                    cursor: handle.job_id.clone(),
                    is_alive: true,
                    observed_at: chrono::Utc::now(),
                })
            }
        }
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), ThalaError> {
        tracing::info!(job_id = %handle.job_id, "Stopping Modal app");

        let mut cmd = tokio::process::Command::new("modal");
        cmd.args(["app", "stop", &handle.job_id]);

        if let Some(env) = &self.config.environment {
            cmd.env("MODAL_ENVIRONMENT", env);
        }

        match cmd.output().await {
            Ok(output) if output.status.success() => {
                tracing::info!(job_id = %handle.job_id, "Modal app stopped successfully");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Treat stop errors as warnings — the app may already be stopped.
                tracing::warn!(
                    job_id = %handle.job_id,
                    "modal app stop returned non-zero: {}",
                    stderr.trim()
                );
            }
            Err(e) => {
                tracing::warn!(job_id = %handle.job_id, "Failed to run `modal app stop`: {e}");
            }
        }

        Ok(())
    }

    async fn cleanup(
        &self,
        handle: &WorkerHandle,
        _workspace_root: &Path,
        task_id: &str,
    ) -> Result<(), ThalaError> {
        self.cancel(handle).await?;
        // No local worktree to remove.
        tracing::info!(task_id, job_id = %handle.job_id, "Modal cleanup complete");
        Ok(())
    }
}

fn base64_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// Hash a string for use as an observation cursor.
fn hash_string(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Parse the Modal app/function-call ID from `modal run` stdout.
///
/// Modal CLI prints a line containing the app ID (prefixed "ap-") or a
/// function-call ID (prefixed "fc-") in its output. We look for either prefix.
/// If neither is found we return None and the caller falls back to the run_id.
fn parse_modal_job_id(stdout: &str) -> Option<String> {
    // Modal CLI output examples:
    //   "✓ Created app: ap-abc123def456"
    //   "Running app: fc-xyz789uvw012"
    // We search for any token starting with "ap-" or "fc-".
    for line in stdout.lines() {
        for token in line.split_whitespace() {
            let token = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
            if token.starts_with("ap-") || token.starts_with("fc-") {
                return Some(token.to_string());
            }
        }
    }
    None
}
