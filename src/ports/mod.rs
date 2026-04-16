//! Thala's port definitions — traits only, no implementations.
//!
//! Each port is a trait defining one boundary. Adapters implement these traits.
//! The orchestrator depends only on these traits, never on adapter types directly.

pub mod backend_router;
pub mod execution;
pub mod interaction;
pub mod repo;
pub mod state_store;
pub mod task_sink;
pub mod task_source;
pub mod validator;

// Convenience re-exports.
pub use backend_router::BackendRouter;
pub use execution::{ExecutionBackend, LaunchRequest, LaunchedRun};
pub use interaction::InteractionLayer;
pub use repo::{CiStatus, RepoProvider};
pub use state_store::StateStore;
pub use task_sink::{NewTaskRequest, TaskSink};
pub use task_source::TaskSource;
pub use validator::Validator;
