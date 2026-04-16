//! TaskSource port — read canonical tasks from Beads.
//!
//! Thala polls this on a configurable interval. The adapter implementation
//! calls `bd ready --json` and translates the output into TaskSpec values.
//!
//! Invariant: TaskSource is read-only. It never writes to Beads.

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::task::TaskSpec;

/// Read-only view of the Beads task queue.
///
/// Implementations: BeadsTaskSource (adapters/beads/source.rs).
#[async_trait]
pub trait TaskSource: Send + Sync {
    /// Fetch tasks that are ready for dispatch.
    ///
    /// Returns only tasks that:
    /// 1. Have acceptance criteria (non-empty).
    /// 2. Are unblocked (all dependencies resolved).
    /// 3. Have not already been dispatched in this Thala session.
    ///
    /// The deduplication against active runtime state is the scheduler's job —
    /// the source returns what Beads says is ready, without filtering.
    async fn fetch_ready(&self) -> Result<Vec<TaskSpec>, ThalaError>;

    /// Fetch a specific task by its Beads ID.
    /// Used by the reconciler to reload task specs after a restart.
    async fn fetch_by_id(&self, task_id: &str) -> Result<Option<TaskSpec>, ThalaError>;
}
