//! Discord webhook server — receives Discord interactions and messages.
//!
//! This module provides an HTTP server that receives Discord webhook events:
//!   1. Slash commands (`/thala create ...`) via Discord Interactions API
//!   2. Message components (button clicks) for approvals/retry
//!
//! Routes:
//!   POST /api/discord/interaction — Discord Interaction endpoint
//!
//! # Example: receiving a slash command
//!
//! 1. User types `/thala create Add a login button`
//! 2. Discord POSTs to /api/discord/interaction with type 2 (APPLICATION_COMMAND)
//! 3. Server verifies signature using Ed25519
//! 4. Server parses command and calls DiscordIntake.handle_create()
//! 5. Server replies with deferred response → follow-up message with task ID

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::adapters::intake::discord::{DiscordIntake, DiscordIntakeMessage};
use crate::adapters::interaction::discord::DiscordInteraction;
use crate::core::error::ThalaError;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8789";

/// Discord webhook server configuration.
#[derive(Debug, Clone)]
pub struct DiscordWebhookConfig {
    pub bind_addr: SocketAddr,
    pub public_key: String,
    pub bot_token: String,
    pub intake_enabled: bool,
    pub interaction_enabled: bool,
}

impl DiscordWebhookConfig {
    /// Load from environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = std::env::var("THALA_DISCORD_BIND").unwrap_or_else(|_| DEFAULT_BIND_ADDR.into());
        let bind_addr = raw
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid THALA_DISCORD_BIND '{raw}': {e}"))?;

        let public_key = std::env::var("DISCORD_PUBLIC_KEY")
            .map_err(|_| anyhow::anyhow!("DISCORD_PUBLIC_KEY not set"))?;
        let bot_token = std::env::var("DISCORD_BOT_TOKEN")
            .map_err(|_| anyhow::anyhow!("DISCORD_BOT_TOKEN not set"))?;

        let intake_enabled = std::env::var("DISCORD_INTAKE_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);
        let interaction_enabled = std::env::var("DISCORD_INTERACTION_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true);

        Ok(Self {
            bind_addr,
            public_key,
            bot_token: normalize_bot_token(&bot_token),
            intake_enabled,
            interaction_enabled,
        })
    }

    /// Create from workflow config.
    pub fn from_workflow(
        discord_cfg: &crate::core::workflow::DiscordConfig,
        bind_addr: Option<SocketAddr>,
    ) -> Self {
        Self {
            bind_addr: bind_addr.unwrap_or_else(|| DEFAULT_BIND_ADDR.parse().unwrap()),
            public_key: discord_cfg.public_key.clone(),
            bot_token: normalize_bot_token(&discord_cfg.bot_token),
            intake_enabled: true,
            interaction_enabled: true,
        }
    }
}

/// State shared across request handlers.
#[derive(Clone)]
pub struct DiscordWebhookState {
    pub config: DiscordWebhookConfig,
    pub intake: Option<Arc<DiscordIntake>>,
    pub interaction: Option<Arc<DiscordInteraction>>,
}

/// Discord webhook server.
pub struct DiscordWebhookServer {
    state: DiscordWebhookState,
}

impl DiscordWebhookServer {
    pub fn new(
        config: DiscordWebhookConfig,
        intake: Option<Arc<DiscordIntake>>,
        interaction: Option<Arc<DiscordInteraction>>,
    ) -> Self {
        Self {
            state: DiscordWebhookState {
                config,
                intake,
                interaction,
            },
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let bind_addr = self.state.config.bind_addr;
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        tracing::info!(%bind_addr, "Discord webhook server listening");

        let app = self.router();
        axum::serve(listener, app).await?;
        Ok(())
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/api/discord/interaction", post(handle_interaction))
            .route("/api/discord/test", post(handle_test))
            .with_state(self.state.clone())
    }
}

/// Discord interaction payload.
/// Discord sends type as integer (1, 2, 3), not string.
#[derive(Debug, Deserialize)]
struct DiscordInteractionPayload {
    #[serde(rename = "type")]
    pub interaction_type: u8,
    pub id: Option<String>,
    #[serde(rename = "channel_id")]
    pub channel_id: Option<String>,
    #[serde(rename = "guild_id")]
    pub guild_id: Option<String>,
    pub data: Option<CommandData>,
    pub member: Option<serde_json::Value>,
    pub user: Option<serde_json::Value>,
}

impl DiscordInteractionPayload {
    pub fn is_ping(&self) -> bool {
        self.interaction_type == 1
    }

