//! SlackInteraction — implements InteractionLayer for Slack.
//!
//! Translates InteractionRequest (core type) into Slack Block Kit messages.
//! Translates Slack button interactions into InteractionResolution values.
//!
//! Boundary rule: no core type imports from this module upstream.
//! The orchestrator calls this only via the InteractionLayer trait.
//!
//! # Example: issuing an approval request
//!
//! The orchestrator calls `layer.send(request)` where request.kind is
//! ApprovalRequired { pr_url, pr_number }. This adapter builds a Block Kit
//! message with Approve and Reject buttons and posts it to the alerts channel.
//!
//! # Example: receiving a retry decision
//!
//! User clicks the "Retry" button in Slack. Slack posts an interaction payload
//! to Thala's webhook endpoint. The endpoint calls
//! `layer.poll_resolutions()` (or processes inline) to return an
//! InteractionResolution { action: Retry, resolved_by: "slack:U12345" }.

use std::path::PathBuf;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

use crate::core::error::ThalaError;
use crate::core::interaction::{
    InteractionAction, InteractionRequest, InteractionRequestKind, InteractionResolution,
};
use crate::ports::interaction::InteractionLayer;

// ── SlackInteractionConfig ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SlackInteractionConfig {
    /// Slack bot token (xoxb-…).
    pub bot_token: String,

    /// Slack signing secret for verifying webhook payloads.
    pub signing_secret: String,

    /// Default channel ID for posting interaction requests.
    pub alerts_channel: String,

    /// Path to the SQLite database used as the durable resolution inbox.
    /// Resolutions written here survive Thala restarts.
    pub db_path: PathBuf,
}

// ── SlackInteraction ──────────────────────────────────────────────────────────

pub struct SlackInteraction {
    config: SlackInteractionConfig,
    http: reqwest::Client,
    /// SQLite connection used as a durable inbox for pending resolutions.
    /// Resolutions are inserted on `receive_interaction` and drained on `poll_resolutions`.
    db: Mutex<Connection>,
}

