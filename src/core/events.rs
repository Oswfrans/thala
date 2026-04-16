//! Domain events emitted by the orchestrator.
//!
//! Events are the communication medium between orchestrator subsystems.
//! They carry the minimum payload needed for the consumer to act —
//! no full records, just IDs and discriminating facts.
//!
//! Events are not stored durably (the StateStore holds the authoritative state).
//! They are passed in-process via channels.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::ids::{InteractionId, RunId, TaskId};
use crate::core::run::{ExecutionBackendKind, RunStatus};
use crate::core::task::TaskStatus;
use crate::core::validation::ValidationOutcome;

// ── OrchestratorEvent ─────────────────────────────────────────────────────────

/// All events emitted within the orchestration kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrchestratorEvent {
    // ── Scheduler events ──────────────────────────────────────────────────────
    /// The scheduler identified a task that is ready to dispatch.
    DispatchReady { task_id: TaskId, at: DateTime<Utc> },

    // ── Dispatcher events ─────────────────────────────────────────────────────
    /// A run was successfully launched on a backend.
    RunLaunched {
        task_id: TaskId,
        run_id: RunId,
        backend: ExecutionBackendKind,
        at: DateTime<Utc>,
    },

    /// A run failed to launch (spawn error, preflight failure).
    RunLaunchFailed {
        task_id: TaskId,
        reason: String,
        at: DateTime<Utc>,
    },

    // ── Monitor events ────────────────────────────────────────────────────────
    /// The monitor observed new activity from a running worker.
    RunActivityObserved { run_id: RunId, at: DateTime<Utc> },

    /// The worker signaled completion (exit 0 or successful callback).
    RunCompleted {
        task_id: TaskId,
        run_id: RunId,
        at: DateTime<Utc>,
    },

    /// The worker reported failure (non-zero exit or failure callback).
    RunFailed {
        task_id: TaskId,
        run_id: RunId,
        reason: String,
        at: DateTime<Utc>,
    },

    /// The run exceeded the stall timeout with no output change.
    RunTimedOut {
        task_id: TaskId,
        run_id: RunId,
        at: DateTime<Utc>,
    },

    /// The run was cancelled by the orchestrator.
    RunCancelled { run_id: RunId, at: DateTime<Utc> },

    /// The run transitioned to a new RunStatus.
    RunStatusChanged {
        run_id: RunId,
        from: RunStatus,
        to: RunStatus,
        at: DateTime<Utc>,
    },

    // ── Validation events ─────────────────────────────────────────────────────
    /// A validator produced an outcome.
    ValidationResult {
        task_id: TaskId,
        run_id: RunId,
        outcome: ValidationOutcome,
        at: DateTime<Utc>,
    },

    // ── Interaction events ────────────────────────────────────────────────────
    /// The orchestrator issued a request for human input.
    InteractionRequested {
        task_id: TaskId,
        run_id: RunId,
        interaction_id: InteractionId,
        at: DateTime<Utc>,
    },

    /// A human responded to an interaction request.
    InteractionResolved {
        task_id: TaskId,
        run_id: RunId,
        interaction_id: InteractionId,
        at: DateTime<Utc>,
    },

    // ── Task-level events ─────────────────────────────────────────────────────
    /// A task transitioned to a new TaskStatus.
    TaskStatusChanged {
        task_id: TaskId,
        from: TaskStatus,
        to: TaskStatus,
        at: DateTime<Utc>,
    },

    /// A task was written back to Beads.
    TaskSyncedToBeads {
        task_id: TaskId,
        beads_status: String,
        at: DateTime<Utc>,
    },
}

impl OrchestratorEvent {
    pub fn now() -> DateTime<Utc> {
        Utc::now()
    }

    pub fn dispatch_ready(task_id: TaskId) -> Self {
        Self::DispatchReady {
            task_id,
            at: Self::now(),
        }
    }

    pub fn run_launched(task_id: TaskId, run_id: RunId, backend: ExecutionBackendKind) -> Self {
        Self::RunLaunched {
            task_id,
            run_id,
            backend,
            at: Self::now(),
        }
    }

    pub fn run_completed(task_id: TaskId, run_id: RunId) -> Self {
        Self::RunCompleted {
            task_id,
            run_id,
            at: Self::now(),
        }
    }

    pub fn run_failed(task_id: TaskId, run_id: RunId, reason: impl Into<String>) -> Self {
        Self::RunFailed {
            task_id,
            run_id,
            reason: reason.into(),
            at: Self::now(),
        }
    }
}
