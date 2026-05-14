//! KubernetesBackend — one worker run per Kubernetes Pod.
//!
//! This adapter keeps Kubernetes as an execution detail behind the
//! `ExecutionBackend` port. Thala pushes a task branch, creates a Pod that runs
//! the standard worker entrypoint, watches Pod phase/container state and logs
//! for activity, and deletes the Pod on cancel/cleanup.
//!
//! Runtime state persisted in TaskRun:
//!   - job_handle.job_id: Pod name
//!   - remote_branch: branch pushed to origin before spawning
//!   - callback_token_hash: SHA-256 of the per-run bearer token

use std::fs;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, RunStatus, WorkerHandle};
use crate::ports::execution::{ExecutionBackend, LaunchRequest, LaunchedRun};

const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const DEFAULT_CONTAINER_NAME: &str = "worker";

// ── KubernetesConfig ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct KubernetesConfig {
    /// Kubernetes API server URL.
    pub api_server: String,
    /// Namespace where worker Pods are created.
    pub namespace: String,
    /// Bearer token used to call the Kubernetes API.
    pub bearer_token: String,
    /// Optional Kubernetes CA certificate path for in-cluster TLS.
    pub ca_cert_path: Option<String>,
    /// Worker image that contains `dev/docker/worker-entrypoint.sh`.
    pub worker_image: String,
    /// Kubernetes service account assigned to worker Pods.
    pub service_account_name: Option<String>,
    /// Optional image pull policy, for example `IfNotPresent` or `Always`.
    pub image_pull_policy: Option<String>,
    /// Optional secret reference for the product repo GitHub token.
    pub github_token_secret: Option<SecretKeyRef>,
    /// Extra env vars sourced from Kubernetes Secrets.
    pub secret_env: Vec<SecretEnvRef>,
    /// Best-effort Pod deletion grace period.
    pub termination_grace_period_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretKeyRef {
    pub name: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretEnvRef {
    pub env_name: String,
    pub secret: SecretKeyRef,
}

impl KubernetesConfig {
    /// Construct from environment variables.
    ///
    /// Required:
    ///   - `THALA_K8S_WORKER_IMAGE`
    ///
    /// In-cluster defaults:
    ///   - API server: `https://kubernetes.default.svc`
    ///   - namespace/token/CA from the mounted service account
    ///
    /// Optional:
    ///   - `THALA_K8S_API_SERVER`
    ///   - `THALA_K8S_NAMESPACE`
    ///   - `THALA_K8S_TOKEN`
    ///   - `THALA_K8S_CA_CERT`
    ///   - `THALA_K8S_SERVICE_ACCOUNT`
    ///   - `THALA_K8S_IMAGE_PULL_POLICY`
    ///   - `THALA_K8S_GITHUB_TOKEN_SECRET=secret:key`
    ///   - `THALA_K8S_SECRET_ENV=OPENROUTER_API_KEY=llm-secrets:openrouter,...`
    ///   - `THALA_K8S_TERMINATION_GRACE_SECONDS`
    pub fn from_env() -> Self {
        let namespace = std::env::var("THALA_K8S_NAMESPACE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| read_to_string_trimmed(&format!("{SERVICE_ACCOUNT_DIR}/namespace")).ok())
            .unwrap_or_else(|| "default".into());

        let bearer_token = std::env::var("THALA_K8S_TOKEN")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| read_to_string_trimmed(&format!("{SERVICE_ACCOUNT_DIR}/token")).ok())
            .unwrap_or_default();

        let ca_cert_path = std::env::var("THALA_K8S_CA_CERT")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                let path = format!("{SERVICE_ACCOUNT_DIR}/ca.crt");
                Path::new(&path).exists().then_some(path)
            });

        Self {
            api_server: std::env::var("THALA_K8S_API_SERVER")
                .unwrap_or_else(|_| "https://kubernetes.default.svc".into()),
            namespace,
            bearer_token,
            ca_cert_path,
            worker_image: std::env::var("THALA_K8S_WORKER_IMAGE").unwrap_or_default(),
            service_account_name: std::env::var("THALA_K8S_SERVICE_ACCOUNT")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            image_pull_policy: std::env::var("THALA_K8S_IMAGE_PULL_POLICY")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            github_token_secret: std::env::var("THALA_K8S_GITHUB_TOKEN_SECRET")
                .ok()
                .and_then(|v| parse_secret_key_ref(&v).ok()),
            secret_env: std::env::var("THALA_K8S_SECRET_ENV")
                .ok()
                .map(|v| parse_secret_env_refs(&v))
                .unwrap_or_default(),
            termination_grace_period_seconds: std::env::var("THALA_K8S_TERMINATION_GRACE_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
        }
    }
}