impl SlackInteraction {
    /// Open (or create) the Slack interaction adapter, initialising the SQLite inbox.
    pub fn new(config: SlackInteractionConfig) -> Result<Self, ThalaError> {
        if let Some(parent) = config.db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ThalaError::interaction(format!(
                    "Failed to create Slack DB directory '{}': {e}",
                    parent.display()
                ))
            })?;
        }

        let conn = Connection::open(&config.db_path).map_err(|e| {
            ThalaError::interaction(format!(
                "Failed to open Slack interactions DB at '{}': {e}",
                config.db_path.display()
            ))
        })?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS slack_pending_resolutions (
                id      INTEGER PRIMARY KEY AUTOINCREMENT,
                payload TEXT    NOT NULL
            );",
        )
        .map_err(|e| {
            ThalaError::interaction(format!("Failed to initialise Slack inbox schema: {e}"))
        })?;

        Ok(Self {
            config,
            http: reqwest::Client::new(),
            db: Mutex::new(conn),
        })
    }

    /// Verify a Slack request signature using HMAC-SHA256.
    ///
    /// Slack signs every Events API and Interactivity request. Verification
    /// must happen in the HTTP handler before passing the payload to
    /// `receive_interaction`.
    ///
    /// # Arguments
    ///
    /// * `timestamp` — value of the `X-Slack-Request-Timestamp` header
    /// * `body`      — raw request body bytes (URL-encoded form or JSON)
    /// * `signature` — value of the `X-Slack-Signature` header (starts with `v0=`)
    ///
    /// Returns `false` if the signature is invalid or if the request is more
    /// than 5 minutes old (replay-attack protection).
    pub fn verify_signature(&self, timestamp: &str, body: &[u8], signature: &str) -> bool {
        verify_slack_signature(&self.config.signing_secret, timestamp, body, signature)
    }

    /// Called by the Slack webhook endpoint when a button interaction arrives.
    /// Parses the Slack payload into an InteractionResolution and buffers it.
    ///
    /// Callers MUST verify the signature with `verify_signature` before calling this.
    pub fn receive_interaction(&self, payload: &serde_json::Value) -> Result<(), ThalaError> {
        let action_id = payload["actions"][0]["action_id"]
            .as_str()
            .unwrap_or("ignore");

        let user_id = payload["user"]["id"].as_str().unwrap_or("unknown");

        // action_id format: "thala:{action}:{interaction_id}:{run_id}:{task_id}"
        let parts: Vec<&str> = action_id.splitn(5, ':').collect();
        if parts.len() < 5 || parts[0] != "thala" {
            return Err(ThalaError::interaction(
                "Invalid action_id format in Slack payload",
            ));
        }

        let action = parse_action(parts[1]);
        let interaction_id = crate::core::ids::InteractionId::from(parts[2]);
        let run_id = crate::core::ids::RunId::from(parts[3]);
        let task_id = crate::core::ids::TaskId::from(parts[4]);

        let note = payload["actions"][0]["value"]
            .as_str()
            .map(ToString::to_string);

        let resolution = InteractionResolution {
            request_id: interaction_id,
            task_id,
            run_id,
            action,
            note,
            resolved_at: chrono::Utc::now(),
            resolved_by: format!("slack:{user_id}"),
        };

        let payload = serde_json::to_string(&resolution).map_err(|e| {
            ThalaError::interaction(format!("Failed to serialize resolution: {e}"))
        })?;

        self.db
            .lock()
            .execute(
                "INSERT INTO slack_pending_resolutions (payload) VALUES (?1)",
                params![payload],
            )
            .map_err(|e| ThalaError::interaction(format!("Failed to persist resolution: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl InteractionLayer for SlackInteraction {
    fn name(&self) -> &'static str {
        "slack"
    }

    async fn send(&self, request: &InteractionRequest) -> Result<Option<String>, ThalaError> {
        let blocks = build_slack_blocks(request);

        let body = serde_json::json!({
            "channel": self.config.alerts_channel,
            "blocks": blocks,
            "text": &request.summary,  // fallback for notifications
        });

        let resp = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.config.bot_token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ThalaError::interaction(format!("Slack API call failed: {e}")))?;

        let data: serde_json::Value = resp.json().await.map_err(|e| {
            ThalaError::interaction(format!("Slack API response parse failed: {e}"))
        })?;

        if !data["ok"].as_bool().unwrap_or(false) {
            let error = data["error"].as_str().unwrap_or("unknown");
            return Err(ThalaError::interaction(format!("Slack API error: {error}")));
        }

        // Return the message timestamp as the channel reference.
        let ts = data["ts"].as_str().map(ToString::to_string);
        Ok(ts)
    }

    async fn update_sent(
        &self,
        message_ref: &str,
        resolution: &InteractionResolution,
    ) -> Result<(), ThalaError> {
        let text = format!(
            "✅ Resolved by {} — {:?}",
            resolution.resolved_by, resolution.action
        );

        let body = serde_json::json!({
            "channel": self.config.alerts_channel,
            "ts": message_ref,
            "text": text,
            "blocks": [{
                "type": "section",
                "text": {"type": "mrkdwn", "text": text}
            }]
        });

        let resp = self
            .http
            .post("https://slack.com/api/chat.update")
            .bearer_auth(&self.config.bot_token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| ThalaError::interaction(format!("Slack update failed: {e}")))?;

        if !resp.status().is_success() {
            tracing::warn!(message_ref, "Failed to update Slack message");
        }

        Ok(())
    }

    async fn poll_resolutions(&self) -> Result<Vec<InteractionResolution>, ThalaError> {
        let conn = self.db.lock();

        // Collect all pending rows ordered by insertion sequence.
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, payload FROM slack_pending_resolutions ORDER BY id",
                )
                .map_err(|e| {
                    ThalaError::interaction(format!("DB prepare failed: {e}"))
                })?;

            let collected: Vec<(i64, String)> =
                stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
                    .map_err(|e| ThalaError::interaction(format!("DB query failed: {e}")))?
                    .filter_map(|r| r.ok())
                    .collect();
            collected
        };

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        // Delete everything up to and including the highest ID we just read.
        let max_id = rows.last().map(|(id, _)| *id).unwrap_or(0);
        conn.execute(
            "DELETE FROM slack_pending_resolutions WHERE id <= ?1",
            params![max_id],
        )
        .map_err(|e| ThalaError::interaction(format!("DB delete failed: {e}")))?;

        // Deserialize; skip rows whose JSON is corrupt (log and continue).
        let resolutions = rows
            .into_iter()
            .filter_map(|(_, payload)| {
                serde_json::from_str::<InteractionResolution>(&payload)
                    .inspect_err(|e| {
                        tracing::warn!("Skipping malformed Slack resolution in SQLite inbox: {e}");
                    })
                    .ok()
            })
            .collect();

        Ok(resolutions)
    }
}

