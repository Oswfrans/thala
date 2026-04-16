//! Thala's core domain — pure logic, no I/O.
//!
//! This module contains only domain types and transition logic.
//! No async, no networking, no database access.

pub mod error;
pub mod events;
pub mod ids;
pub mod interaction;
pub mod run;
pub mod state;
pub mod task;
pub mod transitions;
pub mod validation;
pub mod workflow;

// Convenience re-exports for common types.
pub use error::ThalaError;
pub use ids::{InteractionId, RunId, TaskId};
pub use run::{ExecutionBackendKind, RunObservation, RunStatus, TaskRun, WorkerHandle};
pub use state::StateError;
pub use task::{TaskRecord, TaskSpec, TaskStatus};
pub use transitions::{apply_run_transition, apply_transition, RunTransition, Transition};
pub use validation::ValidationOutcome;
