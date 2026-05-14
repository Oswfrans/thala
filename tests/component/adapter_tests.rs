//! Adapter implementation tests.
//!
//! Tests for the I/O-heavy adapters: execution backends, validators, intake.

// Note: These tests use mocking where possible for external services.
// Tests requiring real credentials are in tests/live/.

use thala::adapters::execution::cloudflare::{CloudflareBackend, CloudflareConfig};
use thala::adapters::execution::kubernetes::{KubernetesBackend, KubernetesConfig, SecretKeyRef};
use thala::adapters::execution::local::LocalBackend;
use thala::adapters::execution::modal::{ModalBackend, ModalConfig};
use thala::adapters::validation::noop::NoopValidator;
use thala::adapters::validation::review_ai::ReviewAiValidator;
use thala::core::ids::{RunId, TaskId};
use thala::core::run::TaskRun;
use thala::core::run::{ExecutionBackendKind, WorkerHandle};
use thala::core::task::TaskSpec;
use thala::core::validation::ValidatorKind;
use thala::ports::execution::{ExecutionBackend, LaunchRequest};
use thala::ports::validator::Validator;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn dummy_spec(id: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id),
        title: "Test task".into(),
        acceptance_criteria: "All tests should pass".into(),
        context: String::new(),
        beads_ref: id.into(),
        model_override: None,
        always_human_review: false,
        labels: vec![],
    }
}

fn kubernetes_config(base_url: String) -> KubernetesConfig {
    KubernetesConfig {
        api_server: base_url,
        namespace: "thala".into(),
        bearer_token: "test-token".into(),
        ca_cert_path: None,
        worker_image: "ghcr.io/example/thala-worker:test".into(),
        service_account_name: Some("thala-worker".into()),
        image_pull_policy: Some("IfNotPresent".into()),
        github_token_secret: Some(SecretKeyRef {
            name: "github-token".into(),
            key: "token".into(),
        }),
        secret_env: Vec::new(),
        termination_grace_period_seconds: 5,
    }
}

fn launch_request_for_backend() -> LaunchRequest {
    LaunchRequest {
        run_id: "run-test".into(),
        task_id: "bd-test".into(),
        attempt: 1,
        product: "test-product".into(),
        prompt: "Do the task".into(),
        model: "test-model".into(),
        workspace_root: std::path::PathBuf::from("/tmp/test-product"),
        remote_branch: Some("task/bd-test".into()),
        callback_url: None,
        callback_token: Some("callback-token".into()),
        github_repo: Some("example/repo".into()),
        github_token: Some("gh-token".into()),
        after_create_hook: None,
        before_run_hook: None,
        after_run_hook: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// LocalBackend tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn local_backend_reports_correct_kind() {
    let backend = LocalBackend::new();
    assert_eq!(backend.kind(), ExecutionBackendKind::Local);
    assert!(backend.is_local());
    assert_eq!(backend.name(), "local");
}

// ─────────────────────────────────────────────────────────────────────────────
// ModalBackend placeholder tests (tests actual behavior of stubbed TODOs)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn modal_backend_reports_correct_kind() {
    let config = ModalConfig {
        app_file: "dev/infra/modal_worker.py::run_worker".into(),
        environment: None,
    };
    let backend = ModalBackend::new(config);
    assert_eq!(backend.kind(), ExecutionBackendKind::Modal);
    assert!(!backend.is_local());
    assert_eq!(backend.name(), "modal");
}

#[tokio::test]
async fn modal_backend_requires_callback_url_before_spawning_cli() {
    let config = ModalConfig {
        app_file: "dev/infra/modal_worker.py::run_worker".into(),
        environment: None,
    };
    let backend = ModalBackend::new(config);
    let err = backend
        .launch(launch_request_for_backend())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("callback_url is required"));
}

