//! Human interaction domain types.
//!
//! The orchestrator emits InteractionRequests when human input is needed.
//! These are channel-agnostic — no Slack blocks, no Discord embeds.
//! Adapters translate them into platform-native messages.
//!
//! InteractionResolutions flow back from the channel adapters and are consumed
//! by the human_loop to drive task transitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::ids::{InteractionId, RunId, TaskId};

// ── InteractionRequest ────────────────────────────────────────────────────────

/// A request for human input emitted by the orchestrator.
///
/// Invariant: only one pending request per run at a time. The human_loop
/// must resolve or expire the previous request before emitting another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRequest {
    pub id: InteractionId,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub kind: InteractionRequestKind,

    /// Short summary for notification subject lines.
    pub summary: String,

    /// Longer detail text for the message body.
    pub detail: String,

    /// Which actions the human may take. Adapters render these as buttons/commands.
    pub available_actions: Vec<InteractionAction>,

    pub created_at: DateTime<Utc>,

    /// If set, the request auto-expires at this time. The orchestrator will
    /// take the `on_timeout_action` if no resolution is received.
    pub expires_at: Option<DateTime<Utc>>,

    /// Action to apply automatically if the request expires without a response.
    pub on_timeout_action: Option<InteractionAction>,
}

impl InteractionRequest {
    pub fn new(
        task_id: TaskId,
        run_id: RunId,
        kind: InteractionRequestKind,
        summary: impl Into<String>,
        detail: impl Into<String>,
        available_actions: Vec<InteractionAction>,
    ) -> Self {
        Self {
            id: InteractionId::new_v4(),
            task_id,
            run_id,
            kind,
            summary: summary.into(),
            detail: detail.into(),
            available_actions,
            created_at: Utc::now(),
            expires_at: None,
            on_timeout_action: None,
        }
    }
}

// ── InteractionRequestKind ────────────────────────────────────────────────────

/// What kind of human input is being requested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InteractionRequestKind {
    /// A PR is ready and a human must approve before merge.
    ApprovalRequired { pr_url: String, pr_number: u32 },

    /// The task is stuck and requires human intervention to continue.
    StuckNotification { reason: String },

    /// The review AI rejected the output and the human should review the feedback.
    ReviewRejected {
        feedback: String,
        pr_diff_summary: Option<String>,
    },

    /// The task needs additional context before it can be dispatched.
    ContextNeeded { missing_fields: Vec<String> },

    /// A hard failure occurred; the human must decide whether to retry, reroute, or close.
    ManualResolution { error: String },
}

// ── InteractionAction ─────────────────────────────────────────────────────────

/// An action the human can take in response to an InteractionRequest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractionAction {
    /// Approve the request (merge, proceed, accept).
    Approve,

    /// Reject the output with an optional reason; triggers a retry.
    Reject,

    /// Retry the task from the start, optionally on a different backend.
    Retry,

    /// Reroute to a specific backend for the next attempt.
    Reroute { backend: String },

    /// Escalate to another team or channel.
    Escalate,

    /// Close the task without further action.
    Close,

    /// Ignore; do nothing now.
    Ignore,
}

// ── InteractionResolution ─────────────────────────────────────────────────────

/// The human's decision in response to an InteractionRequest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionResolution {
    pub request_id: InteractionId,
    pub task_id: TaskId,
    pub run_id: RunId,
    pub action: InteractionAction,

    /// Optional free-text reason or additional context from the human.
    pub note: Option<String>,

    /// When the resolution was received.
    pub resolved_at: DateTime<Utc>,

    /// Who resolved it (e.g. "slack:U12345ABC" or "discord:987654321").
    pub resolved_by: String,
}

// ── InteractionTicket ─────────────────────────────────────────────────────────

/// A persisted record of a pending interaction request.
/// Stored in the StateStore so it survives restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionTicket {
    pub request: InteractionRequest,

    /// Whether this ticket has been sent to at least one interaction channel.
    pub sent: bool,

    /// Platform-specific message reference (e.g. Slack message ts, Discord message ID).
    /// Used to update the message when the request is resolved.
    pub channel_message_ref: Option<String>,
}

impl InteractionTicket {
    pub fn new(request: InteractionRequest) -> Self {
        Self {
            request,
            sent: false,
            channel_message_ref: None,
        }
    }

    pub fn is_expired(&self) -> bool {
        self.request.expires_at.is_some_and(|exp| Utc::now() > exp)
    }
}
