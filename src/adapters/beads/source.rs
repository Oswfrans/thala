//! BeadsTaskSource — read ready tasks from Beads via `bd` CLI.
//!
//! Runs `bd ready --json` in the configured workspace root and translates
//! the JSON output into TaskSpec values.
//!
//! Invariant: this adapter is read-only. It never writes to Beads.
//! All writes go through BeadsTaskSink.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::ids::TaskId;
use crate::core::task::TaskSpec;
use crate::ports::task_source::TaskSource;

// ── BeadsTaskSource ───────────────────────────────────────────────────────────

pub struct BeadsTaskSource {
    /// Path to the repository containing the `.beads/` directory.
    pub workspace_root: PathBuf,

    /// Beads status value that maps to "ready for dispatch". Default: "open".
    pub ready_status: String,
}

impl BeadsTaskSource {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            ready_status: "open".into(),
        }
    }

    #[must_use]
    pub fn with_ready_status(mut self, status: impl Into<String>) -> Self {
        self.ready_status = status.into();
        self
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

    fn parse_spec(issue: &serde_json::Value) -> Option<TaskSpec> {
        let id = issue["id"].as_str()?;

        let title = issue["title"].as_str().unwrap_or("").to_string();

        let acceptance_criteria = issue["acceptanceCriteria"]
            .as_str()
            .or_else(|| issue["acceptance_criteria"].as_str())
            .unwrap_or("")
            .to_string();

        let context = issue["description"].as_str().unwrap_or("").to_string();

        let metadata = &issue["metadata"];
        let always_human_review = metadata["always_human_review"].as_bool().unwrap_or(false);
        let model_override = metadata["model_override"].as_str().map(ToString::to_string);

        let labels: Vec<String> = issue["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();

        Some(TaskSpec {
            id: TaskId::new(id),
            title,
            acceptance_criteria,
            context,
            beads_ref: id.to_string(),
            model_override,
            always_human_review,
            labels,
        })
    }
}

#[async_trait]
impl TaskSource for BeadsTaskSource {
    async fn fetch_ready(&self) -> Result<Vec<TaskSpec>, ThalaError> {
        let stdout = self.run_bd(&["ready", "--json"]).await?;

        let issues: Vec<serde_json::Value> = serde_json::from_str(stdout.trim())
            .map_err(|e| ThalaError::beads(format!("Failed to parse `bd ready` output: {e}")))?;

        let mut specs = Vec::new();

        for issue in &issues {
            let status = issue["status"].as_str().unwrap_or("open");
            if status != self.ready_status {
                continue;
            }

            if let Some(spec) = Self::parse_spec(issue) {
                if spec.is_dispatchable() {
                    specs.push(spec);
                } else {
                    tracing::debug!(
                        task_id = %spec.id,
                        "Skipping Beads task: no acceptance criteria"
                    );
                }
            }
        }

        Ok(specs)
    }

    async fn fetch_by_id(&self, task_id: &str) -> Result<Option<TaskSpec>, ThalaError> {
        let stdout = self.run_bd(&["show", task_id, "--json"]).await?;

        let issue: serde_json::Value = serde_json::from_str(stdout.trim())
            .map_err(|e| ThalaError::beads(format!("Failed to parse `bd show` output: {e}")))?;

        Ok(Self::parse_spec(&issue))
    }
}