    pub fn is_application_command(&self) -> bool {
        self.interaction_type == 2
    }

    pub fn is_message_component(&self) -> bool {
        self.interaction_type == 3
    }
}

#[derive(Debug, Deserialize)]
struct CommandData {
    name: String,
    #[serde(default)]
    options: Vec<CommandOption>,
}

#[derive(Debug, Deserialize)]
struct CommandOption {
    name: String,
    value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ComponentData {
    #[serde(rename = "custom_id")]
    custom_id: String,
}

/// Discord interaction response.
/// Discord expects integer type values (1, 4, 5), not string names.
#[derive(Debug, Serialize)]
struct DiscordResponse {
    /// 1 = Pong, 4 = ChannelMessageWithSource, 5 = DeferredChannelMessageWithSource
    #[serde(rename = "type")]
    response_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<ResponseData>,
}

impl DiscordResponse {
    /// Type 1 — pong for ping
    fn pong() -> Self {
        Self {
            response_type: 1,
            data: None,
        }
    }

    /// Type 4 — reply immediately
    fn channel_message_with_source(data: ResponseData) -> Self {
        Self {
            response_type: 4,
            data: Some(data),
        }
    }

    /// Type 5 — deferred reply (process async)
    fn deferred_channel_message_with_source(data: ResponseData) -> Self {
        Self {
            response_type: 5,
            data: Some(data),
        }
    }
}

#[derive(Debug, Serialize)]
struct ResponseData {
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    embeds: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    components: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "allowed_mentions")]
    allowed_mentions: Option<serde_json::Value>,
}

/// Request signature headers.
#[derive(Debug)]
struct SignatureHeaders {
    timestamp: String,
    signature: String,
}

fn extract_signature_headers(headers: &HeaderMap) -> Option<SignatureHeaders> {
    let timestamp = headers
        .get("x-signature-timestamp")?
        .to_str()
        .ok()?
        .to_string();
    let signature = headers
        .get("x-signature-ed25519")?
        .to_str()
        .ok()?
        .to_string();
    Some(SignatureHeaders {
        timestamp,
        signature,
    })
}

/// Verify Discord request signature using Ed25519.
fn verify_discord_signature(
    public_key_hex: &str,
    timestamp: &str,
    body: &[u8],
    signature_hex: &str,
) -> bool {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    tracing::debug!(
        public_key_len = public_key_hex.len(),
        signature_len = signature_hex.len(),
        timestamp_len = timestamp.len(),
        body_len = body.len(),
        "Starting signature verification"
    );

    let key_bytes = match hex::decode(public_key_hex) {
        Ok(b) => {
            tracing::debug!(key_bytes_len = b.len(), "Public key decoded");
            b
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to decode public key hex");
            return false;
        }
    };

    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) => {
            tracing::debug!(sig_bytes_len = b.len(), "Signature decoded");
            b
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to decode signature hex");
            return false;
        }
    };

    let Ok(key_array) = <[u8; 32]>::try_from(key_bytes) else {
        tracing::warn!("Public key is not 32 bytes");
        return false;
    };
    tracing::debug!("Public key is 32 bytes");

    let Ok(verifying_key) = VerifyingKey::from_bytes(&key_array) else {
        tracing::warn!("Failed to create verifying key from bytes");
        return false;
    };
    tracing::debug!("Verifying key created");

    let Ok(signature) = Signature::from_slice(&sig_bytes) else {
        tracing::warn!("Failed to create signature from bytes");
        return false;
    };
    tracing::debug!("Signature object created");

    // Signed message = timestamp + body
    let mut msg = Vec::with_capacity(timestamp.len() + body.len());
    msg.extend_from_slice(timestamp.as_bytes());
    msg.extend_from_slice(body);

    tracing::debug!(
        msg_len = msg.len(),
        msg_hex = %hex::encode(&msg),
        "Message prepared for verification"
    );

    let result = verifying_key.verify(&msg, &signature);
    match result {
        Ok(()) => {
            tracing::debug!("Signature verification succeeded");
            true
        }
        Err(e) => {
            tracing::warn!(error = ?e, "Signature verification failed - Ed25519 verify returned error");
            false
        }
    }
}

