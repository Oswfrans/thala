//! Task-level domain types.
//!
//! Invariant: TaskSpec fields reflect what Beads knows. Thala must not become
//! the source of truth for these fields — it only reads them.
//!
//! TaskRecord is Thala's local view of a task. It tracks which run is active
//! and what Thala-managed status the task is in, but defers canonical state
//! questions back to Beads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::ids::{RunId, TaskId};
use crate::core::run::ExecutionBackendKind;

// ── TaskSpec ──────────────────────────────────────────────────────────────────

/// The canonical task description as read from Beads.
/// Treated as immutable after ingestion — Thala does not write back to these fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Stable Beads task ID (e.g. "bd-a1b2c3"). Never changes.
    pub id: TaskId,

    /// Human-readable title from Beads.
    pub title: String,

    /// Acceptance criteria that define what "done" looks like.
    /// Required — tasks without acceptance criteria are skipped by the scheduler.
    pub acceptance_criteria: String,

    /// Additional context provided by the task author (description, links, notes).
    pub context: String,

    /// Beads-internal reference used for write-back.
    /// For Beads this is the same as `id`; preserved for forward-compat.
    pub beads_ref: String,

    /// Optional model override — overrides the workflow default for this task.
    pub model_override: Option<String>,

    /// If true, a human must explicitly approve before the PR is merged.
    /// Set by the task author in Beads; read-only from Thala's perspective.
    pub always_human_review: bool,

    /// Labels / tags from Beads (used for routing decisions).
    pub labels: Vec<String>,
}

impl TaskSpec {
    /// A task is dispatchable only if it has acceptance criteria.
    /// Tasks without AC are skipped by the scheduler.
    pub fn is_dispatchable(&self) -> bool {
        !self.acceptance_criteria.trim().is_empty()
    }
}

// ── TaskRecord ────────────────────────────────────────────────────────────────

/// Thala's runtime record for a task it has taken responsibility for.
///
/// This is NOT a shadow of Beads state — it only contains Thala-specific
/// tracking fields. When Thala needs to know the canonical task description,
/// it reads `spec` which was populated from Beads at ingest time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// The canonical task spec as read from Beads.
    pub spec: TaskSpec,

    /// Monotonically increasing dispatch attempt counter.
    /// Incremented each time a new run is launched for this task.
    pub attempt: u32,

    /// Thala's view of the task lifecycle status.
    pub status: TaskStatus,

    /// ID of the most recent or currently active run. None until first dispatch.
    pub active_run_id: Option<RunId>,

    /// When Thala first ingested this task from Beads.
    pub ingested_at: DateTime<Utc>,

    /// When this record was last modified by the orchestrator.
    pub updated_at: DateTime<Utc>,

    /// Backend hint set by a human Reroute action. When set, the dispatcher
    /// will use this backend for the next attempt instead of the workflow default.
    /// Cleared after each successful dispatch.
    #[serde(default)]
    pub reroute_hint: Option<ExecutionBackendKind>,
}

impl TaskRecord {
    pub fn new(spec: TaskSpec) -> Self {
        let now = Utc::now();
        Self {
            spec,
            attempt: 0,
            status: TaskStatus::Pending,
            active_run_id: None,
            ingested_at: now,
            updated_at: now,
            reroute_hint: None,
        }
    }

    pub fn id(&self) -> &TaskId {
        &self.spec.id
    }

    /// Touch updated_at. Call after any mutation.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

// ── TaskStatus ────────────────────────────────────────────────────────────────

/// Task-level lifecycle status managed by Thala.
///
/// This is Thala's operational view. Beads has its own separate status
/// ("open", "in_progress", "closed") which Thala writes back via TaskSink.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Ingested from Beads; not yet assessed for dispatch readiness.
    Pending,

    /// Beads confirms this task is ready. Waiting for an execution slot.
    Ready,

    /// Context is being assembled and the run is being prepared.
    Dispatching,

    /// A run is active on an execution backend.
    Running,

    /// Thala is waiting for a human decision (approval, context, retry).
    WaitingForHuman,

    /// Run completed; validation is in progress (CI checks, review AI, human).
    Validating,

    /// All validation passed and the PR was merged. Written back to Beads.
    Succeeded,

    /// The task failed in a non-recoverable way (max retries exceeded, hard error).
    Failed,

    /// Stalled past the timeout; requires human intervention to continue.
    Stuck,

    /// A human explicitly closed or archived this task.
    Resolved,
}

impl TaskStatus {
    /// Terminal statuses cannot be automatically transitioned out of without
    /// an explicit recovery policy or human action.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Resolved)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Ready => "Ready",
            Self::Dispatching => "Dispatching",
            Self::Running => "Running",
            Self::WaitingForHuman => "WaitingForHuman",
            Self::Validating => "Validating",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Stuck => "Stuck",
            Self::Resolved => "Resolved",
        }
    }
}
