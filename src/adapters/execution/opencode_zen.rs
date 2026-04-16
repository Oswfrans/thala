//! OpenCodeZenBackend — managed worker sessions on OpenCode Zen.
//!
//! How it works:
//!   1. `launch()` POSTs to the OpenCode Zen Sessions API, passing the task
//!      prompt (base64), callback URL, and GitHub branch as JSON.
//!   2. `observe()` GETs the session status to detect progress via cursor changes.
//!   3. `cancel()` DELETEs the session.
//!   4. Completion is signaled by a callback POST to Thala's gateway.
//!
//! Required environment variables:
//!   - `OPENCODE_API_KEY`  — OpenCode Zen API key (opencode.ai/settings)
//!
//! Optional environment variables:
//!   - `OPENCODE_ZEN_BASE_URL` — Override base URL (default: https://opencode.ai/zen/v1)
//!
//! Runtime state persisted in TaskRun:
//!   - job_handle.job_id: session ID returned by the API (e.g. "oz-abc123")
//!   - remote_branch: branch pushed to origin before spawning
//!   - callback_token_hash: SHA-256 of the per-run bearer token

use std::path::Path;

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, WorkerHandle};
use crate::ports::execution::{ExecutionBackend, LaunchRequest, LaunchedRun};

const DEFAULT_BASE_URL: &str = "https://opencode.ai/zen/v1";

// ── OpenCodeZenConfig ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OpenCodeZenConfig {
    /// OpenCode Zen API base URL.
    pub base_url: String,
}

impl Default for OpenCodeZenConfig {
    fn default() -> Self {
        Self {
            base_url: std::env::var("OPENCODE_ZEN_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.into()),
        }
    }
}

impl OpenCodeZenConfig {
    pub fn from_env() -> Self {
        Self::default()
    }
}

// ── OpenCodeZenBackend ────────────────────────────────────────────────────────

pub struct OpenCodeZenBackend {
    config: OpenCodeZenConfig,
    http: reqwest::Client,
}

impl OpenCodeZenBackend {
    pub fn new(config: OpenCodeZenConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    fn api_key() -> Option<String> {
        std::env::var("OPENCODE_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
    }
}

#[async_trait]
impl ExecutionBackend for OpenCodeZenBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::OpenCodeZen
    }

    fn is_local(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "opencode-zen"
    }

    async fn launch(&self, req: LaunchRequest) -> Result<LaunchedRun, ThalaError> {
        let remote_branch = req.remote_branch.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "opencode-zen",
                "remote_branch is required for OpenCode Zen backend",
            )
        })?;

        let callback_url = req.callback_url.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "opencode-zen",
                "callback_url is required for OpenCode Zen backend",
            )
        })?;

        let github_repo = req.github_repo.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "opencode-zen",
                "github_repo is required for OpenCode Zen backend",
            )
        })?;

        let github_token = req.github_token.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "opencode-zen",
                "github_token is required for OpenCode Zen backend",
            )
        })?;

        let api_key = Self::api_key()
            .ok_or_else(|| ThalaError::backend("opencode-zen", "OPENCODE_API_KEY is not set"))?;

        let mut body = serde_json::json!({
            "run_id": req.run_id,
            "task_id": req.task_id,
            "product": req.product,
            "prompt_b64": base64_encode(&req.prompt),
            "model": req.model,
            "branch": remote_branch,
            "callback_url": callback_url,
            "github_repo": github_repo,
            "github_token": github_token,
        });

        if let Some(token) = &req.callback_token {
            body["callback_token"] = serde_json::Value::String(token.clone());
        }

        let url = format!("{}/sessions", self.config.base_url);

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&api_key)
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                ThalaError::backend("opencode-zen", format!("OpenCode Zen API call failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ThalaError::backend(
                "opencode-zen",
                format!("OpenCode Zen API returned {status}: {text}"),
            ));
        }

        let data: serde_json::Value = resp.json().await.map_err(|e| {
            ThalaError::backend(
                "opencode-zen",
                format!("OpenCode Zen API response parse failed: {e}"),
            )
        })?;

        // Response: { "id": "oz-abc123", "status": "queued" }
        let session_id = data["id"]
            .as_str()
            .ok_or_else(|| ThalaError::backend("opencode-zen", "No session ID in response"))?
            .to_string();

        tracing::info!(
            task_id = %req.task_id,
            session_id = %session_id,
            branch = %remote_branch,
            "OpenCode Zen worker spawned"
        );

        Ok(LaunchedRun {
            handle: WorkerHandle {
                job_id: session_id,
                backend: ExecutionBackendKind::OpenCodeZen,
            },
            worktree_path: None,
            remote_branch: Some(remote_branch.to_string()),
        })
    }

    async fn observe(&self, handle: &WorkerHandle) -> Result<RunObservation, ThalaError> {
        let api_key = Self::api_key().unwrap_or_default();
        let url = format!("{}/sessions/{}", self.config.base_url, handle.job_id);

        let resp = self
            .http
            .get(&url)
            .bearer_auth(&api_key)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                ThalaError::backend(
                    "opencode-zen",
                    format!("OpenCode Zen status poll failed: {e}"),
                )
            })?;

        if !resp.status().is_success() {
            // Session may have been deleted or expired.
            return Ok(RunObservation {
                cursor: "deleted".into(),
                is_alive: false,
                observed_at: chrono::Utc::now(),
            });
        }

        let data: serde_json::Value = resp.json().await.map_err(|e| {
            ThalaError::backend(
                "opencode-zen",
                format!("OpenCode Zen status parse failed: {e}"),
            )
        })?;

        // Response: { "id": "oz-...", "status": "queued|running|completed|failed", "output_cursor": "..." }
        let status = data["status"].as_str().unwrap_or("unknown");
        let is_alive = matches!(status, "queued" | "running" | "starting");

        // Use the server-provided output cursor when present; fall back to status.
        let cursor = data["output_cursor"].as_str().unwrap_or(status).to_string();

        Ok(RunObservation {
            cursor,
            is_alive,
            observed_at: chrono::Utc::now(),
        })
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), ThalaError> {
        let api_key = Self::api_key().unwrap_or_default();
        let url = format!("{}/sessions/{}", self.config.base_url, handle.job_id);

        let _ = self
            .http
            .delete(&url)
            .bearer_auth(&api_key)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await;

        Ok(())
    }

    async fn cleanup(
        &self,
        handle: &WorkerHandle,
        _workspace_root: &Path,
        task_id: &str,
    ) -> Result<(), ThalaError> {
        self.cancel(handle).await?;
        tracing::info!(task_id, session_id = %handle.job_id, "OpenCode Zen cleanup complete");
        Ok(())
    }
}

fn base64_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}