// ── Kubernetes API Shapes ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub name: String,
    #[serde(default)]
    pub uid: Option<String>,
    #[serde(default)]
    pub labels: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodResponse {
    pub metadata: ObjectMeta,
    #[serde(default)]
    pub status: Option<PodStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodStatus {
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default, rename = "containerStatuses")]
    pub container_statuses: Vec<ContainerStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerStatus {
    pub name: String,
    #[serde(default)]
    pub state: Option<ContainerState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerState {
    #[serde(default)]
    pub waiting: Option<ContainerStateWaiting>,
    #[serde(default)]
    pub running: Option<Map<String, Value>>,
    #[serde(default)]
    pub terminated: Option<ContainerStateTerminated>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerStateWaiting {
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerStateTerminated {
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct KubernetesStatus {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

// ── KubernetesBackend ────────────────────────────────────────────────────────

pub struct KubernetesBackend {
    config: KubernetesConfig,
    http: reqwest::Client,
}

impl KubernetesBackend {
    pub fn new(config: KubernetesConfig) -> Self {
        Self {
            http: client_from_config(&config),
            config,
        }
    }

    pub fn from_env() -> Self {
        Self::new(KubernetesConfig::from_env())
    }

    pub fn with_client(config: KubernetesConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

    pub fn pod_name(product: &str, task_id: &str, attempt: u32) -> String {
        let raw = format!("thala-{product}-{task_id}-{attempt}");
        sanitize_dns_label(&raw)
    }

    pub fn build_pod(&self, req: &LaunchRequest) -> Result<Value, ThalaError> {
        self.validate_config()?;

        let remote_branch = req.remote_branch.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "kubernetes",
                "remote_branch is required for Kubernetes backend",
            )
        })?;
        let callback_url = req.callback_url.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "kubernetes",
                "callback_url is required for Kubernetes backend",
            )
        })?;
        let callback_token = req.callback_token.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "kubernetes",
                "callback_token is required for Kubernetes backend",
            )
        })?;
        let github_repo = req.github_repo.as_deref().ok_or_else(|| {
            ThalaError::backend(
                "kubernetes",
                "github_repo is required for Kubernetes backend",
            )
        })?;

        let pod_name = Self::pod_name(&req.product, &req.task_id, req.attempt);
        let mut env = vec![
            env_value("THALA_RUN_ID", &req.run_id),
            env_value("THALA_TASK_ID", &req.task_id),
            env_value("THALA_TASK_BRANCH", remote_branch),
            env_value("THALA_GITHUB_REPO", github_repo),
            env_value("THALA_CALLBACK_URL", callback_url),
            env_value("THALA_RUN_TOKEN", callback_token),
            env_value("THALA_MODEL", &req.model),
            env_value("THALA_PROMPT_B64", &base64_encode(&req.prompt)),
        ];

        push_optional_env(
            &mut env,
            "THALA_AFTER_CREATE_HOOK",
            req.after_create_hook.as_ref(),
        );
        push_optional_env(
            &mut env,
            "THALA_BEFORE_RUN_HOOK",
            req.before_run_hook.as_ref(),
        );
        push_optional_env(
            &mut env,
            "THALA_AFTER_RUN_HOOK",
            req.after_run_hook.as_ref(),
        );

        if let Some(secret) = &self.config.github_token_secret {
            env.push(env_secret("GITHUB_TOKEN", secret));
        } else if let Some(token) = &req.github_token {
            env.push(env_value("GITHUB_TOKEN", token));
        }

        for secret_env in &self.config.secret_env {
            env.push(env_secret(&secret_env.env_name, &secret_env.secret));
        }

        let mut container = json!({
            "name": DEFAULT_CONTAINER_NAME,
            "image": self.config.worker_image,
            "env": env,
        });
        if let Some(policy) = &self.config.image_pull_policy {
            container["imagePullPolicy"] = json!(policy);
        }

        let mut spec = json!({
            "restartPolicy": "Never",
            "terminationGracePeriodSeconds": self.config.termination_grace_period_seconds,
            "containers": [container],
        });
        if let Some(service_account) = &self.config.service_account_name {
            spec["serviceAccountName"] = json!(service_account);
        }

        Ok(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": pod_name,
                "labels": {
                    "app.kubernetes.io/name": "thala-worker",
                    "app.kubernetes.io/managed-by": "thala",
                    "thala.dev/product": sanitize_label_value(&req.product),
                    "thala.dev/task": sanitize_label_value(&req.task_id),
                    "thala.dev/run": sanitize_label_value(&req.run_id),
                }
            },
            "spec": spec,
        }))
    }

    async fn create_pod(&self, pod: &Value) -> Result<PodResponse, ThalaError> {
        self.post_json(&format!("/api/v1/namespaces/{}/pods", self.ns()), pod)
            .await
    }

    async fn read_pod(&self, pod_name: &str) -> Result<PodResponse, ThalaError> {
        self.get_json(&format!(
            "/api/v1/namespaces/{}/pods/{}",
            self.ns(),
            path_segment(pod_name)
        ))
        .await
    }

    async fn read_logs(&self, pod_name: &str) -> Result<String, ThalaError> {
        self.get_text(&format!(
            "/api/v1/namespaces/{}/pods/{}/log?container={}&tailLines=100",
            self.ns(),
            path_segment(pod_name),
            DEFAULT_CONTAINER_NAME,
        ))
        .await
    }

    async fn delete_pod(&self, pod_name: &str) -> Result<(), ThalaError> {
        let body = json!({
            "kind": "DeleteOptions",
            "apiVersion": "v1",
            "gracePeriodSeconds": self.config.termination_grace_period_seconds,
        });
        let _: Value = self
            .delete_json(
                &format!(
                    "/api/v1/namespaces/{}/pods/{}",
                    self.ns(),
                    path_segment(pod_name)
                ),
                &body,
            )
            .await?;
        Ok(())
    }

    fn validate_config(&self) -> Result<(), ThalaError> {
        if self.config.api_server.trim().is_empty() {
            return Err(ThalaError::backend(
                "kubernetes",
                "THALA_K8S_API_SERVER is not set",
            ));
        }
        if self.config.namespace.trim().is_empty() {
            return Err(ThalaError::backend(
                "kubernetes",
                "THALA_K8S_NAMESPACE is not set",
            ));
        }
        if self.config.bearer_token.trim().is_empty() {
            return Err(ThalaError::backend(
                "kubernetes",
                "Kubernetes bearer token is not set; run in-cluster or set THALA_K8S_TOKEN",
            ));
        }
        if self.config.worker_image.trim().is_empty() {
            return Err(ThalaError::backend(
                "kubernetes",
                "THALA_K8S_WORKER_IMAGE is not set",
            ));
        }
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.config.api_server.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn ns(&self) -> String {
        path_segment(&self.config.namespace)
    }

    async fn get_json<T>(&self, path: &str) -> Result<T, ThalaError>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.validate_config()?;
        let resp = self
            .http
            .get(self.endpoint(path))
            .bearer_auth(&self.config.bearer_token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ThalaError::backend("kubernetes", format!("API request failed: {e}")))?;
        parse_json_response(resp).await
    }

    async fn get_text(&self, path: &str) -> Result<String, ThalaError> {
        self.validate_config()?;
        let resp = self
            .http
            .get(self.endpoint(path))
            .bearer_auth(&self.config.bearer_token)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ThalaError::backend("kubernetes", format!("API request failed: {e}")))?;
        parse_text_response(resp).await
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
            .bearer_auth(&self.config.bearer_token)
            .json(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ThalaError::backend("kubernetes", format!("API request failed: {e}")))?;
        parse_json_response(resp).await
    }

    async fn delete_json<B, T>(&self, path: &str, body: &B) -> Result<T, ThalaError>
    where
        B: Serialize + ?Sized,
        T: for<'de> Deserialize<'de>,
    {
        self.validate_config()?;
        let resp = self
            .http
            .delete(self.endpoint(path))
            .bearer_auth(&self.config.bearer_token)
            .json(body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| ThalaError::backend("kubernetes", format!("API request failed: {e}")))?;
        parse_json_response(resp).await
    }
}

