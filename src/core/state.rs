//! State transition error type.
//!
//! Kept separate from ThalaError so transition logic can return a focused type
//! that callers can match on without pattern-matching the full error hierarchy.

use crate::core::ids::{RunId, TaskId};
use crate::core::run::RunStatus;
use crate::core::task::TaskStatus;
use thiserror::Error;

/// Errors that arise when a state transition is attempted but the transition
/// is illegal given the current state.
#[derive(Debug, Error)]
pub enum StateError {
    #[error("Illegal task transition for {task_id} from {from:?}: {reason}")]
    IllegalTaskTransition {
        task_id: TaskId,
        from: TaskStatus,
        reason: String,
    },

    #[error("Illegal run transition for {run_id} from {from:?}: {reason}")]
    IllegalRunTransition {
        run_id: RunId,
        from: RunStatus,
        reason: String,
    },

    #[error("Task {0} is in a terminal state and cannot be transitioned")]
    TaskTerminal(TaskId),

    #[error("Run {0} is in a terminal state and cannot be transitioned")]
    RunTerminal(RunId),
}
