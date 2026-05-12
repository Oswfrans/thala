//! ModalBackend — serverless containers on Modal via the `modal` CLI.
//!
//! How it works:
//!   1. `launch()` calls `modal run --detach <app_file>` with env vars encoding
//!      the task prompt, callback URL, and GitHub branch.
//!   2. `observe()` calls `modal app logs` and tracks a cursor for stall detection.
//!   3. `cancel()` calls `modal app stop`.
//!   4. Completion is signaled by a callback POST to Thala (not polling).
//!
//! Runtime state that must be persisted (stored in TaskRun):
//!   - job_handle.job_id: Modal app/call ID (e.g. "ap-abc123" or "fc-xyz789")
//!   - remote_branch: the branch pushed to origin before spawning
//!   - callback_token_hash: SHA-256 of the per-run bearer token
//!
//! Required environment variables:
//!   - `MODAL_APP_FILE` — path to the Modal worker Python file
//!     (default: dev/infra/modal_worker.py::run_worker)
//!
//! Optional environment variables:
//!   - `MODAL_ENVIRONMENT` — Modal environment/workspace to target

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::timeout;

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, WorkerHandle};
use crate::ports::execution::{ExecutionBackend, LaunchRequest, LaunchedRun};

// ── ModalConfig ───────────────────────────────────────────────────────────────

const DEFAULT_APP_FILE: &str = "dev/infra/modal_worker.py::run_worker";

#[derive(Debug, Clone)]
pub struct ModalConfig {
    /// Path to the Modal worker Python file and function, e.g.
    /// `"dev/infra/modal_worker.py::run_worker"`.
    /// Overridden by `MODAL_APP_FILE` env var.
    pub app_file: String,

    /// Modal environment/workspace to run in.
    /// Overridden by `MODAL_ENVIRONMENT` env var.
    pub environment: Option<String>,
}

impl Default for ModalConfig {
    fn default() -> Self {
        Self {
            app_file: DEFAULT_APP_FILE.into(),
            environment: None,
        }
    }
}

impl ModalConfig {
    pub fn from_env() -> Self {
        Self {
            app_file: std::env::var("MODAL_APP_FILE").unwrap_or_else(|_| DEFAULT_APP_FILE.into()),
            environment: std::env::var("MODAL_ENVIRONMENT")
                .ok()
                .filter(|v| !v.is_empty()),
        }
    }
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

        // Build the `modal run --detach` command.
        // --detach returns immediately with the app/call ID instead of streaming logs.
        // Env vars carry all task context; the worker reads them at startup.
        let mut cmd = tokio::process::Command::new("modal");
        cmd.args(["run", "--detach", &self.config.app_file]);

        // Pass config as environment variables — names must match modal_worker.py exactly.
        cmd.env("THALA_RUN_ID", &req.run_id)
            .env("THALA_TASK_ID", &req.task_id)
            .env("THALA_PROMPT_B64", base64_encode(&req.prompt))
            .env("THALA_MODEL", &req.model)
            .env("THALA_TASK_BRANCH", remote_branch)
            .env("THALA_CALLBACK_URL", callback_url)
            .env("THALA_GITHUB_REPO", github_repo)
            .env("GITHUB_TOKEN", github_token);

        if let Some(token) = &req.callback_token {
            cmd.env("THALA_RUN_TOKEN", token);
        }

        // Forward lifecycle hooks so the worker can replicate Thala's local behaviour.
        if let Some(h) = &req.after_create_hook {
            cmd.env("THALA_AFTER_CREATE_HOOK", h);
        }
        if let Some(h) = &req.before_run_hook {
            cmd.env("THALA_BEFORE_RUN_HOOK", h);
        }
        if let Some(h) = &req.after_run_hook {
            cmd.env("THALA_AFTER_RUN_HOOK", h);
        }

        if let Some(env) = &self.config.environment {
            cmd.env("MODAL_ENVIRONMENT", env);
        }

