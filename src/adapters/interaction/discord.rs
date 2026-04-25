//! DiscordInteraction — implements InteractionLayer for Discord.
//!
//! Translates InteractionRequest into Discord embeds with component buttons.
//! Translates Discord button interactions into InteractionResolution values.
//!
//! # Example: issuing an approval request
//!
//! Orchestrator calls `layer.send(request)`.
//! This adapter POSTs a Discord message with an embed and Approve/Reject buttons.
//! Returns the Discord message ID as the channel reference.
//!
//! # Example: receiving a retry decision
//!
//! User clicks "Retry" in Discord. Discord sends an interaction to Thala's
//! interactions endpoint. The endpoint calls `receive_interaction()`.
//! Next `poll_resolutions()` call returns the InteractionResolution.

use async_trait::async_trait;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use parking_lot::Mutex;

use crate::core::error::ThalaError;
use crate::core::interaction::{
    InteractionAction, InteractionRequest, InteractionRequestKind, InteractionResolution,
};
use crate::ports::interaction::InteractionLayer;

// ── DiscordInteractionConfig ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DiscordInteractionConfig {
    /// Discord bot token.
    pub bot_token: String,

    /// Discord application public key for verifying interaction signatures.
    pub public_key: String,

    /// Channel ID to post interaction requests to.
    pub alerts_channel_id: String,
}

// ── DiscordInteraction ────────────────────────────────────────────────────────

pub struct DiscordInteraction {
    config: DiscordInteractionConfig,
    http: reqwest::Client,
    pending_resolutions: Mutex<Vec<InteractionResolution>>,
}

impl DiscordInteraction {
    pub fn new(config: DiscordInteractionConfig) -> Self {
        let config = DiscordInteractionConfig {
            bot_token: normalize_bot_token(&config.bot_token),
            ..config
        };
        Self {
            config,
            http: reqwest::Client::new(),
            pending_resolutions: Mutex::new(Vec::new()),
        }
    }

    /// Verify a Discord interaction request signature using Ed25519.
    ///
    /// Discord signs every interaction with the application's private key.
    /// The signed message is `timestamp + body` (concatenated, no separator).
    ///
    /// Call this before `receive_interaction`. Return `false` means reject the request.
    ///
    /// # Arguments
    ///
    /// * `timestamp` — value of the `X-Signature-Timestamp` header
    /// * `body`      — raw request body bytes
    /// * `signature` — value of the `X-Signature-Ed25519` header (hex-encoded)
    pub fn verify_signature(&self, timestamp: &str, body: &[u8], signature: &str) -> bool {
        verify_ed25519_signature(&self.config.public_key, timestamp, body, signature)
    }

    /// Called by the Discord interactions endpoint when a button is clicked.
    /// The custom_id format is: "thala:{action}:{interaction_id}:{run_id}:{task_id}"
    ///
    /// Callers MUST verify the signature with `verify_signature` before calling this.
    pub fn receive_interaction(&self, payload: &serde_json::Value) -> Result<(), ThalaError> {
        let custom_id = payload["data"]["custom_id"].as_str().unwrap_or("");
        let user_id = payload["member"]["user"]["id"]
            .as_str()
            .or_else(|| payload["user"]["id"].as_str())
            .unwrap_or("unknown");

        let parts: Vec<&str> = custom_id.splitn(5, ':').collect();
        if parts.len() < 5 || parts[0] != "thala" {
            return Err(ThalaError::interaction(
                "Invalid custom_id format in Discord interaction",
            ));
        }

        let action = parse_action(parts[1]);
        let interaction_id = crate::core::ids::InteractionId::from(parts[2]);
        let run_id = crate::core::ids::RunId::from(parts[3]);
        let task_id = crate::core::ids::TaskId::from(parts[4]);

        let resolution = InteractionResolution {
            request_id: interaction_id,
            task_id,
            run_id,
            action,
            note: None,
            resolved_at: chrono::Utc::now(),
            resolved_by: format!("discord:{user_id}"),
        };

        self.pending_resolutions.lock().push(resolution);
        Ok(())
    }
}

#[async_trait]
impl InteractionLayer for DiscordInteraction {
    fn name(&self) -> &'static str {
        "discord"
    }

    async fn send(&self, request: &InteractionRequest) -> Result<Option<String>, ThalaError> {
        let body = build_discord_message(request);

        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            self.config.alerts_channel_id
        );

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ThalaError::interaction(format!("Discord API call failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ThalaError::interaction(format!(
                "Discord API returned {status}: {text}"
            )));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ThalaError::interaction(format!("Discord response parse failed: {e}")))?;

        let message_id = data["id"].as_str().map(ToString::to_string);
        Ok(message_id)
    }

    async fn update_sent(
        &self,
        message_ref: &str,
        resolution: &InteractionResolution,
    ) -> Result<(), ThalaError> {
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages/{message_ref}",
            self.config.alerts_channel_id
        );

        let text = format!(
            "✅ Resolved by {} — {:?}",
            resolution.resolved_by, resolution.action
        );

        let body = serde_json::json!({
            "content": text,
            "components": []  // Remove buttons after resolution.
        });

        let resp = self
            .http
            .patch(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ThalaError::interaction(format!("Discord update failed: {e}")))?;

        if !resp.status().is_success() {
            tracing::warn!(message_ref, "Failed to update Discord message");
        }

        Ok(())
    }

    async fn poll_resolutions(&self) -> Result<Vec<InteractionResolution>, ThalaError> {
        let resolutions = std::mem::take(&mut *self.pending_resolutions.lock());
        Ok(resolutions)
    }
}

fn normalize_bot_token(raw: &str) -> String {
    let trimmed = raw.trim();
    let token = trimmed.strip_prefix("Bot ").unwrap_or(trimmed).trim();

    expand_env_var(token).unwrap_or_else(|| token.to_string())
}

