//! DefaultBackendRouter — routes tasks to the configured execution backend.
//!
//! The default router reads the configured backend from WorkflowConfig.
//! All backends are registered at construction
//! time so routing decisions can be made at runtime without reconstruction.
//!
//! The router also handles reroute decisions for retries.

use std::sync::Arc;

use crate::core::run::ExecutionBackendKind;
use crate::core::task::TaskSpec;
use crate::core::workflow::WorkflowConfig;
use crate::ports::backend_router::BackendRouter;
use crate::ports::execution::ExecutionBackend;

// ── DefaultBackendRouter ──────────────────────────────────────────────────────

pub struct DefaultBackendRouter {
    local: Arc<dyn ExecutionBackend>,
    modal: Arc<dyn ExecutionBackend>,
    cloudflare: Arc<dyn ExecutionBackend>,
    kubernetes: Arc<dyn ExecutionBackend>,
}

impl DefaultBackendRouter {
    pub fn new(
        local: Arc<dyn ExecutionBackend>,
        modal: Arc<dyn ExecutionBackend>,
        cloudflare: Arc<dyn ExecutionBackend>,
        kubernetes: Arc<dyn ExecutionBackend>,
    ) -> Self {
        Self {
            local,
            modal,
            cloudflare,
            kubernetes,
        }
    }
}

impl BackendRouter for DefaultBackendRouter {
    fn route(
        &self,
        spec: &TaskSpec,
        workflow: &WorkflowConfig,
        _attempt: u32,
    ) -> ExecutionBackendKind {
        // Label-based routing: a label of the form "backend:<kind>" overrides
        // the workflow default, allowing per-task backend selection.
        //
        // Examples:
        //   labels: ["backend:modal"]      → routes to Modal
        //   labels: ["backend:cloudflare"] → routes to Cloudflare
        //   labels: ["backend:local"]      → routes to Local
        for label in &spec.labels {
            if let Some(backend_name) = label.strip_prefix("backend:") {
                let kind = match backend_name.to_lowercase().as_str() {
                    "modal" => ExecutionBackendKind::Modal,
                    "cloudflare" | "cf" => ExecutionBackendKind::Cloudflare,
                    "kubernetes" | "k8s" => ExecutionBackendKind::Kubernetes,
                    "local" => ExecutionBackendKind::Local,
                    other => {
                        tracing::warn!(
                            task_id = %spec.id.as_str(),
                            "Unknown backend label '{}' — falling back to workflow default",
                            other
                        );
                        continue;
                    }
                };
                tracing::debug!(
                    task_id = %spec.id.as_str(),
                    backend = %kind.as_str(),
                    "Routing via backend label"
                );
                return kind;
            }
        }

        // No label override — use the workflow default.
        workflow.execution.backend.clone()
    }

    fn backend(&self, kind: &ExecutionBackendKind) -> Arc<dyn ExecutionBackend> {
        match kind {
            ExecutionBackendKind::Local => self.local.clone(),
            ExecutionBackendKind::Modal => self.modal.clone(),
            ExecutionBackendKind::Cloudflare => self.cloudflare.clone(),
            ExecutionBackendKind::Kubernetes => self.kubernetes.clone(),
        }
    }

    fn reroute_backend(
        &self,
        _spec: &TaskSpec,
        workflow: &WorkflowConfig,
        failed_backend: &ExecutionBackendKind,
        attempt: u32,
    ) -> Option<ExecutionBackendKind> {
        if !workflow.retry.allow_backend_reroute {
            return None;
        }

        if attempt >= workflow.retry.max_attempts {
            return None;
        }

        // If a specific reroute target is configured, use that.
        if let Some(target) = &workflow.retry.reroute_to {
            if target != failed_backend {
                return Some(target.clone());
            }
        }

        // Default fallback: non-local → Local, Local → Modal.
        match failed_backend {
            ExecutionBackendKind::Local => Some(ExecutionBackendKind::Modal),
            ExecutionBackendKind::Modal
            | ExecutionBackendKind::Cloudflare
            | ExecutionBackendKind::Kubernetes => Some(ExecutionBackendKind::Local),
        }
    }
}
