//! CloudflareBackend — HTTP control plane backed by Worker + Durable Object.
//!
//! Thala remains the orchestrator and source of truth. This adapter only turns
//! Thala launch/observe/cancel operations into a small JSON API exposed by the
//! Cloudflare control-plane Worker in `cloudflare/control-plane`.
//!
//! Runtime state persisted in TaskRun:
//!   - job_handle.job_id: remote_run_id (`cf-<task_id>-<attempt>`)
//!   - remote_branch: branch pushed to origin before spawning

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, RunStatus, WorkerHandle};
use crate::ports::execution::{ExecutionBackend, LaunchRequest, LaunchedRun};

const DEFAULT_MAX_DURATION_SECONDS: u64 = 1_800;

// ── CloudflareConfig ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct CloudflareConfig {
    /// Worker base URL, for example `http://localhost:8787`.
    pub base_url: String,

    /// Shared bearer token for Thala -> Worker requests.
    pub auth_token: String,

    /// Maximum execution duration passed to the remote executor.
    pub max_duration_seconds: u64,

    /// Whether the remote execution policy permits network access.
    pub allow_network: bool,
}

impl CloudflareConfig {
    /// Construct from environment variables.
    ///
    /// Reads:
    ///   - `THALA_CF_BASE_URL`
    ///   - `THALA_CF_TOKEN`
    ///   - `THALA_CF_MAX_DURATION_SECONDS` (optional)
    ///   - `THALA_CF_ALLOW_NETWORK` (optional, defaults true; false is rejected until supported)
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("THALA_CF_BASE_URL").unwrap_or_default(),
            auth_token: std::env::var("THALA_CF_TOKEN").unwrap_or_default(),
            max_duration_seconds: std::env::var("THALA_CF_MAX_DURATION_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_DURATION_SECONDS),
            allow_network: std::env::var("THALA_CF_ALLOW_NETWORK")
                .map(|v| !matches!(v.as_str(), "0" | "false" | "FALSE" | "False"))
                .unwrap_or(true),
        }
    }
}

// ── Contract Types ────────────────────────────────────────────────────────────