fn expand_env_var(raw: &str) -> Option<String> {
    if let Some(name) = raw.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(name).ok();
    }

    let name = raw.strip_prefix('$')?;
    if name
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
    {
        return std::env::var(name).ok();
    }

    None
}

// ── Discord message builder ───────────────────────────────────────────────────

fn build_discord_message(request: &InteractionRequest) -> serde_json::Value {
    let description = match &request.kind {
        InteractionRequestKind::ApprovalRequired { pr_url, pr_number } => {
            format!("{}\n\n[PR #{pr_number}]({pr_url})", request.detail)
        }
        InteractionRequestKind::StuckNotification { reason } => {
            format!("{}\n\n**Reason:** {reason}", request.detail)
        }
        InteractionRequestKind::ReviewRejected { feedback, .. } => {
            format!("{}\n\n**Feedback:**\n```\n{feedback}\n```", request.detail)
        }
        InteractionRequestKind::ContextNeeded { missing_fields } => {
            format!(
                "{}\n\n**Missing:** {}",
                request.detail,
                missing_fields.join(", ")
            )
        }
        InteractionRequestKind::ManualResolution { error } => {
            format!("{}\n\n**Error:** `{error}`", request.detail)
        }
    };

    let embed = serde_json::json!({
        "title": request.summary,
        "description": description,
        "color": embed_color(&request.kind),
        "footer": {
            "text": format!("task:{} run:{}", request.task_id, request.run_id)
        }
    });

    let components = if request.available_actions.is_empty() {
        vec![]
    } else {
        let buttons: Vec<serde_json::Value> = request
            .available_actions
            .iter()
            .map(|action| {
                let custom_id = format!(
                    "thala:{}:{}:{}:{}",
                    action_code(action),
                    request.id,
                    request.run_id,
                    request.task_id,
                );
                serde_json::json!({
                    "type": 2,  // Button component type
                    "label": action_label(action),
                    "style": button_style(action),
                    "custom_id": custom_id,
                })
            })
            .collect();

        vec![serde_json::json!({
            "type": 1,  // Action row
            "components": buttons
        })]
    };

    serde_json::json!({
        "embeds": [embed],
        "components": components,
    })
}

fn embed_color(kind: &InteractionRequestKind) -> u32 {
    match kind {
        InteractionRequestKind::ApprovalRequired { .. } => 0x0058_65F2, // Discord blurple
        InteractionRequestKind::StuckNotification { .. } => 0x00FE_E75C, // yellow
        InteractionRequestKind::ReviewRejected { .. }
        | InteractionRequestKind::ManualResolution { .. } => 0x00ED_4245, // red
        InteractionRequestKind::ContextNeeded { .. } => 0x00EB_459E,    // pink
    }
}

fn action_label(action: &InteractionAction) -> &'static str {
    match action {
        InteractionAction::Approve => "Approve",
        InteractionAction::Reject => "Reject",
        InteractionAction::Retry => "Retry",
        InteractionAction::Reroute { .. } => "Reroute",
        InteractionAction::Escalate => "Escalate",
        InteractionAction::Close => "Close",
        InteractionAction::Ignore => "Ignore",
    }
}

fn button_style(action: &InteractionAction) -> u8 {
    match action {
        InteractionAction::Approve => 3, // Success (green)
        InteractionAction::Reject | InteractionAction::Close => 4, // Danger (red)
        _ => 2,                          // Secondary (grey)
    }
}

fn action_code(action: &InteractionAction) -> &'static str {
    match action {
        InteractionAction::Approve => "approve",
        InteractionAction::Reject => "reject",
        InteractionAction::Retry => "retry",
        InteractionAction::Reroute { .. } => "reroute",
        InteractionAction::Escalate => "escalate",
        InteractionAction::Close => "close",
        InteractionAction::Ignore => "ignore",
    }
}

fn parse_action(code: &str) -> InteractionAction {
    match code {
        "approve" => InteractionAction::Approve,
        "reject" => InteractionAction::Reject,
        "retry" => InteractionAction::Retry,
        "escalate" => InteractionAction::Escalate,
        "close" => InteractionAction::Close,
        _ => InteractionAction::Ignore,
    }
}

// ── Signature verification ────────────────────────────────────────────────────

/// Verify a Discord interaction request using Ed25519.
///
/// Discord signs interactions as: ed25519_sign(private_key, timestamp_bytes + body_bytes)
/// The public key and signature are hex-encoded.
fn verify_ed25519_signature(
    public_key_hex: &str,
    timestamp: &str,
    body: &[u8],
    signature_hex: &str,
) -> bool {
    let Ok(key_bytes) = hex::decode(public_key_hex) else {
        tracing::warn!("Discord: failed to decode public key hex");
        return false;
    };

    let Ok(sig_bytes) = hex::decode(signature_hex) else {
        tracing::warn!("Discord: failed to decode signature hex");
        return false;
    };

    let Ok(key_array) = <[u8; 32]>::try_from(key_bytes) else {
        tracing::warn!("Discord: public key is not 32 bytes");
        return false;
    };

    let Ok(verifying_key) = VerifyingKey::from_bytes(&key_array) else {
        tracing::warn!("Discord: invalid Ed25519 public key");
        return false;
    };

    let Ok(signature) = Signature::from_slice(&sig_bytes) else {
        tracing::warn!("Discord: invalid Ed25519 signature");
        return false;
    };

    // The signed message is the concatenation of timestamp and body bytes.
    let mut msg = Vec::with_capacity(timestamp.len() + body.len());
    msg.extend_from_slice(timestamp.as_bytes());
    msg.extend_from_slice(body);

    verifying_key.verify(&msg, &signature).is_ok()
}
