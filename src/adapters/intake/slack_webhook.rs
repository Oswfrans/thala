//! Slack webhook server — receives slash commands and button interactions.
//!
//! Routes:
//!   POST /api/slack/command     — Slack slash command for task intake
//!   POST /api/slack/interaction — Slack Block Kit button interactions

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;

use crate::adapters::intake::slack::{SlackIntake, SlackIntakeMessage};
use crate::adapters::interaction::slack::SlackInteraction;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8790";

#[derive(Debug, Clone)]
pub struct SlackWebhookConfig {
    pub bind_addr: SocketAddr,
    pub intake_enabled: bool,
    pub interaction_enabled: bool,
}

impl SlackWebhookConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = std::env::var("THALA_SLACK_BIND").unwrap_or_else(|_| DEFAULT_BIND_ADDR.into());
        let bind_addr = raw
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid THALA_SLACK_BIND '{raw}': {e}"))?;

        Ok(Self {
            bind_addr,
            intake_enabled: truthy_env("SLACK_INTAKE_ENABLED", true),
            interaction_enabled: truthy_env("SLACK_INTERACTION_ENABLED", true),
        })
    }
}

#[derive(Clone)]
pub struct SlackWebhookState {
    pub interaction: Arc<SlackInteraction>,
    pub intake: Option<Arc<SlackIntake>>,
    pub config: SlackWebhookConfig,
    pub http: reqwest::Client,
}

pub struct SlackWebhookServer {
    state: SlackWebhookState,
}

impl SlackWebhookServer {
    pub fn new(
        config: SlackWebhookConfig,
        interaction: Arc<SlackInteraction>,
        intake: Option<Arc<SlackIntake>>,
    ) -> Self {
        Self {
            state: SlackWebhookState {
                interaction,
                intake,
                config,
                http: reqwest::Client::new(),
            },
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let bind_addr = self.state.config.bind_addr;
        let listener = tokio::net::TcpListener::bind(bind_addr).await?;
        tracing::info!(%bind_addr, "Slack webhook server listening");

        axum::serve(listener, self.router()).await?;
        Ok(())
    }

    fn router(&self) -> Router {
        Router::new()
            .route("/api/slack/command", post(handle_command))
            .route("/api/slack/interaction", post(handle_interaction))
            .with_state(self.state.clone())
    }
}

async fn handle_interaction(
    State(state): State<SlackWebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !state.config.interaction_enabled {
        return slack_text_response("Slack interaction handling is not enabled.").into_response();
    }
    if !verify_request(&state.interaction, &headers, &body) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid signature"})),
        )
            .into_response();
    }

    let payload = match parse_interaction_payload(&body) {
        Ok(payload) => payload,
        Err(e) => {
            tracing::warn!("Slack interaction payload parse failed: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid payload"})),
            )
                .into_response();
        }
    };

    if let Err(e) = state.interaction.receive_interaction(&payload) {
        tracing::warn!("Failed to record Slack interaction: {e}");
        return slack_text_response("Failed to process interaction.").into_response();
    }

    slack_text_response("Processing action...").into_response()
}

async fn handle_command(
    State(state): State<SlackWebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if !state.config.intake_enabled {
        return slack_text_response("Slack intake is not enabled.").into_response();
    }
    if !verify_request(&state.interaction, &headers, &body) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid signature"})),
        )
            .into_response();
    }

    let form = parse_form(&body);
    let text = form.get("text").map_or("", String::as_str).trim();
    if text.is_empty() {
        return slack_text_response("Please provide a task description.").into_response();
    }

    let Some(intake) = state.intake else {
        return slack_text_response("Slack intake is not enabled.").into_response();
    };

    let msg = SlackIntakeMessage {
        channel_id: form.get("channel_id").cloned().unwrap_or_default(),
        user_id: form.get("user_id").cloned().unwrap_or_default(),
        text: text.to_string(),
        thread_ts: form.get("thread_ts").cloned(),
    };
    let response_url = form.get("response_url").cloned();
    let http = state.http.clone();

    tokio::spawn(async move {
        let reply = intake.handle(msg).await;
        if let Some(url) = response_url {
            if let Err(e) = post_response_url(&http, &url, &reply).await {
                tracing::warn!("Slack response_url update failed: {e}");
            }
        }
    });

    slack_text_response("Creating task...").into_response()
}

fn verify_request(interaction: &SlackInteraction, headers: &HeaderMap, body: &[u8]) -> bool {
    let timestamp = headers
        .get("x-slack-request-timestamp")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let signature = headers
        .get("x-slack-signature")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    interaction.verify_signature(timestamp, body, signature)
}

fn parse_interaction_payload(body: &[u8]) -> Result<serde_json::Value, String> {
    if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(body) {
        return Ok(payload);
    }

    let form = parse_form(body);
    let raw_payload = form
        .get("payload")
        .ok_or_else(|| "missing payload form field".to_string())?;
    serde_json::from_str(raw_payload).map_err(|e| e.to_string())
}

fn parse_form(body: &[u8]) -> HashMap<String, String> {
    let raw = String::from_utf8_lossy(body);
    raw.split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Some((percent_decode(key)?, percent_decode(value)?))
        })
        .collect()
}

fn percent_decode(input: &str) -> Option<String> {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_value(bytes[i + 1])?;
                let lo = hex_value(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b'%' => return None,
            b => {
                out.push(b);
                i += 1;
            }
        }
    }

    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn slack_text_response(text: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "response_type": "ephemeral",
            "text": text,
        })),
    )
}

async fn post_response_url(
    http: &reqwest::Client,
    response_url: &str,
    text: &str,
) -> Result<(), reqwest::Error> {
    http.post(response_url)
        .json(&json!({
            "response_type": "ephemeral",
            "replace_original": true,
            "text": text,
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

fn truthy_env(name: &str, default: bool) -> bool {
    std::env::var(name).map_or(default, |value| {
        matches!(
            value.as_str(),
            "1" | "true" | "TRUE" | "yes" | "YES" | "y" | "Y"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urlencoded_slack_form() {
        let form =
            parse_form(b"text=Fix+mobile+login&channel_id=C123&payload=%7B%22ok%22%3Atrue%7D");

        assert_eq!(
            form.get("text").map(String::as_str),
            Some("Fix mobile login")
        );
        assert_eq!(form.get("channel_id").map(String::as_str), Some("C123"));
        assert_eq!(
            form.get("payload").map(String::as_str),
            Some(r#"{"ok":true}"#)
        );
    }

    #[test]
    fn parses_block_kit_payload_from_form() {
        let payload = parse_interaction_payload(
            br"payload=%7B%22actions%22%3A%5B%7B%22action_id%22%3A%22thala%3Aretry%3Aint-1%3Arun-1%3Atask-1%22%7D%5D%7D",
        )
        .unwrap();

        assert_eq!(
            payload["actions"][0]["action_id"].as_str(),
            Some("thala:retry:int-1:run-1:task-1")
        );
    }
}