#[tokio::test]
async fn modal_backend_observe_falls_back_gracefully() {
    // When the `modal` CLI is not available, observe() falls back to returning
    // the job_id as cursor (non-changing) and is_alive = true so that a missing
    // CLI does not incorrectly mark a worker as dead.
    let config = ModalConfig {
        app_file: "dev/infra/modal_worker.py::run_worker".into(),
        environment: None,
    };
    let backend = ModalBackend::new(config);
    let handle = WorkerHandle {
        job_id: "ap-test123".into(),
        backend: ExecutionBackendKind::Modal,
    };

    let obs = backend.observe(&handle, None).await.unwrap();

    // Missing CLI, timeout, or a non-existent remote app should all return a
    // structured observation instead of panicking or hanging.
    assert!(!obs.cursor.is_empty());
    assert!(obs.observed_at.timestamp() > 0);
}

#[tokio::test]
async fn modal_backend_cancel_is_noop() {
    // The cancel() method is stubbed and just logs
    let config = ModalConfig {
        app_file: "dev/infra/modal_worker.py::run_worker".into(),
        environment: None,
    };
    let backend = ModalBackend::new(config);
    let handle = WorkerHandle {
        job_id: "ap-test456".into(),
        backend: ExecutionBackendKind::Modal,
    };

    // Should succeed even though it's not fully implemented
    let result = backend.cancel(&handle).await;
    assert!(result.is_ok());
}

// ─────────────────────────────────────────────────────────────────────────────
// CloudflareBackend tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cloudflare_backend_reports_correct_kind() {
    let config = CloudflareConfig {
        base_url: "http://localhost:8787".into(),
        auth_token: "test-token".into(),
        max_duration_seconds: 1_800,
        allow_network: true,
    };
    let backend = CloudflareBackend::new(config);
    assert_eq!(backend.kind(), ExecutionBackendKind::Cloudflare);
    assert!(!backend.is_local());
    assert_eq!(backend.name(), "cloudflare");
}

#[tokio::test]
async fn cloudflare_backend_cancel_requires_config_but_does_not_panic() {
    let config = CloudflareConfig {
        base_url: String::new(),
        auth_token: String::new(),
        max_duration_seconds: 1_800,
        allow_network: true,
    };
    let backend = CloudflareBackend::new(config);
    let handle = WorkerHandle {
        job_id: "nonexistent-container".into(),
        backend: ExecutionBackendKind::Cloudflare,
    };

    let result = backend.cancel(&handle).await;
    assert!(result.is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// CloudflareConfig::from_env tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn cloudflare_config_from_env_reads_control_plane_auth() {
    std::env::set_var("THALA_CF_BASE_URL", "http://localhost:8787");
    std::env::set_var("THALA_CF_TOKEN", "test-token");
    let config = CloudflareConfig::from_env();
    assert_eq!(config.base_url, "http://localhost:8787");
    assert_eq!(config.auth_token, "test-token");
    std::env::remove_var("THALA_CF_BASE_URL");
    std::env::remove_var("THALA_CF_TOKEN");
}

#[test]
fn cloudflare_config_from_env_parses_resource_limits() {
    std::env::set_var("THALA_CF_MAX_DURATION_SECONDS", "2400");
    std::env::set_var("THALA_CF_ALLOW_NETWORK", "false");
    let config = CloudflareConfig::from_env();
    assert_eq!(config.max_duration_seconds, 2400);
    assert!(!config.allow_network);
    std::env::remove_var("THALA_CF_MAX_DURATION_SECONDS");
    std::env::remove_var("THALA_CF_ALLOW_NETWORK");
}

// ─────────────────────────────────────────────────────────────────────────────
// KubernetesBackend tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn kubernetes_backend_reports_correct_kind() {
    let backend = KubernetesBackend::new(kubernetes_config("http://localhost".into()));
    assert_eq!(backend.kind(), ExecutionBackendKind::Kubernetes);
    assert!(!backend.is_local());
    assert_eq!(backend.name(), "kubernetes");
}

#[test]
fn kubernetes_backend_builds_pod_from_launch_request() {
    let backend = KubernetesBackend::new(kubernetes_config("http://localhost".into()));
    let mut req = launch_request_for_backend();
    req.callback_url = Some("https://thala.example.com/api/worker/callback".into());
    req.product = "demo-app".into();
    req.task_id = "BD/123".into();

    let pod = backend.build_pod(&req).unwrap();

    assert_eq!(pod["metadata"]["name"], "thala-demo-app-bd-123-1");
    assert_eq!(pod["spec"]["restartPolicy"], "Never");
    assert_eq!(
        pod["spec"]["containers"][0]["env"]
            .as_array()
            .unwrap()
            .iter()
            .find(|env| env["name"] == "GITHUB_TOKEN")
            .unwrap()["valueFrom"]["secretKeyRef"]["name"],
        "github-token"
    );
}

