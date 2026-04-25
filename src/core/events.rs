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
///
/// Variants annotated "observability-only" are emitted for tracing/logging
/// purposes but are not consumed by any routing logic in the engine.
/// Only the explicitly-routed variants trigger engine dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrchestratorEvent {
    // ── Routed events ─────────────────────────────────────────────────────────
    /// Routed → engine → dispatcher.dispatch.
    DispatchReady {
        task_id: TaskId,
        at: DateTime<Utc>,
    },

    /// Routed → engine → validator.handle_run_completed.
    RunCompleted {
        task_id: TaskId,
        run_id: RunId,
        at: DateTime<Utc>,
    },

    /// Routed → engine → creates stuck ticket + notifies channels.
    RunTimedOut {
        task_id: TaskId,
        run_id: RunId,
        at: DateTime<Utc>,
    },

    /// Routed → engine → dispatcher (retry) or validator (human-approved merge).
    InteractionResolved {
        task_id: TaskId,
        run_id: RunId,
        interaction_id: InteractionId,
        at: DateTime<Utc>,
    },

    /// Routed → engine (logged); validator drives its own transitions directly.
    ValidationResult {
        task_id: TaskId,
        run_id: RunId,
        outcome: ValidationOutcome,
        at: DateTime<Utc>,
    },

    // ── Observability-only events (emitted but not consumed by any subsystem) ─
    RunLaunched {
        task_id: TaskId,
        run_id: RunId,
        backend: ExecutionBackendKind,
        at: DateTime<Utc>,
    },
    RunLaunchFailed {
        task_id: TaskId,
        reason: String,
        at: DateTime<Utc>,
    },
    RunFailed {
        task_id: TaskId,
        run_id: RunId,
        reason: String,
        at: DateTime<Utc>,
    },
    RunActivityObserved {
        run_id: RunId,
        at: DateTime<Utc>,
    },
    RunCancelled {
        run_id: RunId,
        at: DateTime<Utc>,
    },
    RunStatusChanged {
        run_id: RunId,
        from: RunStatus,
        to: RunStatus,
        at: DateTime<Utc>,
    },
    InteractionRequested {
        task_id: TaskId,
        run_id: RunId,
        interaction_id: InteractionId,
        at: DateTime<Utc>,
    },
    TaskStatusChanged {
        task_id: TaskId,
        from: TaskStatus,
        to: TaskStatus,
        at: DateTime<Utc>,
    },
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