        // Spawn the process with piped stdout so we can read lines as they arrive.
        // We must NOT call .output() — that blocks until the process exits, and
        // `modal run --detach` in Modal 1.x streams logs until the remote function
        // finishes (which for a coding task can be hours).
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ThalaError::backend("modal", format!("failed to spawn modal: {e}")))?;

        // Read stdout/stderr line by line until the detached launch exits or
        // times out. Modal emits progress on stderr in some CLI versions.
        // The app ID can appear before the remote function is scheduled, so
        // keep the local CLI alive through the launch window.
        let stdout_handle = child.stdout.take().expect("piped stdout");
        let stderr_handle = child.stderr.take().expect("piped stderr");
        let mut stdout_lines = BufReader::new(stdout_handle).lines();
        let mut stderr_lines = BufReader::new(stderr_handle).lines();

        let mut launch_output = String::new();
        let job_id = tokio::time::timeout(Duration::from_secs(90), async {
            let mut job_id: Option<String> = None;
            let mut stdout_open = true;
            let mut stderr_open = true;

            while stdout_open || stderr_open {
                tokio::select! {
                    line = stdout_lines.next_line(), if stdout_open => {
                        match line {
                            Ok(Some(line)) => {
                                tracing::debug!(task_id = %req.task_id, "modal stdout: {line}");
                                launch_output.push_str(&line);
                                launch_output.push('\n');
                                if job_id.is_none() {
                                    job_id = parse_modal_job_id(&line);
                                }
                            }
                            Ok(None) => stdout_open = false,
                            Err(e) => {
                                tracing::debug!(task_id = %req.task_id, "modal stdout read failed: {e}");
                                stdout_open = false;
                            }
                        }
                    }
                    line = stderr_lines.next_line(), if stderr_open => {
                        match line {
                            Ok(Some(line)) => {
                                tracing::debug!(task_id = %req.task_id, "modal stderr: {line}");
                                launch_output.push_str(&line);
                                launch_output.push('\n');
                                if job_id.is_none() {
                                    job_id = parse_modal_job_id(&line);
                                }
                            }
                            Ok(None) => stderr_open = false,
                            Err(e) => {
                                tracing::debug!(task_id = %req.task_id, "modal stderr read failed: {e}");
                                stderr_open = false;
                            }
                        }
                    }
                }
            }

            job_id
        })
        .await;

        let job_id = match job_id {
            Ok(Some(job_id)) => job_id,
            Ok(None) => {
                let status = child.wait().await.ok();
                return Err(ThalaError::backend(
                    "modal",
                    format!(
                        "Modal CLI did not print an app or call ID{}{}",
                        status
                            .map(|s| format!(" before exiting with status {s}"))
                            .unwrap_or_default(),
                        if launch_output.trim().is_empty() {
                            String::new()
                        } else {
                            format!("; output: {}", launch_output.trim())
                        }
                    ),
                ));
            }
            Err(_) => {
                if let Some(job_id) = parse_modal_job_id(&launch_output) {
                    // Modal 1.x may keep streaming logs after the detached app is
                    // scheduled. Once the launch window has elapsed and we have
                    // an id, stop the local CLI; the detached app continues.
                    child.kill().await.ok();
                    child.wait().await.ok();
                    job_id
                } else {
                    child.kill().await.ok();
                    child.wait().await.ok();
                    return Err(ThalaError::backend(
                        "modal",
                        "timed out waiting for Modal app ID",
                    ));
                }
            }
        };

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

    async fn observe(
        &self,
        handle: &WorkerHandle,
        _prev_cursor: Option<&str>,
    ) -> Result<RunObservation, ThalaError> {
        // Fetch the last 20 lines of logs from the running Modal app/function call.
        // The log cursor is the SHA-256 of the output, changing only when new output arrives.
        // This is used purely for stall detection; completion is signaled via callback.
        let mut cmd = tokio::process::Command::new("modal");
        cmd.args(["app", "logs", &handle.job_id, "--tail", "20"]);

        if let Some(env) = &self.config.environment {
            cmd.env("MODAL_ENVIRONMENT", env);
        }

        match timeout(Duration::from_secs(15), cmd.output()).await {
            Err(_) => {
                tracing::warn!(job_id = %handle.job_id, "`modal app logs` timed out");
                Ok(RunObservation {
                    cursor: handle.job_id.clone(),
                    is_alive: true,
                    terminal_status: None,
                    observed_at: chrono::Utc::now(),
                })
            }
            Ok(Ok(output)) if output.status.success() => {
                let log_text = String::from_utf8_lossy(&output.stdout).to_string();
                let cursor = hash_string(&log_text);

                tracing::debug!(job_id = %handle.job_id, "Modal: observed log output");

                Ok(RunObservation {
                    cursor,
                    is_alive: true,
                    terminal_status: None,
                    observed_at: chrono::Utc::now(),
                })
            }
            Ok(Ok(output)) => {
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
                    terminal_status: None,
                    observed_at: chrono::Utc::now(),
                })
            }
            Ok(Err(e)) => {
                // If the `modal` CLI is unavailable, return alive=true so we don't
                // accidentally crash a live worker. The stall timeout will eventually
                // fire if there is genuinely no progress.
                tracing::warn!(job_id = %handle.job_id, "Failed to run `modal app logs`: {e}");
                Ok(RunObservation {
                    cursor: handle.job_id.clone(),
                    is_alive: true,
                    terminal_status: None,
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

        match timeout(Duration::from_secs(15), cmd.output()).await {
            Err(_) => {
                tracing::warn!(job_id = %handle.job_id, "`modal app stop` timed out");
            }
            Ok(Ok(output)) if output.status.success() => {
                tracing::info!(job_id = %handle.job_id, "Modal app stopped successfully");
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Treat stop errors as warnings — the app may already be stopped.
                tracing::warn!(
                    job_id = %handle.job_id,
                    "modal app stop returned non-zero: {}",
                    stderr.trim()
                );
            }
            Ok(Err(e)) => {
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

/// Parse the Modal app/function-call ID from `modal run` output.
///
/// Modal CLI prints a line containing the app ID (prefixed "ap-") or a
/// function-call ID (prefixed "fc-") in its output. We look for either prefix.
fn parse_modal_job_id(stdout: &str) -> Option<String> {
    // Modal CLI output examples:
    //   "✓ Created app: ap-abc123def456"
    //   "Running app: fc-xyz789uvw012"
    // Search inside URLs and progress text, not only whitespace-delimited tokens.
    for token in stdout.split(|c: char| !c.is_alphanumeric() && c != '-') {
        if token.starts_with("ap-") || token.starts_with("fc-") {
            return Some(token.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_modal_job_id;

    #[test]
    fn parses_modal_id_from_plain_output() {
        assert_eq!(
            parse_modal_job_id("Created app: ap-abc123"),
            Some("ap-abc123".into())
        );
    }

    #[test]
    fn parses_modal_id_from_url_output() {
        assert_eq!(
            parse_modal_job_id("View run at https://modal.com/apps/ap-abc123/main"),
            Some("ap-abc123".into())
        );
    }

    #[test]
    fn returns_none_when_modal_id_is_missing() {
        assert_eq!(parse_modal_job_id("Running detached."), None);
    }
}