// ── Slack block builder ───────────────────────────────────────────────────────

fn build_slack_blocks(request: &InteractionRequest) -> serde_json::Value {
    let mut blocks = vec![serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": format!("*{}*\n{}", request.summary, request.detail)
        }
    })];

    // Add context based on request kind.
    let context_text = match &request.kind {
        InteractionRequestKind::ApprovalRequired { pr_url, pr_number } => {
            Some(format!("<{pr_url}|PR #{pr_number}>"))
        }
        InteractionRequestKind::StuckNotification { reason } => Some(format!("Reason: {reason}")),
        InteractionRequestKind::ReviewRejected { feedback, .. } => {
            Some(format!("Feedback: {feedback}"))
        }
        InteractionRequestKind::ContextNeeded { missing_fields } => {
            Some(format!("Missing: {}", missing_fields.join(", ")))
        }
        InteractionRequestKind::ManualResolution { error } => Some(format!("Error: {error}")),
    };

    if let Some(text) = context_text {
        blocks.push(serde_json::json!({
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": text}]
        }));
    }

    // Add action buttons.
    if !request.available_actions.is_empty() {
        let buttons: Vec<serde_json::Value> = request
            .available_actions
            .iter()
            .map(|action| {
                let (text, style) = action_label_and_style(action);
                let action_id = format!(
                    "thala:{}:{}:{}:{}",
                    action_code(action),
                    request.id,
                    request.run_id,
                    request.task_id,
                );
                let mut btn = serde_json::json!({
                    "type": "button",
                    "text": {"type": "plain_text", "text": text},
                    "action_id": action_id,
                });
                if let Some(s) = style {
                    btn["style"] = serde_json::Value::String(s);
                }
                btn
            })
            .collect();

        blocks.push(serde_json::json!({
            "type": "actions",
            "elements": buttons
        }));
    }

    serde_json::Value::Array(blocks)
}

fn action_label_and_style(action: &InteractionAction) -> (&'static str, Option<String>) {
    match action {
        InteractionAction::Approve => ("Approve", Some("primary".into())),
        InteractionAction::Reject => ("Reject", Some("danger".into())),
        InteractionAction::Retry => ("Retry", None),
        InteractionAction::Reroute { .. } => ("Reroute", None),
        InteractionAction::Escalate => ("Escalate", None),
        InteractionAction::Close => ("Close", Some("danger".into())),
        InteractionAction::Ignore => ("Ignore", None),
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

/// Verify a Slack request signature (HMAC-SHA256 with version prefix "v0").
///
/// Reference: <https://api.slack.com/authentication/verifying-requests-from-slack>
fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &[u8],
    signature: &str,
) -> bool {
    // Reject requests older than 5 minutes to prevent replay attacks.
    if let Ok(ts) = timestamp.parse::<i64>() {
        let now = chrono::Utc::now().timestamp();
        if (now - ts).abs() > 300 {
            tracing::warn!("Slack: request timestamp is outside the 5-minute window");
            return false;
        }
    } else {
        tracing::warn!("Slack: could not parse X-Slack-Request-Timestamp");
        return false;
    }

    // Build the signing base string: "v0:{timestamp}:{body}"
    let base = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));

    let Ok(mut mac) = HmacSha256::new_from_slice(signing_secret.as_bytes()) else {
        tracing::warn!("Slack: failed to create HMAC");
        return false;
    };
    mac.update(base.as_bytes());
    let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

    // Constant-time comparison to prevent timing side-channel attacks.
    constant_time_eq(expected.as_bytes(), signature.as_bytes())
}

/// Constant-time byte-slice comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