/// Main handler for Discord interactions.
async fn handle_interaction(
    State(state): State<DiscordWebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Extract and verify signature
    let Some(sig_headers) = extract_signature_headers(&headers) else {
        tracing::warn!("Missing Discord signature headers");
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Missing signature headers"})),
        )
            .into_response();
    };

    tracing::info!(
        timestamp = %sig_headers.timestamp,
        body_len = body.len(),
        signature_len = sig_headers.signature.len(),
        "Discord interaction received - verifying signature"
    );

    let is_valid = verify_discord_signature(
        &state.config.public_key,
        &sig_headers.timestamp,
        &body,
        &sig_headers.signature,
    );

    if is_valid {
        tracing::debug!("Discord signature verified successfully");
    } else {
        tracing::warn!("Discord signature verification failed");
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid signature"})),
        )
            .into_response();
    }

    // Parse payload
    let payload = match serde_json::from_slice::<DiscordInteractionPayload>(&body) {
        Ok(p) => {
            tracing::debug!(
                interaction_type = p.interaction_type,
                "Discord interaction received"
            );
            p
        }
        Err(e) => {
            tracing::warn!(error = %e, body_len = body.len(), "Failed to parse Discord interaction");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid JSON"})),
            )
                .into_response();
        }
    };

    if payload.is_ping() {
        // Type 1: Ping - respond with Pong (type: 1)
        tracing::debug!("Discord ping received");
        (StatusCode::OK, Json(DiscordResponse::pong())).into_response()
    } else if payload.is_application_command() {
        // Type 2: Application Command (slash command)
        let channel_id = payload.channel_id.unwrap_or_default();
        let guild_id = payload.guild_id;
        let data = payload.data.unwrap_or(CommandData {
            name: String::new(),
            options: Vec::new(),
        });
        handle_slash_command(
            state,
            channel_id,
            guild_id,
            data,
            payload.member,
            payload.user,
        )
    } else if payload.is_message_component() {
        // Type 3: Message Component (button click)
        // Parse component data from the payload
        let custom_id = payload
            .data
            .as_ref()
            .and_then(|d| d.options.first())
            .and_then(|o| o.value.as_str())
            .unwrap_or("")
            .to_string();
        let component_data = ComponentData { custom_id };
        handle_component_interaction(state, component_data, payload.member, payload.user)
    } else {
        // Unknown type
        tracing::warn!(
            interaction_type = payload.interaction_type,
            "Unknown Discord interaction type"
        );
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Unknown interaction type"})),
        )
            .into_response()
    }
}

/// Test handler - always returns Pong without verification
async fn handle_test() -> impl IntoResponse {
    tracing::info!("Test endpoint hit - returning Pong without verification");
    // Return a simple JSON response that matches Discord's expected format
    (StatusCode::OK, Json(serde_json::json!({"type": 1})))
}

/// Handle slash commands.
fn handle_slash_command(
    state: DiscordWebhookState,
    channel_id: String,
    guild_id: Option<String>,
    data: CommandData,
    member: Option<serde_json::Value>,
    user: Option<serde_json::Value>,
) -> axum::response::Response {
    let user_id = member
        .as_ref()
        .and_then(|m| m.get("user"))
        .and_then(|u| u.get("id"))
        .and_then(|id| id.as_str())
        .or_else(|| {
            user.as_ref()
                .and_then(|u| u.get("id"))
                .and_then(|id| id.as_str())
        })
        .unwrap_or("unknown");

    match data.name.as_str() {
        "thala" | "create" => {
            // Extract description from options
            let description = data
                .options
                .iter()
                .find(|o| o.name == "description")
                .and_then(|o| o.value.as_str())
                .map_or_else(
                    || {
                        // Try to extract from "task" option if present
                        data.options
                            .iter()
                            .find(|o| o.name == "task")
                            .and_then(|o| o.value.as_str())
                            .map(std::string::ToString::to_string)
                            .unwrap_or_default()
                    },
                    std::string::ToString::to_string,
                );

            if description.is_empty() {
                return reply_response(
                    "Please provide a task description. Usage: `/thala create <description>`",
                );
            }

            // Send deferred response (process async) - type 5
            let deferred = DiscordResponse::deferred_channel_message_with_source(ResponseData {
                content: "Creating task...".to_string(),
                embeds: None,
                components: None,
                allowed_mentions: Some(json!({"parse": []})),
            });

            // Spawn async processing
            if let Some(intake) = &state.intake {
                let intake_clone = Arc::clone(intake);
                let msg = DiscordIntakeMessage {
                    channel_id: channel_id.clone(),
                    user_id: user_id.to_string(),
                    guild_id: guild_id.clone(),
                    content: description,
                    message_id: "slash-cmd".to_string(),
                };
                let bot_token = state.config.bot_token.clone();

                tokio::spawn(async move {
                    let reply = intake_clone.handle_create(msg).await;

                    // Send follow-up message via Discord API
                    let _ = send_followup_message(&bot_token, &channel_id, &reply).await;
                });
            } else {
                return reply_response("Discord intake is not enabled.");
            }

            (StatusCode::OK, Json(deferred)).into_response()
        }
        _ => reply_response("Unknown command. Available: `/thala create <description>`"),
    }
}