#[tokio::test]
async fn kubernetes_backend_launch_surfaces_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/namespaces/thala/pods"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "kind": "Status",
            "message": "pods is forbidden"
        })))
        .mount(&server)
        .await;

    let backend = KubernetesBackend::new(kubernetes_config(server.uri()));
    let mut req = launch_request_for_backend();
    req.callback_url = Some("https://thala.example.com/api/worker/callback".into());

    let err = backend.launch(req).await.unwrap_err();
    assert!(err.to_string().contains("403"));
    assert!(err.to_string().contains("pods is forbidden"));
}

#[tokio::test]
async fn kubernetes_backend_observe_rejects_malformed_pod_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/namespaces/thala/pods/thala-demo-bd-test-1"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&server)
        .await;

    let backend = KubernetesBackend::new(kubernetes_config(server.uri()));
    let handle = WorkerHandle {
        job_id: "thala-demo-bd-test-1".into(),
        backend: ExecutionBackendKind::Kubernetes,
    };

    let err = backend.observe(&handle, None).await.unwrap_err();
    assert!(err.to_string().contains("JSON parse failed"));
}

// ─────────────────────────────────────────────────────────────────────────────
// NoopValidator tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn noop_validator_always_passes() {
    let validator = NoopValidator;
    assert_eq!(validator.kind(), ValidatorKind::Noop);

    let run = TaskRun::new(
        RunId::new_v4(),
        TaskId::new("bd-test"),
        1,
        ExecutionBackendKind::Local,
    );
    let spec = dummy_spec("bd-test");

    let outcome = validator.validate(&run, &spec).await.unwrap();
    assert!(outcome.passed);
}

// ─────────────────────────────────────────────────────────────────────────────
// ReviewAiValidator stub tests
// ─────────────────────────────────────────────────────────────────────────────

/// Mutex to prevent parallel mutation of ANTHROPIC_API_KEY between tests.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn review_ai_validator_reports_correct_kind() {
    let validator = ReviewAiValidator::new("fake-api-key", "claude-opus-4");
    assert_eq!(validator.kind(), ValidatorKind::ReviewAi);
}

#[tokio::test]
async fn review_ai_validator_invalid_key_returns_error() {
    // The ReviewAiValidator now makes a real LLM call.
    // With an invalid API key the call must fail — not silently pass.
    let validator = ReviewAiValidator::new("fake-api-key", "claude-opus-4-6");

    let run = TaskRun::new(
        RunId::new_v4(),
        TaskId::new("bd-test"),
        1,
        ExecutionBackendKind::Local,
    );
    let spec = dummy_spec("bd-test");

    // With a fake key the Anthropic API returns 401 → validate() returns Err.
    let result = validator.validate(&run, &spec).await;
    assert!(
        result.is_err(),
        "Expected error with invalid API key, got: {result:?}"
    );
}

#[test]
fn review_ai_validator_from_env_requires_api_key() {
    let _guard = ENV_LOCK.lock().unwrap();

    // Save current env var state
    let original = std::env::var("ANTHROPIC_API_KEY").ok();

    // Remove the env var
    std::env::remove_var("ANTHROPIC_API_KEY");

    // Should fail when ANTHROPIC_API_KEY is not set
    let result = ReviewAiValidator::from_env("claude-opus-4");
    assert!(result.is_err());

    // Restore env var if it was set
    if let Some(val) = original {
        std::env::set_var("ANTHROPIC_API_KEY", val);
    }
}

#[test]
fn review_ai_validator_from_env_succeeds_with_key() {
    let _guard = ENV_LOCK.lock().unwrap();

    // Set a fake API key
    std::env::set_var("ANTHROPIC_API_KEY", "test-key-12345");

    let result = ReviewAiValidator::from_env("claude-opus-4");
    assert!(result.is_ok());

    let validator = result.unwrap();
    assert_eq!(validator.kind(), ValidatorKind::ReviewAi);
}
