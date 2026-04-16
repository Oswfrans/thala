//! TaskSink port — write task state back to Beads.
//!
//! Thala calls this after key lifecycle events so Beads stays authoritative.
//! Slack/Discord intake also routes through this port to create tasks.
//!
//! Invariant: only Beads fields are written here.
//! Thala-internal run state lives only in the StateStore.

use async_trait::async_trait;

use crate::core::error::ThalaError;

/// Write-path interface to Beads.
///
/// Implementations: BeadsTaskSink (adapters/beads/sink.rs).
#[async_trait]
pub trait TaskSink: Send + Sync {
    /// Create a new task in Beads from an intake source (Slack/Discord).
    ///
    /// Returns the Beads-assigned task ID.
    async fn create_task(&self, spec: NewTaskRequest) -> Result<String, ThalaError>;

    /// Append additional context to an existing Beads task.
    /// Used by intake adapters when a user adds context to an existing task.
    async fn append_context(&self, task_id: &str, context: &str) -> Result<(), ThalaError>;

    /// Mark a task as in-progress in Beads.
    /// Called by the dispatcher when a run is launched.
    async fn mark_in_progress(&self, task_id: &str) -> Result<(), ThalaError>;

    /// Mark a task as succeeded in Beads and record the PR.
    /// Called by the reconciler after validation passes.
    async fn mark_done(&self, task_id: &str, pr_number: u32) -> Result<(), ThalaError>;

    /// Mark a task as stuck/blocked in Beads.
    /// Called when the task enters the Stuck or Failed state.
    async fn mark_stuck(&self, task_id: &str, reason: &str) -> Result<(), ThalaError>;

    /// Re-open a task in Beads (used when recovery is requested).
    async fn reopen(&self, task_id: &str) -> Result<(), ThalaError>;
}

// ── NewTaskRequest ────────────────────────────────────────────────────────────

/// Request to create a new task in Beads, typically from an intake adapter.
#[derive(Debug, Clone)]
pub struct NewTaskRequest {
    pub title: String,
    pub acceptance_criteria: String,
    pub context: String,

    /// Priority label (e.g. "P0", "P1", "P2").
    pub priority: Option<String>,

    /// Labels to attach (e.g. product name, team).
    pub labels: Vec<String>,

    /// Which channel and user submitted this request.
    /// Format: "slack:C12345:U12345" or "discord:guild_id:user_id".
    pub submitted_by: String,

    /// Whether a human must approve before merge.
    pub always_human_review: bool,
}

// ── Example: creating a task from Slack ──────────────────────────────────────
//
// ```rust
// let req = NewTaskRequest {
//     title: "Fix login button alignment".into(),
//     acceptance_criteria: "Button is centred on mobile breakpoint".into(),
//     context: "Reported by @jane in #bugs".into(),
//     priority: Some("P1".into()),
//     labels: vec!["frontend".into()],
//     submitted_by: "slack:C012345:U678901".into(),
//     always_human_review: false,
// };
// let task_id = sink.create_task(req).await?;
// ```
