//! OrchestratorEngine — the main wiring struct for Thala's orchestration kernel.
//!
//! The engine assembles all ports and subsystems, runs reconciliation at startup,
//! then launches the scheduler, dispatcher, monitor, validator coordinator,
//! and human loop as concurrent Tokio tasks.
//!
//! The engine does not contain any business logic — it delegates to the
//! purpose-specific subsystems. Its job is wiring and lifecycle management.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::core::events::OrchestratorEvent;
use crate::core::interaction::{
    InteractionAction, InteractionRequest, InteractionRequestKind, InteractionTicket,
};
use crate::core::task::TaskStatus;
use crate::core::workflow::WorkflowConfig;
use crate::orchestrator::dispatcher::{Dispatcher, DispatcherConfig};
use crate::orchestrator::human_loop::{HumanLoop, HumanLoopConfig};
use crate::orchestrator::monitor::{Monitor, MonitorConfig};
use crate::orchestrator::reconciler::Reconciler;
use crate::orchestrator::scheduler::{Scheduler, SchedulerConfig};
use crate::orchestrator::validator::ValidatorCoordinator;
use crate::ports::backend_router::BackendRouter;
use crate::ports::interaction::InteractionLayer;
use crate::ports::repo::RepoProvider;
use crate::ports::state_store::StateStore;
use crate::ports::task_sink::TaskSink;
use crate::ports::task_source::TaskSource;
use crate::ports::validator::Validator;

// ── EngineConfig ──────────────────────────────────────────────────────────────

/// Runtime configuration assembled from WORKFLOW.md and env vars.
pub struct EngineConfig {
    pub workflow: WorkflowConfig,
    pub scheduler: SchedulerConfig,
    pub monitor: MonitorConfig,
    pub human_loop: HumanLoopConfig,
    pub dispatcher: DispatcherConfig,
}

// ── OrchestratorEngine ────────────────────────────────────────────────────────

pub struct OrchestratorEngine {
    config: EngineConfig,
    // Ports
    source: Arc<dyn TaskSource>,
    sink: Arc<dyn TaskSink>,
    store: Arc<dyn StateStore>,
    router: Arc<dyn BackendRouter>,
    repo: Arc<dyn RepoProvider>,
    review_ai: Arc<dyn Validator>,
    interaction_layers: Vec<Arc<dyn InteractionLayer>>,
}

impl OrchestratorEngine {
    pub fn new(
        config: EngineConfig,
        source: Arc<dyn TaskSource>,
        sink: Arc<dyn TaskSink>,
        store: Arc<dyn StateStore>,
        router: Arc<dyn BackendRouter>,
        repo: Arc<dyn RepoProvider>,
        review_ai: Arc<dyn Validator>,
        interaction_layers: Vec<Arc<dyn InteractionLayer>>,
    ) -> Self {
        Self {
            config,
            source,
            sink,
            store,
            router,
            repo,
            review_ai,
            interaction_layers,
        }
    }

    /// Start the orchestration engine.
    ///
    /// 1. Runs reconciliation (once, synchronously before loops start).
    /// 2. Spawns all subsystem loops as concurrent Tokio tasks.
    /// 3. Returns a shutdown handle.
    ///
    /// This method runs indefinitely until the returned handle is dropped.
    pub async fn run(self) -> anyhow::Result<()> {
        tracing::info!(
            product = %self.config.workflow.product,
            "OrchestratorEngine starting"
        );

        // Event channel — subsystems communicate via events.
        let (events_tx, mut events_rx) = mpsc::channel::<OrchestratorEvent>(256);

        // Step 1: Reconcile state from previous run.
        let reconciler = Reconciler::new(
            self.store.clone(),
            self.source.clone(),
            self.router.clone(),
            events_tx.clone(),
        );
        let recovered = reconciler.reconcile().await?;
        tracing::info!(recovered, "Reconciliation complete");

        // Step 2: Start subsystem tasks.

        let scheduler = Scheduler::new(
            self.config.scheduler.clone(),
            self.source.clone(),
            self.store.clone(),
            events_tx.clone(),
        );

        let monitor = Monitor::new(
            self.config.monitor.clone(),
            self.config.workflow.clone(),
            self.store.clone(),
            self.router.clone(),
            events_tx.clone(),
        );

        let human_loop = HumanLoop::new(
            self.config.human_loop.clone(),
            self.store.clone(),
            self.sink.clone(),
            self.interaction_layers.clone(),
            events_tx.clone(),
        );

        let dispatcher = Arc::new(Dispatcher::new(
            self.config.dispatcher.clone(),
            self.config.workflow.clone(),
            self.source.clone(),
            self.sink.clone(),
            self.store.clone(),
            self.router.clone(),
            self.repo.clone(),
            events_tx.clone(),
        ));

        let validator = Arc::new(ValidatorCoordinator::new(
            self.config.workflow.clone(),
            self.review_ai.clone(),
            self.repo.clone(),
            self.store.clone(),
            self.sink.clone(),
            self.interaction_layers.clone(),
            events_tx.clone(),
        ));

        // Spawn long-running loops.
        tokio::spawn(async move { scheduler.run().await });
        tokio::spawn(async move { monitor.run().await });
        tokio::spawn(async move { human_loop.run().await });

        let store = self.store.clone();
        let interaction_layers = self.interaction_layers.clone();

        // Step 3: Event routing loop (main loop).
        tracing::info!("Engine event loop running");
        while let Some(event) = events_rx.recv().await {
            let dispatcher = dispatcher.clone();
            let validator = validator.clone();
            let store = store.clone();
            let layers = interaction_layers.clone();

            tokio::spawn(async move {
                if let Err(e) = route_event(event, dispatcher, validator, store, layers).await {
                    tracing::error!("Event routing error: {e}");
                }
            });
        }

        tracing::info!("OrchestratorEngine shutting down");
        Ok(())
    }
}