/// Handle button clicks (component interactions).
fn handle_component_interaction(
    _state: DiscordWebhookState,
    data: ComponentData,
    _member: Option<serde_json::Value>,
    _user: Option<serde_json::Value>,
) -> axum::response::Response {
    // Parse custom_id: "thala:{action}:{interaction_id}:{run_id}:{task_id}"
    let parts: Vec<&str> = data.custom_id.split(':').collect();
    if parts.len() < 2 || parts[0] != "thala" {
        return reply_response("Invalid interaction");
    }

    let action = parts[1];

    // Acknowledge the interaction - type 4
    let response = DiscordResponse::channel_message_with_source(ResponseData {
        content: format!("Processing {} action...", action),
        embeds: None,
        components: None,
        allowed_mentions: Some(json!({"parse": []})),
    });

    // The actual handling would be done by the interaction layer polling
    // For now, we just acknowledge and the orchestrator will handle it
    tracing::info!(action = %action, "Discord component interaction received");

    (StatusCode::OK, Json(response)).into_response()
}

/// Helper to create a simple text response.
fn reply_response(text: &str) -> axum::response::Response {
    let response = DiscordResponse::channel_message_with_source(ResponseData {
        content: text.to_string(),
        embeds: None,
        components: None,
        allowed_mentions: Some(json!({"parse": []})),
    });
    (StatusCode::OK, Json(response)).into_response()
}

/// Send a follow-up message to Discord channel via API.
async fn send_followup_message(
    bot_token: &str,
    channel_id: &str,
    content: &str,
) -> Result<(), ThalaError> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://discord.com/api/v10/channels/{}/messages",
        channel_id
    );

    let body = json!({
        "content": content,
        "allowed_mentions": {"parse": []}
    });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {}", bot_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| ThalaError::interaction(format!("Discord API call failed: {}", e)))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Err(ThalaError::interaction(format!(
            "Discord API returned {}: {}",
            status, text
        )))
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

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn test_signature_verification_invalid() {
        // Invalid signature should fail
        let result = verify_discord_signature(
            "0000000000000000000000000000000000000000000000000000000000000000",
            "1234567890",
            b"test body",
            "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(!result);
    }

    #[test]
    fn test_signature_verification_valid() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verify_key_hex = hex::encode(signing_key.verifying_key().to_bytes());
        let timestamp = "1776873383";
        let body = br#"{"type":1}"#;

        let mut msg = Vec::with_capacity(timestamp.len() + body.len());
        msg.extend_from_slice(timestamp.as_bytes());
        msg.extend_from_slice(body);

        let signature = signing_key.sign(&msg);
        let signature_hex = hex::encode(signature.to_bytes());

        let result = verify_discord_signature(&verify_key_hex, timestamp, body, &signature_hex);
        assert!(result);
    }

    #[test]
    fn test_extract_signature_headers_missing() {
        let headers = HeaderMap::new();
        assert!(extract_signature_headers(&headers).is_none());
    }

    #[test]
    fn test_normalize_bot_token_supports_workflow_env_reference() {
        unsafe {
            std::env::set_var("DISCORD_BOT_TOKEN", "abc123");
        }

        assert_eq!(normalize_bot_token("Bot ${DISCORD_BOT_TOKEN}"), "abc123");
    }
}
