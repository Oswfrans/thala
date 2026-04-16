//! Human loop — manages interaction requests and applies resolutions.
//!
//! The human loop runs on a slow interval. Each tick it:
//!   1. Sends any unsent interaction tickets through all registered channels.
//!   2. Polls each channel for new resolutions.
//!   3. Applies each resolution via state transitions.
//!   4. Expires timed-out tickets and applies default actions.
//!   5. Emits events for resolved interactions.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;

use crate::core::events::OrchestratorEvent;
use crate::core::interaction::{
    InteractionAction, InteractionRequest, InteractionRequestKind, InteractionResolution,
    InteractionTicket,
};
use crate::core::run::ExecutionBackendKind;
use crate::core::transitions::{apply_transition, Transition};
use crate::ports::interaction::InteractionLayer;
use crate::ports::state_store::StateStore;
use crate::ports::task_sink::TaskSink;

// ── HumanLoopConfig ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct HumanLoopConfig {
    /// How often to poll channels for resolutions.
    pub poll_interval: Duration,
}

impl Default for HumanLoopConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(20),
        }
    }
}

// ── HumanLoop ─────────────────────────────────────────────────────────────────

pub struct HumanLoop {
    config: HumanLoopConfig,
    store: Arc<dyn StateStore>,
    sink: Arc<dyn TaskSink>,
    interaction_layers: Vec<Arc<dyn InteractionLayer>>,
    events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
}

impl HumanLoop {
    pub fn new(
        config: HumanLoopConfig,
        store: Arc<dyn StateStore>,
        sink: Arc<dyn TaskSink>,
        interaction_layers: Vec<Arc<dyn InteractionLayer>>,
        events_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
    ) -> Self {
        Self {
            config,
            store,
            sink,
            interaction_layers,
            events_tx,
        }
    }

    /// Run the human loop. Runs until the process exits.
    pub async fn run(self) {
        tracing::info!(
            poll_interval_secs = self.config.poll_interval.as_secs(),
            channels = self.interaction_layers.len(),
            "Human loop starting"
        );

        loop {
            if let Err(e) = self.tick().await {
                tracing::error!("Human loop tick failed: {e}");
            }
            sleep(self.config.poll_interval).await;
        }
    }

