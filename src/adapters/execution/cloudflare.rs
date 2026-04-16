//! CloudflareBackend — containers on Cloudflare Containers via REST API.
//!
//! How it works:
//!   1. `launch()` POSTs to the CF Containers API to create a container,
//!      passing env vars for the task prompt, callback URL, and GitHub branch.
//!   2. `observe()` GETs the container status from the CF API.
//!   3. `cancel()` DELETEs or stops the container.
//!   4. Completion is signaled by a callback POST to Thala's gateway (not polling).
//!
//! Runtime state that must be persisted (stored in TaskRun):
//!   - job_handle.job_id: Cloudflare container instance ID
//!   - remote_branch: the branch pushed to origin before spawning
//!   - callback_token_hash: SHA-256 of the per-run bearer token

use std::path::Path;

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, WorkerHandle};
use crate::ports::execution::{ExecutionBackend, LaunchRequest, LaunchedRun};

// ── CloudflareConfig ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct CloudflareConfig {
    /// Cloudflare account ID. Falls back to CF_ACCOUNT_ID env var.
    pub account_id: String,

    /// Container image reference (e.g. "docker.io/yourorg/thala-worker:latest").
    pub image: String,

    /// Number of vCPUs to allocate.
    pub vcpus: Option<u32>,

    /// Memory in MB to allocate.
    pub memory_mb: Option<u32>,
}

// ── CloudflareBackend ─────────────────────────────────────────────────────────

pub struct CloudflareBackend {
    config: CloudflareConfig,
    http: reqwest::Client,
}

impl CloudflareConfig {
    /// Construct from environment variables.
    ///
    /// Reads:
    ///   - `CF_ACCOUNT_ID`        — Cloudflare account ID
    ///   - `CF_WORKER_IMAGE`      — Container image reference
    ///   - `CF_WORKER_VCPUS`      — vCPU count (optional, integer)
    ///   - `CF_WORKER_MEMORY_MB`  — Memory in MB (optional, integer)
    pub fn from_env() -> Self {
        Self {
            account_id: std::env::var("CF_ACCOUNT_ID").unwrap_or_default(),
            image: std::env::var("CF_WORKER_IMAGE").unwrap_or_default(),
            vcpus: std::env::var("CF_WORKER_VCPUS")
                .ok()
                .and_then(|v| v.parse().ok()),
            memory_mb: std::env::var("CF_WORKER_MEMORY_MB")
                .ok()
                .and_then(|v| v.parse().ok()),
        }
    }
}

