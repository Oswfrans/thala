//! LocalBackend — tmux sessions + git worktrees on the Thala host.
//!
//! How it works:
//!   1. `launch()` creates a git worktree, writes a prompt file, runs
//!      after_create and before_run hooks, then spawns an opencode session in a
//!      new tmux window.
//!   2. `observe()` runs `tmux capture-pane` and hashes the output for stall detection.
//!   3. `cancel()` kills the tmux session.
//!   4. `cleanup()` kills the session and removes the worktree.
//!
//! Runtime state that must be persisted (stored in TaskRun):
//!   - job_handle.job_id: tmux session name (e.g. "thala-example-app-bd-a1b2")
//!   - worktree_path: absolute path to the local worktree

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, WorkerHandle};
use crate::ports::execution::{ExecutionBackend, LaunchRequest, LaunchedRun};

// ── LocalBackend ──────────────────────────────────────────────────────────────

pub struct LocalBackend;

impl Default for LocalBackend {
    fn default() -> Self {
        Self
    }
}

impl LocalBackend {
    pub fn new() -> Self {
        Self
    }

    fn session_name(task_id: &str, product: &str) -> String {
        let slug = task_id.replace(['/', ':'], "-");
        format!("thala-{product}-{slug}")
    }

    fn worktree_path(workspace_root: &Path, task_id: &str) -> PathBuf {
        let slug = task_id.replace(['/', ':'], "-");
        workspace_root.join(format!(".thala-worktrees/{slug}"))
    }

    fn path_arg<'a>(path: &'a Path, label: &str) -> Result<&'a str, ThalaError> {
        path.to_str().ok_or_else(|| {
            ThalaError::backend(
                "local",
                format!("{label} path is not valid UTF-8: {}", path.display()),
            )
        })
    }

    async fn run_hook(
        hook_name: &str,
        hook: Option<&str>,
        worktree: &Path,
    ) -> Result<(), ThalaError> {
        let Some(hook) = hook.filter(|h| !h.trim().is_empty()) else {
            return Ok(());
        };

        let output = tokio::process::Command::new("sh")
            .args(["-lc", hook])
            .current_dir(worktree)
            .output()
            .await
            .map_err(|e| ThalaError::backend("local", format!("{hook_name} hook failed: {e}")))?;

        if !output.status.success() {
            return Err(ThalaError::backend(
                "local",
                format!(
                    "{hook_name} hook exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }

        Ok(())
    }
}

#[async_trait]
impl ExecutionBackend for LocalBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn is_local(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "local"
    }

    async fn launch(&self, req: LaunchRequest) -> Result<LaunchedRun, ThalaError> {
        let session = Self::session_name(&req.task_id, &req.product);
        let worktree = Self::worktree_path(&req.workspace_root, &req.task_id);
        let worktree_arg = Self::path_arg(&worktree, "worktree")?;

        // Create git worktree.
        let branch = format!("task/{}", req.task_id);
        let output = tokio::process::Command::new("git")
            .args(["worktree", "add", "-b", &branch, worktree_arg, "HEAD"])
            .current_dir(&req.workspace_root)
            .output()
            .await
            .map_err(|e| ThalaError::backend("local", format!("git worktree add failed: {e}")))?;

        if !output.status.success() {
            return Err(ThalaError::backend(
                "local",
                format!(
                    "git worktree add exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }

        tokio::fs::create_dir_all(worktree.join(".thala").join("signals"))
            .await
            .map_err(|e| {
                ThalaError::backend("local", format!("Failed to create signal dir: {e}"))
            })?;
        tokio::fs::create_dir_all(worktree.join(".thala").join("prompts"))
            .await
            .map_err(|e| {
                ThalaError::backend("local", format!("Failed to create prompt dir: {e}"))
            })?;

        // Write prompt file.
        let prompt_path = worktree
            .join(".thala")
            .join("prompts")
            .join(format!("{}.md", req.task_id.replace(['/', ':'], "-")));
        tokio::fs::write(&prompt_path, &req.prompt)
            .await
            .map_err(|e| ThalaError::backend("local", format!("Failed to write prompt: {e}")))?;

        Self::run_hook("after_create", req.after_create_hook.as_deref(), &worktree).await?;
        Self::run_hook("before_run", req.before_run_hook.as_deref(), &worktree).await?;

        // Spawn opencode in a new tmux session.
        let status = tokio::process::Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-c",
                worktree_arg,
                "opencode",
                "--model",
                &req.model,
                "--no-session",
                "-p",
                &req.prompt,
            ])
            .status()
            .await
            .map_err(|e| ThalaError::backend("local", format!("tmux new-session failed: {e}")))?;

        if !status.success() {
            return Err(ThalaError::backend(
                "local",
                format!("tmux new-session exited with {status}"),
            ));
        }

        tracing::info!(
            task_id = %req.task_id,
            session = %session,
            worktree = %worktree.display(),
            "Local worker spawned"
        );

        Ok(LaunchedRun {
            handle: WorkerHandle {
                job_id: session,
                backend: ExecutionBackendKind::Local,
            },
            worktree_path: Some(worktree),
            remote_branch: None,
        })
    }

    async fn observe(
        &self,
        handle: &WorkerHandle,
        _prev_cursor: Option<&str>,
    ) -> Result<RunObservation, ThalaError> {
        // Check if the tmux session exists.
        let session_check = tokio::process::Command::new("tmux")
            .args(["has-session", "-t", &handle.job_id])
            .status()
            .await
            .map_err(|e| ThalaError::backend("local", format!("tmux has-session failed: {e}")))?;

        let is_alive = session_check.success();

        if !is_alive {
            return Ok(RunObservation {
                cursor: "dead".into(),
                is_alive: false,
                terminal_status: None,
                observed_at: chrono::Utc::now(),
            });
        }

        // Capture recent pane output for a stable, bounded activity cursor.
        let output = tokio::process::Command::new("tmux")
            .args(["capture-pane", "-t", &handle.job_id, "-p", "-S", "-100"])
            .output()
            .await
            .map_err(|e| ThalaError::backend("local", format!("tmux capture-pane failed: {e}")))?;

        let text = String::from_utf8_lossy(&output.stdout).to_string();

        // Hash the output — cursor changes when content changes.
        let cursor = hex::encode(Sha256::digest(text.as_bytes()));

        Ok(RunObservation {
            cursor,
            is_alive: true,
            terminal_status: None,
            observed_at: chrono::Utc::now(),
        })
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), ThalaError> {
        let _ = tokio::process::Command::new("tmux")
            .args(["kill-session", "-t", &handle.job_id])
            .status()
            .await;
        Ok(())
    }

    async fn cleanup(
        &self,
        handle: &WorkerHandle,
        workspace_root: &Path,
        task_id: &str,
    ) -> Result<(), ThalaError> {
        // Kill session.
        self.cancel(handle).await?;

        // Remove worktree.
        let worktree = Self::worktree_path(workspace_root, task_id);
        if worktree.exists() {
            let worktree_arg = Self::path_arg(&worktree, "worktree")?;
            let _ = tokio::process::Command::new("git")
                .args(["worktree", "remove", "--force", worktree_arg])
                .current_dir(workspace_root)
                .status()
                .await;
        }

        tracing::info!(task_id, "Local backend cleanup complete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::LocalBackend;

    #[cfg(unix)]
    #[test]
    fn path_arg_rejects_non_utf8_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let path = PathBuf::from(OsString::from_vec(vec![0xff]));
        let err = LocalBackend::path_arg(&path, "worktree").unwrap_err();

        assert!(err.to_string().contains("worktree path is not valid UTF-8"));
    }
}
