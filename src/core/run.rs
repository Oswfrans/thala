//! Run-level domain types.
//!
//! A TaskRun represents one execution attempt of a task on one backend.
//! Beads does not see TaskRun — it lives only in Thala's StateStore.
//!
//! When a retry happens, a NEW TaskRun is created with an incremented attempt
//! number. The old run is kept as a historical record. Runs are never mutated
//! into a different backend attempt.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::ids::{RunId, TaskId};

// ── ExecutionBackendKind ──────────────────────────────────────────────────────

/// Which execution backend is responsible for this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionBackendKind {
    /// Local tmux session + git worktree on the Thala host.
    Local,
    /// Serverless container on Modal (via modal CLI).
    Modal,
    /// Cloudflare Worker/Durable Object control plane backed by Sandbox containers.
    Cloudflare,
    /// Managed worker session on OpenCode Zen (opencode.ai).
    #[serde(rename = "opencode-zen")]
    OpenCodeZen,
}

impl ExecutionBackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Modal => "modal",
            Self::Cloudflare => "cloudflare",
            Self::OpenCodeZen => "opencode-zen",
        }
    }

    /// Whether this backend creates a local git worktree on the Thala host.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

// ── WorkerHandle ──────────────────────────────────────────────────────────────

/// Opaque backend-specific job handle returned after a successful spawn.
/// Stored in the TaskRun; passed back to the backend for polling and cancellation.
///
/// - Local: tmux session name (e.g. "thala-example-app-bd-a1b2")
/// - Modal: function call ID (e.g. "fc-abc123def456")
/// - Cloudflare: control-plane remote run ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHandle {
    pub job_id: String,
    pub backend: ExecutionBackendKind,
}

// ── RunObservation ────────────────────────────────────────────────────────────

/// Activity snapshot used by the monitor for stall detection.
/// The monitor compares `cursor` values between ticks; if the cursor changes,
/// the worker is making progress.
#[derive(Debug, Clone)]
pub struct RunObservation {
    /// Opaque string that changes whenever worker output changes.
    /// Local: hash of captured tmux output.
    /// Remote: log cursor or etag from the backend's log API.
    pub cursor: String,

    /// Whether the job/container/session is still alive according to the backend.
    pub is_alive: bool,

    /// Terminal status reported by a polling backend.
    ///
    /// Callback and signal-file backends leave this as None. Polling-first
    /// remote backends use it to report completion without relying on a
    /// callback path.
    pub terminal_status: Option<RunStatus>,

    /// When this observation was taken.
    pub observed_at: DateTime<Utc>,
}

// ── RunStatus ─────────────────────────────────────────────────────────────────

/// Run-level lifecycle status.
///
/// Separate from TaskStatus. A task may have multiple runs across its lifetime;
/// each run goes through this independent lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    /// The backend is being prepared; the worker has not yet started.
    Launching,

    /// The worker is executing and producing output.
    Active,

    /// The worker signaled successful completion (exit 0 or callback).
    Completed,

    /// Terminated by the orchestrator before natural completion.
    Cancelled,

    /// The worker exited with an error or reported failure via callback.
    Failed,

    /// No output progress was detected within the stall timeout window.
    TimedOut,
}

impl RunStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed | Self::TimedOut
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Launching => "Launching",
            Self::Active => "Active",
            Self::Completed => "Completed",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
            Self::TimedOut => "TimedOut",
        }
    }
}

// ── TaskRun ───────────────────────────────────────────────────────────────────

/// One execution attempt of a task.
///
/// Invariants:
/// - Created by the dispatcher with status Launching.
/// - `handle` is set once the backend confirms the worker is spawned.
/// - `completed_at` is set on any terminal transition.
/// - Retries create a NEW TaskRun, never mutating this one's backend or handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    // ── Identity ──────────────────────────────────────────────────────────────
    pub run_id: RunId,
    pub task_id: TaskId,
    /// Copied from TaskRecord.attempt at dispatch time.
    pub attempt: u32,

    // ── Execution state ───────────────────────────────────────────────────────
    pub status: RunStatus,
    pub backend: ExecutionBackendKind,
    /// Set once the backend has successfully spawned the worker.
    pub handle: Option<WorkerHandle>,
    /// Absolute path to the local git worktree (Local backend only).
    pub worktree_path: Option<String>,
    /// Branch name pushed to origin (remote backends only).
    pub remote_branch: Option<String>,
    /// SHA-256 of the per-run callback bearer token (raw token is sent to worker only).
    pub callback_token_hash: Option<String>,

    // ── Timing & stall detection ──────────────────────────────────────────────
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// When output last changed; drives stall detection in the monitor.
    pub last_activity_at: Option<DateTime<Utc>>,
    /// Opaque cursor from the last backend poll; changes when output changes.
    pub last_observation_cursor: Option<String>,

    // ── Validation state (populated after execution completes) ────────────────
    /// PR number created during validation.
    pub pr_number: Option<u32>,
    /// Full PR URL (e.g. "https://github.com/org/repo/pull/42").
    pub pr_url: Option<String>,
    /// Feedback from a failed review AI pass; injected into the re-run prompt.
    pub review_feedback: Option<String>,
    /// Number of review-feedback cycles for THIS run. Resets to 0 on each new
    /// TaskRun (i.e. each retry attempt), so max_review_cycles is per-attempt.
    pub review_cycle: u32,
}

impl TaskRun {
    pub fn new(
        run_id: RunId,
        task_id: TaskId,
        attempt: u32,
        backend: ExecutionBackendKind,
    ) -> Self {
        let now = Utc::now();
        Self {
            run_id,
            task_id,
            attempt,
            status: RunStatus::Launching,
            backend,
            handle: None,
            worktree_path: None,
            remote_branch: None,
            callback_token_hash: None,
            started_at: now,
            updated_at: now,
            completed_at: None,
            last_activity_at: None,
            last_observation_cursor: None,
            pr_number: None,
            pr_url: None,
            review_feedback: None,
            review_cycle: 0,
        }
    }

    /// Touch updated_at. Call after any mutation.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Mark the run as having entered a terminal state.
    pub fn mark_completed(&mut self) {
        self.completed_at = Some(Utc::now());
        self.touch();
    }
}