    async fn tick(&self) -> anyhow::Result<()> {
        // 1. Send unsent tickets.
        self.send_pending_tickets().await?;

        // 2. Poll channels for resolutions.
        for layer in &self.interaction_layers {
            match layer.poll_resolutions().await {
                Ok(resolutions) => {
                    for resolution in resolutions {
                        if let Err(e) = self.apply_resolution(resolution).await {
                            tracing::error!("Failed to apply resolution: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(channel = layer.name(), "Failed to poll resolutions: {e}");
                }
            }
        }

        // 3. Check for expired tickets.
        self.expire_timed_out_tickets().await?;

        Ok(())
    }

    async fn send_pending_tickets(&self) -> anyhow::Result<()> {
        let tickets = self.store.pending_tickets().await?;

        for ticket in tickets {
            if ticket.sent {
                continue;
            }

            let mut updated = ticket.clone();

            for layer in &self.interaction_layers {
                match layer.send(&ticket.request).await {
                    Ok(Some(msg_ref)) => {
                        updated.channel_message_ref = Some(msg_ref);
                        updated.sent = true;
                    }
                    Ok(None) => {
                        updated.sent = true;
                    }
                    Err(e) => {
                        tracing::warn!(
                            channel = layer.name(),
                            interaction_id = %ticket.request.id,
                            "Failed to send interaction ticket: {e}"
                        );
                    }
                }
            }

            if updated.sent {
                self.store.update_ticket(&updated).await?;
            }
        }

        Ok(())
    }

    async fn apply_resolution(&self, resolution: InteractionResolution) -> anyhow::Result<()> {
        tracing::info!(
            interaction_id = %resolution.request_id,
            task_id = %resolution.task_id,
            action = ?resolution.action,
            resolved_by = %resolution.resolved_by,
            "Applying human resolution"
        );

        // Load task record.
        let Some(mut record) = self.store.get_task(&resolution.task_id).await? else {
            tracing::warn!(task_id = %resolution.task_id, "Task not found for resolution");
            return Ok(());
        };

        // Derive the transition from the action.
        let transition = match &resolution.action {
            InteractionAction::Approve => Transition::HumanApproved,
            InteractionAction::Reject => Transition::HumanRejected {
                reason: resolution
                    .note
                    .clone()
                    .unwrap_or_else(|| "rejected by human".into()),
            },
            InteractionAction::Retry => Transition::RecoveryRequested,
            InteractionAction::Close => Transition::HumanResolved,
            InteractionAction::Escalate => {
                tracing::info!(task_id = %resolution.task_id, "Escalation requested — forwarding to all channels");
                self.send_escalation_notification(&resolution).await?;
                // Mark the ticket resolved so it is not re-sent.
                self.store.resolve_ticket(&resolution).await?;
                return Ok(());
            }
            InteractionAction::Ignore => {
                tracing::debug!(task_id = %resolution.task_id, "Interaction ignored");
                return Ok(());
            }
            InteractionAction::Reroute { ref backend } => {
                // Parse the backend string into an ExecutionBackendKind and store it
                // as a reroute hint on the task record. The dispatcher will use it
                // for the next attempt.
                let backend_kind = parse_backend_kind(backend);
                tracing::info!(
                    task_id = %resolution.task_id,
                    backend = %backend,
                    "Reroute requested — setting backend hint"
                );
                if let Some(mut rec) = self.store.get_task(&resolution.task_id).await? {
                    rec.reroute_hint = Some(backend_kind);
                    self.store.upsert_task(&rec).await?;
                }
                Transition::RecoveryRequested
            }
        };

        // Apply transition.
        record = apply_transition(&record, transition)?;
        self.store.upsert_task(&record).await?;

        // Persist the resolution.
        self.store.resolve_ticket(&resolution).await?;

        // Update the message in the channel (mark as resolved).
        if let Some(ticket) = self.store.get_ticket(&resolution.request_id).await? {
            if let Some(msg_ref) = &ticket.channel_message_ref {
                for layer in &self.interaction_layers {
                    let _ = layer.update_sent(msg_ref, &resolution).await;
                }
            }
        }

        // Emit event.
        let _ = self
            .events_tx
            .send(OrchestratorEvent::InteractionResolved {
                task_id: resolution.task_id.clone(),
                run_id: resolution.run_id.clone(),
                interaction_id: resolution.request_id.clone(),
                at: chrono::Utc::now(),
            })
            .await;

        Ok(())
    }

    /// Send an escalation notification for a human-triggered escalation.
    ///
    /// Creates a new ManualResolution interaction request so all channels
    /// receive an escalation alert with Retry / Close actions.
    async fn send_escalation_notification(
        &self,
        resolution: &InteractionResolution,
    ) -> anyhow::Result<()> {
        let req = InteractionRequest::new(
            resolution.task_id.clone(),
            resolution.run_id.clone(),
            InteractionRequestKind::ManualResolution {
                error: format!(
                    "Escalated by {} — {}",
                    resolution.resolved_by,
                    resolution.note.as_deref().unwrap_or("no additional note")
                ),
            },
            format!("[ESCALATED] Task {}", resolution.task_id),
            format!(
                "Task `{}` has been escalated by `{}`.\n\
                 This task requires immediate human attention.",
                resolution.task_id, resolution.resolved_by
            ),
            vec![InteractionAction::Retry, InteractionAction::Close],
        );

        let ticket = InteractionTicket::new(req.clone());
        if let Err(e) = self.store.save_ticket(&ticket).await {
            tracing::warn!("Failed to persist escalation ticket: {e}");
        }

        for layer in &self.interaction_layers {
            if let Err(e) = layer.send(&req).await {
                tracing::warn!(
                    channel = layer.name(),
                    "Failed to send escalation notification: {e}"
                );
            }
        }

        Ok(())
    }

    async fn expire_timed_out_tickets(&self) -> anyhow::Result<()> {
        let tickets = self.store.pending_tickets().await?;

        for ticket in tickets {
            if !ticket.is_expired() {
                continue;
            }

            let Some(default_action) = &ticket.request.on_timeout_action else {
                continue;
            };

            tracing::info!(
                interaction_id = %ticket.request.id,
                task_id = %ticket.request.task_id,
                "Interaction ticket expired — applying default action"
            );

            // Synthesise a resolution for the default action.
            let resolution = InteractionResolution {
                request_id: ticket.request.id.clone(),
                task_id: ticket.request.task_id.clone(),
                run_id: ticket.request.run_id.clone(),
                action: default_action.clone(),
                note: Some("auto-applied: ticket expired".into()),
                resolved_at: chrono::Utc::now(),
                resolved_by: "thala:timeout".into(),
            };

            self.apply_resolution(resolution).await?;
        }

        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a backend name string into an ExecutionBackendKind.
/// Defaults to Local for unrecognised names.
fn parse_backend_kind(name: &str) -> ExecutionBackendKind {
    match name.to_lowercase().as_str() {
        "modal" => ExecutionBackendKind::Modal,
        "cloudflare" | "cf" => ExecutionBackendKind::Cloudflare,
        _ => ExecutionBackendKind::Local,
    }
}