#[async_trait]
impl ExecutionBackend for KubernetesBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Kubernetes
    }

    fn is_local(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "kubernetes"
    }

    async fn launch(&self, req: LaunchRequest) -> Result<LaunchedRun, ThalaError> {
        let pod_name = Self::pod_name(&req.product, &req.task_id, req.attempt);
        let remote_branch = req.remote_branch.clone().ok_or_else(|| {
            ThalaError::backend(
                "kubernetes",
                "remote_branch is required for Kubernetes backend",
            )
        })?;

        let pod = self.build_pod(&req)?;
        let created = self.create_pod(&pod).await?;

        tracing::info!(
            task_id = %req.task_id,
            pod = %created.metadata.name,
            branch = %remote_branch,
            namespace = %self.config.namespace,
            "Kubernetes worker Pod created"
        );

        Ok(LaunchedRun {
            handle: WorkerHandle {
                job_id: pod_name,
                backend: ExecutionBackendKind::Kubernetes,
            },
            worktree_path: None,
            remote_branch: Some(remote_branch),
        })
    }

    async fn observe(
        &self,
        handle: &WorkerHandle,
        _prev_cursor: Option<&str>,
    ) -> Result<RunObservation, ThalaError> {
        let pod = self.read_pod(&handle.job_id).await?;
        let logs = self.read_logs(&handle.job_id).await.unwrap_or_default();
        let state = pod_state(&pod);
        let cursor = format!(
            "{}:{}",
            state.cursor_part,
            hex::encode(Sha256::digest(logs.as_bytes()))
        );

        Ok(RunObservation {
            cursor,
            is_alive: state.is_alive,
            terminal_status: state.terminal_status,
            observed_at: Utc::now(),
        })
    }

    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), ThalaError> {
        match self.delete_pod(&handle.job_id).await {
            Ok(()) => Ok(()),
            Err(e) if e.to_string().contains("404") => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn cleanup(
        &self,
        handle: &WorkerHandle,
        _workspace_root: &Path,
        task_id: &str,
    ) -> Result<(), ThalaError> {
        self.cancel(handle).await?;
        tracing::info!(task_id, pod = %handle.job_id, "Kubernetes cleanup complete");
        Ok(())
    }
}

struct PodRunState {
    cursor_part: String,
    is_alive: bool,
    terminal_status: Option<RunStatus>,
}

fn pod_state(pod: &PodResponse) -> PodRunState {
    let phase = pod
        .status
        .as_ref()
        .and_then(|s| s.phase.as_deref())
        .unwrap_or("Unknown");

    let worker_state = pod.status.as_ref().and_then(|s| {
        s.container_statuses
            .iter()
            .find(|c| c.name == DEFAULT_CONTAINER_NAME)
            .and_then(|c| c.state.as_ref())
    });

    if let Some(terminated) = worker_state.and_then(|s| s.terminated.as_ref()) {
        return PodRunState {
            cursor_part: format!(
                "terminated:{}:{}",
                terminated.exit_code,
                terminated.reason.clone().unwrap_or_default()
            ),
            is_alive: false,
            terminal_status: Some(if terminated.exit_code == 0 {
                RunStatus::Completed
            } else {
                RunStatus::Failed
            }),
        };
    }

    if let Some(waiting) = worker_state.and_then(|s| s.waiting.as_ref()) {
        let reason = waiting.reason.clone().unwrap_or_default();
        let failed = matches!(
            reason.as_str(),
            "ErrImagePull" | "ImagePullBackOff" | "CreateContainerConfigError" | "InvalidImageName"
        );
        return PodRunState {
            cursor_part: format!("waiting:{reason}"),
            is_alive: !failed,
            terminal_status: failed.then_some(RunStatus::Failed),
        };
    }

    match phase {
        "Succeeded" => PodRunState {
            cursor_part: "phase:Succeeded".into(),
            is_alive: false,
            terminal_status: Some(RunStatus::Completed),
        },
        "Failed" => PodRunState {
            cursor_part: "phase:Failed".into(),
            is_alive: false,
            terminal_status: Some(RunStatus::Failed),
        },
        _ => PodRunState {
            cursor_part: format!("phase:{phase}"),
            is_alive: true,
            terminal_status: None,
        },
    }
}

async fn parse_json_response<T>(resp: reqwest::Response) -> Result<T, ThalaError>
where
    T: for<'de> Deserialize<'de>,
{
    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        ThalaError::backend("kubernetes", format!("Kubernetes body read failed: {e}"))
    })?;

    if !status.is_success() {
        let message = serde_json::from_str::<KubernetesStatus>(&text)
            .ok()
            .and_then(|s| s.message.or(s.reason))
            .unwrap_or_else(|| text.trim().to_string());
        return Err(ThalaError::backend(
            "kubernetes",
            format!("Kubernetes API returned {status}: {message}"),
        ));
    }

    serde_json::from_str(&text).map_err(|e| {
        ThalaError::backend("kubernetes", format!("Kubernetes JSON parse failed: {e}"))
    })
}

