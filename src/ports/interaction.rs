//! InteractionLayer port — notify humans, request input, receive responses.
//!
//! Both Slack and Discord implement this trait. The orchestrator calls it
//! without knowing which platform is on the other end.
//!
//! Invariant: no Slack blocks, Discord embeds, or platform-specific payloads
//! cross this boundary. Adapters translate InteractionRequest to platform
//! native format internally.

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::interaction::{InteractionRequest, InteractionResolution};

// ── InteractionLayer ──────────────────────────────────────────────────────────

/// Abstraction over human interaction channels (Slack, Discord).
///
/// Implementations: SlackInteraction, DiscordInteraction.
#[async_trait]
pub trait InteractionLayer: Send + Sync {
    /// Human-readable name for this channel (e.g. "slack", "discord").
    fn name(&self) -> &'static str;

    /// Send an interaction request to the channel.
    ///
    /// The adapter translates the request into a platform-native message
    /// (Slack blocks, Discord embed) and posts it. Returns a platform-specific
    /// message reference (Slack ts, Discord message ID) for later update.
    async fn send(&self, request: &InteractionRequest) -> Result<Option<String>, ThalaError>;

    /// Update an already-sent message to reflect that it has been resolved.
    /// Used to visually close out the request in the channel.
    ///
    /// `message_ref` is the value returned by `send()`.
    async fn update_sent(
        &self,
        message_ref: &str,
        resolution: &InteractionResolution,
    ) -> Result<(), ThalaError>;

    /// Poll for pending resolutions from this channel.
    ///
    /// For Slack: checks for button interactions via the Events API.
    /// For Discord: checks for slash command responses or button interactions.
    ///
    /// Returns all unprocessed resolutions since the last poll.
    /// Each resolution must reference a known InteractionRequest ID.
    async fn poll_resolutions(&self) -> Result<Vec<InteractionResolution>, ThalaError>;
}

// ── Notes on intake vs interaction ───────────────────────────────────────────
//
// This port is for INTERACTION only — approvals, retries, escalations.
//
// Intake (Slack/Discord message → new task in Beads) is handled by separate
// adapter types (adapters/intake/slack.rs, adapters/intake/discord.rs) that
// do NOT implement this trait. Intake writes to TaskSink; interaction reads
// from and writes to the human.
//
// Keeping them separate means the interaction layer can be registered without
// also enabling intake, and vice versa.
