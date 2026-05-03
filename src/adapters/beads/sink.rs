//! BeadsTaskSink — write task state back to Beads via `bd` CLI.
//!
//! This is the only place in Thala that writes to Beads for task-level state.
//! Slack/Discord intake also routes through this adapter.
//!
//! All subprocess calls use argument arrays — never `sh -c` with interpolation.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::ports::task_sink::{NewTaskRequest, TaskSink};

// ── BeadsTaskSink ─────────────────────────────────────────────────────────────

pub struct BeadsTaskSink {
    pub workspace_root: PathBuf,
}

impl BeadsTaskSink {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    async fn run_bd(&self, args: &[&str]) -> Result<String, ThalaError> {
        let output = tokio::process::Command::new("bd")
            .args(args)
            .current_dir(&self.workspace_root)
            .output()
            .await
            .map_err(|e| ThalaError::beads(format!("Failed to run `bd`: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ThalaError::beads(format!(
                "`bd {}` failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[async_trait]
impl TaskSink for BeadsTaskSink {
    async fn create_task(&self, req: NewTaskRequest) -> Result<String, ThalaError> {
        // `bd create` outputs the new task ID on stdout (one line, no trailing whitespace).
        let mut args = vec![
            "create",
            "--title",
            &req.title,
            "--acceptance",
            &req.acceptance_criteria,
        ];

        // Append context if present.
        let context_owned;
        if !req.context.is_empty() {
            args.push("--description");
            context_owned = req.context.clone();
            args.push(&context_owned);
        }

        let stdout = self.run_bd(&args).await?;

        // `bd create` is expected to output the new task ID on stdout.
        let task_id = stdout.trim().to_string();
        if task_id.is_empty() {
            return Err(ThalaError::beads("bd create returned no task ID"));
        }

        // Apply metadata for fields not covered by core bd create args.
        if req.always_human_review {
            let meta = r#"{"always_human_review": true}"#;
            let _ = self.run_bd(&["update", &task_id, "--metadata", meta]).await;
        }

        Ok(task_id)
    }

    async fn append_context(&self, task_id: &str, context: &str) -> Result<(), ThalaError> {
        // `bd comment` appends to the task without replacing the existing description.
        self.run_bd(&["comment", task_id, "--message", context])
            .await?;
        Ok(())
    }

    async fn mark_in_progress(&self, task_id: &str) -> Result<(), ThalaError> {
        self.run_bd(&["update", task_id, "--status", "in_progress"])
            .await?;
        Ok(())
    }

    async fn mark_done(&self, task_id: &str, pr_number: u32) -> Result<(), ThalaError> {
        self.run_bd(&["close", task_id, "--reason", "PR merged by Thala"])
            .await?;

        let meta = format!(r#"{{"pr_number": {pr_number}}}"#);
        let _ = self.run_bd(&["update", task_id, "--metadata", &meta]).await;

        Ok(())
    }

    async fn mark_stuck(&self, task_id: &str, reason: &str) -> Result<(), ThalaError> {
        self.run_bd(&["update", task_id, "--status", "blocked"])
            .await?;

        let meta = serde_json::json!({"stuck_reason": reason}).to_string();
        let _ = self.run_bd(&["update", task_id, "--metadata", &meta]).await;

        Ok(())
    }

    async fn reopen(&self, task_id: &str) -> Result<(), ThalaError> {
        self.run_bd(&["update", task_id, "--status", "open"])
            .await?;
        Ok(())
    }
}