pub type TaskId = String;
pub type AttemptNumber = u32;
pub type RemoteRunId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSpec {
    pub provider: String,
    pub owner: String,
    pub name: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSpec {
    pub prompt: String,
    pub working_dir: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_create_hook: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_run_hook: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_run_hook: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub max_duration_seconds: u64,
    pub allow_network: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartTaskRequest {
    pub task_id: TaskId,
    pub attempt: AttemptNumber,
    pub repo: RepoSpec,
    pub instruction: InstructionSpec,
    pub execution_policy: ExecutionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartTaskResponse {
    pub remote_run_id: RemoteRunId,
    pub status: RemoteTaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteTaskStatus {
    Queued,
    Booting,
    Cloning,
    Running,
    Pushing,
    Completed,
    Failed,
    Cancelled,
}

impl RemoteTaskStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    fn as_run_status(self) -> Option<RunStatus> {
        match self {
            Self::Completed => Some(RunStatus::Completed),
            Self::Failed => Some(RunStatus::Failed),
            Self::Cancelled => Some(RunStatus::Cancelled),
            Self::Queued | Self::Booting | Self::Cloning | Self::Running | Self::Pushing => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStatusResponse {
    pub remote_run_id: RemoteRunId,
    pub status: RemoteTaskStatus,
    pub phase: RemoteTaskStatus,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub index: u64,
    pub ts: DateTime<Utc>,
    pub stream: LogStream,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsResponse {
    pub remote_run_id: RemoteRunId,
    pub lines: Vec<LogLine>,
    pub next_cursor: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub commit_sha: Option<String>,
    pub branch: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResultResponse {
    pub remote_run_id: RemoteRunId,
    pub status: RemoteTaskStatus,
    #[serde(default)]
    pub result: Option<TaskResult>,
    #[serde(default)]
    pub error: Option<TaskError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelTaskResponse {
    pub remote_run_id: RemoteRunId,
    pub status: RemoteTaskStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkerErrorResponse {
    error: TaskError,
}

// ── CloudflareBackend ─────────────────────────────────────────────────────────

pub struct CloudflareBackend {
    config: CloudflareConfig,
    http: reqwest::Client,
}

impl CloudflareBackend {
    pub fn new(config: CloudflareConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(CloudflareConfig::from_env())
    }

    #[cfg(test)]
    fn with_client(config: CloudflareConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

    pub async fn start_task(
        &self,
        request: &StartTaskRequest,
    ) -> Result<StartTaskResponse, ThalaError> {
        self.post_json("/tasks/start", request).await
    }

    pub async fn task_status(&self, remote_run_id: &str) -> Result<TaskStatusResponse, ThalaError> {
        self.get_json(&format!("/tasks/{}/status", path_segment(remote_run_id)))
            .await
    }

    pub async fn task_logs(
        &self,
        remote_run_id: &str,
        cursor: Option<u64>,
    ) -> Result<LogsResponse, ThalaError> {
        let path = cursor.map_or_else(
            || format!("/tasks/{}/logs", path_segment(remote_run_id)),
            |c| format!("/tasks/{}/logs?cursor={c}", path_segment(remote_run_id)),
        );
        self.get_json(&path).await
    }

    pub async fn cancel_task(&self, remote_run_id: &str) -> Result<CancelTaskResponse, ThalaError> {
        self.post_json::<(), _>(
            &format!("/tasks/{}/cancel", path_segment(remote_run_id)),
            &(),
        )
        .await
    }

    pub async fn task_result(&self, remote_run_id: &str) -> Result<TaskResultResponse, ThalaError> {
        self.get_json(&format!("/tasks/{}/result", path_segment(remote_run_id)))
            .await
    }

    fn validate_config(&self) -> Result<(), ThalaError> {
        if self.config.base_url.trim().is_empty() {
            return Err(ThalaError::backend(
                "cloudflare",
                "THALA_CF_BASE_URL is not set",
            ));
        }
        if self.config.auth_token.trim().is_empty() {
            return Err(ThalaError::backend(
                "cloudflare",
                "THALA_CF_TOKEN is not set",
            ));
        }
        if self.config.max_duration_seconds == 0 {
            return Err(ThalaError::backend(
                "cloudflare",
                "THALA_CF_MAX_DURATION_SECONDS must be greater than zero",
            ));
        }
        if !self.config.allow_network {
            return Err(ThalaError::backend(
                "cloudflare",
                "THALA_CF_ALLOW_NETWORK=false is not supported by the Cloudflare Sandbox backend yet",
            ));
        }
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn get_json<T>(&self, path: &str) -> Result<T, ThalaError>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.validate_config()?;
        let resp = self
            .http
            .get(self.endpoint(path))
            .bearer_auth(&self.config.auth_token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                ThalaError::backend("cloudflare", format!("Worker request failed: {e}"))
            })?;

        parse_response(resp).await
    }

    async fn post_json<B, T>(&self, path: &str, body: &B) -> Result<T, ThalaError>
    where
        B: Serialize + ?Sized,
        T: for<'de> Deserialize<'de>,
    {
        self.validate_config()?;
        let resp = self
            .http
            .post(self.endpoint(path))
            .bearer_auth(&self.config.auth_token)
            .json(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                ThalaError::backend("cloudflare", format!("Worker request failed: {e}"))
            })?;

        parse_response(resp).await
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
        let github_repo = req.github_repo.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "cloudflare",
                "github_repo is required for Cloudflare backend",
            )
        })?;

        let (owner, name) = parse_github_repo(github_repo)?;
        let start = StartTaskRequest {
            task_id: req.task_id.clone(),
            attempt: req.attempt,
            repo: RepoSpec {
                provider: "github".into(),
                owner,
                name,
                branch: remote_branch.to_string(),
            },
            instruction: InstructionSpec {
                prompt: req.prompt,
                working_dir: ".".into(),
                model: req.model,
                after_create_hook: req.after_create_hook,
                before_run_hook: req.before_run_hook,
                after_run_hook: req.after_run_hook,
            },
            execution_policy: ExecutionPolicy {
                max_duration_seconds: self.config.max_duration_seconds,
                allow_network: self.config.allow_network,
            },
        };

        let launched = self.start_task(&start).await?;

        tracing::info!(
            task_id = %start.task_id,
            remote_run_id = %launched.remote_run_id,
            branch = %remote_branch,
            "Cloudflare control-plane task started"
        );

        Ok(LaunchedRun {
            handle: WorkerHandle {
                job_id: launched.remote_run_id,
                backend: ExecutionBackendKind::Cloudflare,
            },
            worktree_path: None,
            remote_branch: Some(remote_branch.to_string()),
        })
    }

    async fn observe(
        &self,
        handle: &WorkerHandle,
        prev_cursor: Option<&str>,
    ) -> Result<RunObservation, ThalaError> {
        let status = self.task_status(&handle.job_id).await?;
        let log_cursor = prev_cursor.and_then(parse_log_cursor);
        let logs = self.task_logs(&handle.job_id, log_cursor).await?;
        let terminal_status = status.status.as_run_status();

        Ok(RunObservation {
            cursor: format!(
                "{}:{}:{}",
                status_string(status.status),
                logs.next_cursor,
                status.updated_at.timestamp_millis()
            ),
            is_alive: !status.status.is_terminal(),
            terminal_status,
            observed_at: chrono::Utc::now(),
        })
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), ThalaError> {
        let _ = self.cancel_task(&handle.job_id).await?;
        Ok(())
    }

    async fn cleanup(
        &self,
        handle: &WorkerHandle,
        _workspace_root: &Path,
        task_id: &str,
    ) -> Result<(), ThalaError> {
        self.cancel(handle).await?;
        tracing::info!(task_id, remote_run_id = %handle.job_id, "Cloudflare cleanup complete");
        Ok(())
    }
}

async fn parse_response<T>(resp: reqwest::Response) -> Result<T, ThalaError>
where
    T: for<'de> Deserialize<'de>,
{
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| ThalaError::backend("cloudflare", format!("Worker body read failed: {e}")))?;

    if !status.is_success() {
        let message = serde_json::from_str::<WorkerErrorResponse>(&text)
            .map_or_else(|_| text.trim().to_string(), |e| e.error.message);
        return Err(ThalaError::backend(
            "cloudflare",
            format!("Worker returned {status}: {message}"),
        ));
    }

    serde_json::from_str(&text)
        .map_err(|e| ThalaError::backend("cloudflare", format!("Worker JSON parse failed: {e}")))
}

fn path_segment(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn parse_github_repo(repo: &str) -> Result<(String, String), ThalaError> {
    let (owner, name) = repo.split_once('/').ok_or_else(|| {
        ThalaError::backend(
            "cloudflare",
            "github_repo must be an owner/name slug for Cloudflare backend",
        )
    })?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(ThalaError::backend(
            "cloudflare",
            "github_repo must be an owner/name slug for Cloudflare backend",
        ));
    }
    Ok((owner.to_string(), name.to_string()))
}

/// Extract the log cursor from a previously-returned observation cursor string.
///
/// Cursor format: "<status>:<log_cursor>:<timestamp_ms>"
/// Returns the log cursor (second field) so incremental log fetches resume where
/// they left off rather than re-fetching all lines from 0 on every poll tick.
fn parse_log_cursor(cursor: &str) -> Option<u64> {
    cursor.split(':').nth(1)?.parse().ok()
}

fn status_string(status: RemoteTaskStatus) -> &'static str {
    match status {
        RemoteTaskStatus::Queued => "queued",
        RemoteTaskStatus::Booting => "booting",
        RemoteTaskStatus::Cloning => "cloning",
        RemoteTaskStatus::Running => "running",
        RemoteTaskStatus::Pushing => "pushing",
        RemoteTaskStatus::Completed => "completed",
        RemoteTaskStatus::Failed => "failed",
        RemoteTaskStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::execution::ExecutionBackend;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config(base_url: String) -> CloudflareConfig {
        CloudflareConfig {
            base_url,
            auth_token: "test-token".into(),
            max_duration_seconds: 90,
            allow_network: true,
        }
    }

    fn sample_start() -> StartTaskRequest {
        StartTaskRequest {
            task_id: "bd-123".into(),
            attempt: 2,
            repo: RepoSpec {
                provider: "github".into(),
                owner: "Oswfrans".into(),
                name: "thala".into(),
                branch: "task/bd-123".into(),
            },
            instruction: InstructionSpec {
                prompt: "Do the task".into(),
                working_dir: ".".into(),
                model: "worker-model".into(),
                after_create_hook: None,
                before_run_hook: None,
                after_run_hook: None,
            },
            execution_policy: ExecutionPolicy {
                max_duration_seconds: 90,
                allow_network: true,
            },
        }
    }

    #[test]
    fn remote_task_status_serializes_lowercase() {
        let encoded = serde_json::to_string(&RemoteTaskStatus::Booting).unwrap();
        assert_eq!(encoded, "\"booting\"");
        let decoded: RemoteTaskStatus = serde_json::from_str("\"completed\"").unwrap();
        assert_eq!(decoded, RemoteTaskStatus::Completed);
    }

    #[test]
    fn start_task_request_round_trips() {
        let request = sample_start();
        let json = serde_json::to_string(&request).unwrap();
        let decoded: StartTaskRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn client_sends_auth_and_parses_start_response() {
        let server = MockServer::start().await;
        let backend = CloudflareBackend::with_client(config(server.uri()), reqwest::Client::new());
        let request = sample_start();

        Mock::given(method("POST"))
            .and(path("/tasks/start"))
            .and(header("authorization", "Bearer test-token"))
            .and(body_json(&request))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "remote_run_id": "cf-bd-123-2",
                "status": "queued"
            })))
            .mount(&server)
            .await;

        let response = backend.start_task(&request).await.unwrap();
        assert_eq!(response.remote_run_id, "cf-bd-123-2");
        assert_eq!(response.status, RemoteTaskStatus::Queued);
    }

    #[tokio::test]
    async fn client_returns_worker_error_without_token_leak() {
        let server = MockServer::start().await;
        let backend = CloudflareBackend::with_client(config(server.uri()), reqwest::Client::new());

        Mock::given(method("GET"))
            .and(path("/tasks/cf-bd-404-1/status"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": {
                    "code": "not_found",
                    "message": "task not found"
                }
            })))
            .mount(&server)
            .await;

        let err = backend.task_status("cf-bd-404-1").await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("task not found"));
        assert!(!message.contains("test-token"));
    }

    #[tokio::test]
    async fn client_reports_malformed_json() {
        let server = MockServer::start().await;
        let backend = CloudflareBackend::with_client(config(server.uri()), reqwest::Client::new());

        Mock::given(method("GET"))
            .and(path("/tasks/cf-bd-1-1/status"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{bad json"))
            .mount(&server)
            .await;

        let err = backend.task_status("cf-bd-1-1").await.unwrap_err();
        assert!(err.to_string().contains("Worker JSON parse failed"));
    }

    #[tokio::test]
    async fn client_url_encodes_remote_run_ids() {
        let server = MockServer::start().await;
        let backend = CloudflareBackend::with_client(config(server.uri()), reqwest::Client::new());

        Mock::given(method("GET"))
            .and(path("/tasks/cf-feature%2Fslash-1/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "remote_run_id": "cf-feature/slash-1",
                "status": "running",
                "phase": "running",
                "updated_at": "2026-04-18T00:00:00Z"
            })))
            .mount(&server)
            .await;

        let response = backend.task_status("cf-feature/slash-1").await.unwrap();
        assert_eq!(response.remote_run_id, "cf-feature/slash-1");
    }

    #[tokio::test]
    async fn launch_builds_start_request_from_launch_request() {
        let server = MockServer::start().await;
        let backend = CloudflareBackend::with_client(config(server.uri()), reqwest::Client::new());

        Mock::given(method("POST"))
            .and(path("/tasks/start"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "remote_run_id": "cf-bd-123-3",
                "status": "queued"
            })))
            .mount(&server)
            .await;

        let launched = backend
            .launch(LaunchRequest {
                run_id: "run-1".into(),
                task_id: "bd-123".into(),
                attempt: 3,
                product: "thala-core".into(),
                prompt: "Prompt".into(),
                model: "worker-model".into(),
                workspace_root: ".".into(),
                remote_branch: Some("task/bd-123".into()),
                callback_url: None,
                callback_token: None,
                github_repo: Some("Oswfrans/thala".into()),
                github_token: None,
                after_create_hook: None,
                before_run_hook: None,
                after_run_hook: None,
            })
            .await
            .unwrap();

        assert_eq!(launched.handle.job_id, "cf-bd-123-3");
        assert_eq!(launched.handle.backend, ExecutionBackendKind::Cloudflare);
        assert_eq!(launched.remote_branch.as_deref(), Some("task/bd-123"));
    }
}