async fn parse_text_response(resp: reqwest::Response) -> Result<String, ThalaError> {
    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        ThalaError::backend("kubernetes", format!("Kubernetes body read failed: {e}"))
    })?;

    if !status.is_success() {
        return Err(ThalaError::backend(
            "kubernetes",
            format!("Kubernetes API returned {status}: {}", text.trim()),
        ));
    }

    Ok(text)
}

fn client_from_config(config: &KubernetesConfig) -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if let Some(ca_path) = &config.ca_cert_path {
        if let Ok(bytes) = fs::read(ca_path) {
            if let Ok(cert) = reqwest::Certificate::from_pem(&bytes) {
                builder = builder.add_root_certificate(cert);
            }
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

fn env_value(name: &str, value: &str) -> Value {
    json!({ "name": name, "value": value })
}

fn env_secret(name: &str, secret: &SecretKeyRef) -> Value {
    json!({
        "name": name,
        "valueFrom": {
            "secretKeyRef": {
                "name": secret.name,
                "key": secret.key
            }
        }
    })
}

fn push_optional_env(env: &mut Vec<Value>, name: &str, value: Option<&String>) {
    if let Some(value) = value.map(String::as_str).filter(|v| !v.trim().is_empty()) {
        env.push(env_value(name, value));
    }
}

fn parse_secret_key_ref(value: &str) -> Result<SecretKeyRef, ThalaError> {
    let (name, key) = value.split_once(':').ok_or_else(|| {
        ThalaError::backend(
            "kubernetes",
            "secret references must use the form secret-name:key",
        )
    })?;
    if name.trim().is_empty() || key.trim().is_empty() {
        return Err(ThalaError::backend(
            "kubernetes",
            "secret references must include both secret name and key",
        ));
    }
    Ok(SecretKeyRef {
        name: name.trim().into(),
        key: key.trim().into(),
    })
}

fn parse_secret_env_refs(value: &str) -> Vec<SecretEnvRef> {
    value
        .split(',')
        .filter_map(|entry| {
            let (env_name, secret_ref) = entry.split_once('=')?;
            let secret = parse_secret_key_ref(secret_ref).ok()?;
            Some(SecretEnvRef {
                env_name: env_name.trim().into(),
                secret,
            })
        })
        .filter(|entry| !entry.env_name.is_empty())
        .collect()
}

fn read_to_string_trimmed(path: &str) -> Result<String, std::io::Error> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

fn base64_encode(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn sanitize_dns_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let mut label = if trimmed.is_empty() {
        "thala-worker".to_string()
    } else {
        trimmed.to_string()
    };
    if label.len() > 63 {
        let hash = hex::encode(Sha256::digest(value.as_bytes()));
        label.truncate(52);
        label = format!("{}-{}", label.trim_end_matches('-'), &hash[..10]);
    }
    label
}

fn sanitize_label_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(63));
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
        if out.len() == 63 {
            break;
        }
    }
    out.trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> KubernetesConfig {
        KubernetesConfig {
            api_server: "http://127.0.0.1:12345".into(),
            namespace: "thala".into(),
            bearer_token: "token".into(),
            ca_cert_path: None,
            worker_image: "thala-worker:test".into(),
            service_account_name: Some("thala-worker".into()),
            image_pull_policy: Some("IfNotPresent".into()),
            github_token_secret: Some(SecretKeyRef {
                name: "git".into(),
                key: "token".into(),
            }),
            secret_env: vec![SecretEnvRef {
                env_name: "OPENROUTER_API_KEY".into(),
                secret: SecretKeyRef {
                    name: "llm".into(),
                    key: "openrouter".into(),
                },
            }],
            termination_grace_period_seconds: 5,
        }
    }

    fn launch_request() -> LaunchRequest {
        LaunchRequest {
            run_id: "run-123".into(),
            task_id: "BD-123".into(),
            attempt: 2,
            product: "demo-app".into(),
            prompt: "Do the task".into(),
            model: "opencode/test".into(),
            workspace_root: "/tmp/demo".into(),
            remote_branch: Some("task/bd-123".into()),
            callback_url: Some("https://thala.example.com/api/worker/callback".into()),
            callback_token: Some("callback-token".into()),
            github_repo: Some("org/repo".into()),
            github_token: Some("raw-token".into()),
            after_create_hook: Some("npm install".into()),
            before_run_hook: None,
            after_run_hook: None,
        }
    }

    #[test]
    fn pod_name_is_dns_label() {
        assert_eq!(
            KubernetesBackend::pod_name("Demo_App", "BD/123:work", 1),
            "thala-demo-app-bd-123-work-1"
        );
    }

    #[test]
    fn build_pod_uses_worker_contract_and_secret_refs() {
        let backend = KubernetesBackend::new(config());
        let pod = backend.build_pod(&launch_request()).unwrap();

        assert_eq!(pod["metadata"]["name"], "thala-demo-app-bd-123-2");
        assert_eq!(
            pod["spec"]["serviceAccountName"],
            Value::String("thala-worker".into())
        );
        assert_eq!(
            pod["spec"]["containers"][0]["image"],
            Value::String("thala-worker:test".into())
        );
        assert_eq!(
            pod["spec"]["containers"][0]["env"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["name"] == "GITHUB_TOKEN")
                .unwrap()["valueFrom"]["secretKeyRef"]["name"],
            Value::String("git".into())
        );
    }

    #[test]
    fn pod_state_maps_terminated_container_to_run_status() {
        let pod = PodResponse {
            metadata: ObjectMeta {
                name: "pod".into(),
                uid: None,
                labels: None,
            },
            status: Some(PodStatus {
                phase: Some("Failed".into()),
                container_statuses: vec![ContainerStatus {
                    name: DEFAULT_CONTAINER_NAME.into(),
                    state: Some(ContainerState {
                        waiting: None,
                        running: None,
                        terminated: Some(ContainerStateTerminated {
                            exit_code: 7,
                            reason: Some("Error".into()),
                            message: None,
                        }),
                    }),
                }],
            }),
        };

        let state = pod_state(&pod);
        assert!(!state.is_alive);
        assert_eq!(state.terminal_status, Some(RunStatus::Failed));
    }

    #[test]
    fn parses_secret_env_refs() {
        let refs =
            parse_secret_env_refs("OPENAI_API_KEY=llm:openai,ANTHROPIC_API_KEY=llm:anthropic");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].env_name, "OPENAI_API_KEY");
        assert_eq!(refs[1].secret.key, "anthropic");
    }
}