// ── Event routing ─────────────────────────────────────────────────────────────

async fn route_event(
    event: OrchestratorEvent,
    dispatcher: Arc<Dispatcher>,
    validator: Arc<ValidatorCoordinator>,
    store: Arc<dyn StateStore>,
    interaction_layers: Vec<Arc<dyn InteractionLayer>>,
) -> anyhow::Result<()> {
    match event {
        OrchestratorEvent::DispatchReady { task_id, .. } => {
            tracing::debug!(task_id = %task_id, "Routing DispatchReady");
            dispatcher.dispatch(task_id).await?;
        }

        OrchestratorEvent::RunCompleted {
            task_id, run_id, ..
        } => {
            tracing::debug!(task_id = %task_id, run_id = %run_id, "Routing RunCompleted");
            validator.handle_run_completed(&task_id, &run_id).await?;
        }

        // Route RunTimedOut to a stuck-recovery path: create an interaction ticket so
        // the human loop will send a StuckNotification to all configured channels.
        OrchestratorEvent::RunTimedOut {
            task_id, run_id, ..
        } => {
            tracing::warn!(task_id = %task_id, run_id = %run_id, "Routing RunTimedOut");

            let req = InteractionRequest::new(
                task_id.clone(),
                run_id.clone(),
                InteractionRequestKind::StuckNotification {
                    reason: "Stall timeout: no worker output within the configured window".into(),
                },
                format!("[Stuck] Task {task_id}"),
                format!(
                    "Task `{task_id}` (run `{run_id}`) exceeded the stall timeout and is now Stuck.\n\
                     Manual intervention is required."
                ),
                vec![
                    InteractionAction::Retry,
                    InteractionAction::Reroute { backend: "local".into() },
                    InteractionAction::Close,
                ],
            );

            let ticket = InteractionTicket::new(req.clone());
            if let Err(e) = store.save_ticket(&ticket).await {
                tracing::error!(task_id = %task_id, "Failed to persist stuck ticket: {e}");
            }

            // Also send immediately to all layers (human loop will retry on failure).
            for layer in &interaction_layers {
                if let Err(e) = layer.send(&req).await {
                    tracing::warn!(
                        channel = layer.name(),
                        "Failed to send stuck notification: {e}"
                    );
                }
            }
        }

        // Route InteractionResolved: if the human decision moved the task back to
        // Dispatching (Retry or Reroute), trigger immediate re-dispatch without
        // waiting for the next scheduler tick.
        OrchestratorEvent::InteractionResolved { task_id, .. } => {
            tracing::debug!(task_id = %task_id, "Routing InteractionResolved");

            if let Ok(Some(record)) = store.get_task(&task_id).await {
                if record.status == TaskStatus::Dispatching {
                    tracing::info!(
                        task_id = %task_id,
                        "Task in Dispatching after human resolution — triggering re-dispatch"
                    );
                    if let Err(e) = dispatcher.dispatch(task_id).await {
                        tracing::error!("Re-dispatch after interaction failed: {e}");
                    }
                }
            }
        }

        // ValidationResult is informational — the ValidatorCoordinator handles transitions.
        // Log it here for observability; no additional routing needed.
        OrchestratorEvent::ValidationResult {
            task_id,
            run_id,
            ref outcome,
            ..
        } => {
            tracing::info!(
                task_id = %task_id,
                run_id = %run_id,
                passed = outcome.passed,
                "Routing ValidationResult (informational)"
            );
        }

        _ => {
            // Events not routed here are handled by individual subsystems directly.
        }
    }

    Ok(())
}