impl CloudflareBackend {
    pub fn new(config: CloudflareConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Construct from environment variables (shorthand for `new(CloudflareConfig::from_env())`).
    pub fn from_env() -> Self {
        Self::new(CloudflareConfig::from_env())
    }

    fn account_id(&self) -> String {
        if self.config.account_id.is_empty() {
            std::env::var("CF_ACCOUNT_ID").unwrap_or_default()
        } else {
            self.config.account_id.clone()
        }
    }

    fn api_token() -> Option<String> {
        std::env::var("CF_API_TOKEN").ok().filter(|t| !t.is_empty())
    }
}

#[async_trait]
impl ExecutionBackend for CloudflareBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Cloudflare
    }

    fn is_local(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "cloudflare"
    }

    async fn launch(&self, req: LaunchRequest) -> Result<LaunchedRun, ThalaError> {
        let remote_branch = req.remote_branch.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "cloudflare",
                "remote_branch is required for Cloudflare backend",
            )
        })?;

        let callback_url = req.callback_url.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "cloudflare",
                "callback_url is required for Cloudflare backend",
            )
        })?;

        let github_repo = req.github_repo.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "cloudflare",
                "github_repo is required for Cloudflare backend",
            )
        })?;

        let github_token = req.github_token.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "cloudflare",
                "github_token is required for Cloudflare backend",
            )
        })?;

        let api_token = Self::api_token()
            .ok_or_else(|| ThalaError::backend("cloudflare", "CF_API_TOKEN is not set"))?;

        let account_id = self.account_id();
        if account_id.is_empty() {
            return Err(ThalaError::backend(
                "cloudflare",
                "CF_ACCOUNT_ID is not set",
            ));
        }

        // Build environment variables for the container.
        // CF Containers API uses an "env" array of {"name", "value"} objects.
        let mut env_vars = vec![
            cf_env("THALA_RUN_ID", &req.run_id),
            cf_env("THALA_TASK_ID", &req.task_id),
            cf_env("THALA_PROMPT_B64", &base64_encode(&req.prompt)),
            cf_env("THALA_MODEL", &req.model),
            cf_env("THALA_BRANCH", remote_branch),
            cf_env("THALA_CALLBACK_URL", callback_url),
            cf_env("GITHUB_REPO", github_repo),
            cf_env("GITHUB_TOKEN", github_token),
        ];

        if let Some(token) = &req.callback_token {
            env_vars.push(cf_env("THALA_CALLBACK_TOKEN", token));
        }

        let mut body = serde_json::json!({
            "image": self.config.image,
            "env": env_vars,
        });

        // Populate the resources block from whichever limits are configured.
        match (self.config.vcpus, self.config.memory_mb) {
            (Some(vcpus), Some(mem)) => {
                body["resources"] = serde_json::json!({"vcpus": vcpus, "memory_mb": mem});
            }
            (Some(vcpus), None) => {
                body["resources"] = serde_json::json!({"vcpus": vcpus});
            }
            (None, Some(mem)) => {
                body["resources"] = serde_json::json!({"memory_mb": mem});
            }
            (None, None) => {}
        }

        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{account_id}/containers/instances"
        );

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&api_token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ThalaError::backend("cloudflare", format!("CF API call failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ThalaError::backend(
                "cloudflare",
                format!("CF API returned {status}: {text}"),
            ));
        }

        let data: serde_json::Value = resp.json().await.map_err(|e| {
            ThalaError::backend("cloudflare", format!("CF API response parse failed: {e}"))
        })?;

        // CF Containers API response: { "result": { "id": "...", "status": "..." }, "success": true }
        let container_id = data["result"]["id"]
            .as_str()
            .ok_or_else(|| ThalaError::backend("cloudflare", "No container ID in response"))?
            .to_string();

        tracing::info!(
            task_id = %req.task_id,
            container_id = %container_id,
            branch = %remote_branch,
            "Cloudflare worker spawned"
        );

        Ok(LaunchedRun {
            handle: WorkerHandle {
                job_id: container_id,
                backend: ExecutionBackendKind::Cloudflare,
            },
            worktree_path: None,
            remote_branch: Some(remote_branch.to_string()),
        })
    }

    async fn observe(&self, handle: &WorkerHandle) -> Result<RunObservation, ThalaError> {
        let api_token = Self::api_token().unwrap_or_default();
        let account_id = self.account_id();

        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{account_id}/containers/instances/{}",
            handle.job_id
        );

        let resp = self
            .http
            .get(&url)
            .bearer_auth(&api_token)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                ThalaError::backend("cloudflare", format!("CF status poll failed: {e}"))
            })?;

        if !resp.status().is_success() {
            // Container may have been deleted.
            return Ok(RunObservation {
                cursor: "deleted".into(),
                is_alive: false,
                observed_at: chrono::Utc::now(),
            });
        }

        let data: serde_json::Value = resp.json().await.map_err(|e| {
            ThalaError::backend("cloudflare", format!("CF status parse failed: {e}"))
        })?;

        // CF Containers API status field: "pending" | "starting" | "running" | "stopping" | "stopped" | "failed"
        let status = data["result"]["status"].as_str().unwrap_or("unknown");
        let is_alive = matches!(status, "running" | "starting" | "pending" | "stopping");

        Ok(RunObservation {
            cursor: status.to_string(), // status string changes on state changes
            is_alive,
            observed_at: chrono::Utc::now(),
        })
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), ThalaError> {
        let api_token = Self::api_token().unwrap_or_default();
        let account_id = self.account_id();

        let url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{account_id}/containers/instances/{}",
            handle.job_id
        );

        let _ = self
            .http
            .delete(&url)
            .bearer_auth(&api_token)
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
        tracing::info!(task_id, container_id = %handle.job_id, "Cloudflare cleanup complete");
        Ok(())
    }
}

fn cf_env(name: &str, value: &str) -> serde_json::Value {
    serde_json::json!({"name": name, "value": value})
}

fn base64_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}
