//! BackendRouter port — choose which execution backend handles a run.
//!
//! The default router reads from WorkflowConfig. A policy-aware router
//! could route based on task labels, retry count, cost, or backend availability.

use std::sync::Arc;

use crate::core::run::ExecutionBackendKind;
use crate::core::task::TaskSpec;
use crate::core::workflow::WorkflowConfig;
use crate::ports::execution::ExecutionBackend;

// ── BackendRouter ─────────────────────────────────────────────────────────────

/// Selects the appropriate backend for a given task and workflow config.
///
/// Implementations: DefaultBackendRouter (adapters/execution/router.rs).
pub trait BackendRouter: Send + Sync {
    /// Choose which backend kind should handle this task.
    ///
    /// Called by the dispatcher before building a LaunchRequest.
    /// The router does not need to know about retry counts; the dispatcher
    /// passes in the current attempt number via the workflow config.
    fn route(
        &self,
        spec: &TaskSpec,
        workflow: &WorkflowConfig,
        attempt: u32,
    ) -> ExecutionBackendKind;

    /// Retrieve the backend implementation for a given kind.
    ///
    /// Panics if the requested kind has no registered implementation.
    /// All three backends (Local, Modal, Cloudflare) must be registered
    /// before the router is used, even if only one is configured.
    fn backend(&self, kind: &ExecutionBackendKind) -> Arc<dyn ExecutionBackend>;

    /// Whether a reroute is allowed given the current attempt and policy.
    ///
    /// Used by the dispatcher when a retry is requested — returns the
    /// backend kind to use for the next attempt.
    fn reroute_backend(
        &self,
        spec: &TaskSpec,
        workflow: &WorkflowConfig,
        failed_backend: &ExecutionBackendKind,
        attempt: u32,
    ) -> Option<ExecutionBackendKind>;
}
