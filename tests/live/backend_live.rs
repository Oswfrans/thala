//! Live tests for execution backends.
//!
//! These tests require valid credentials and actual external services.
//! Run with: cargo test --test test_live -- --ignored

use std::path::PathBuf;
use thala::adapters::execution::cloudflare::{CloudflareBackend, CloudflareConfig};
use thala::adapters::execution::modal::{ModalBackend, ModalConfig};
use thala::core::run::ExecutionBackendKind;
use thala::ports::execution::{ExecutionBackend, LaunchRequest};

// ─────────────────────────────────────────────────────────────────────────────
// ModalBackend live tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Modal CLI authentication"]
async fn modal_backend_live_launch_and_observe() {
    // This test requires:
    // - modal CLI installed and authenticated
    // - A Modal app named "thala_worker" deployed
    let config = ModalConfig {
        app_file: "dev/infra/modal_worker.py::run_worker".into(),
        environment: None,
    };
    let backend = ModalBackend::new(config);

    let req = LaunchRequest {
        run_id: "test-run-001".into(),
        task_id: "bd-test-001".into(),
        product: "test-app".into(),
        prompt: "Test prompt".into(),
        model: "claude-sonnet".into(),
        workspace_root: PathBuf::from("/tmp/test-workspace"),
        remote_branch: Some("test-branch".into()),
        callback_url: Some("http://localhost:8080/callback".into()),
        callback_token: Some("secret-token".into()),
        github_repo: Some("owner/repo".into()),
        github_token: Some("fake-token".into()),
        after_create_hook: None,
        before_run_hook: None,
        after_run_hook: None,
    };

    // This will actually call `modal run`
    let launched = backend.launch(req).await.unwrap();
    assert_eq!(launched.handle.backend, ExecutionBackendKind::Modal);

    // Observe should return real cursor from modal logs
    let obs = backend.observe(&launched.handle).await.unwrap();
    // Real implementation should have meaningful cursor
    assert!(!obs.cursor.is_empty());

    // Cleanup
    backend.cancel(&launched.handle).await.unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// CloudflareBackend live tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Cloudflare API credentials"]
async fn cloudflare_backend_live_launch_observe_cancel() {
    // This test requires:
    // - CF_API_TOKEN environment variable set
    // - CF_ACCOUNT_ID environment variable set
    // - A valid container image pushed to a registry

    let config = CloudflareConfig {
        account_id: std::env::var("CF_ACCOUNT_ID").unwrap_or_default(),
        image: "docker.io/yourorg/thala-worker:latest".into(),
        vcpus: Some(1),
        memory_mb: Some(512),
    };
    let backend = CloudflareBackend::new(config);

    let req = LaunchRequest {
        run_id: "test-run-002".into(),
        task_id: "bd-test-002".into(),
        product: "test-app".into(),
        prompt: "Test prompt for Cloudflare".into(),
        model: "claude-sonnet".into(),
        workspace_root: PathBuf::from("/tmp/test-workspace"),
        remote_branch: Some("cf-test-branch".into()),
        callback_url: Some("http://localhost:8080/callback".into()),
        callback_token: Some("secret-token".into()),
        github_repo: Some("owner/repo".into()),
        github_token: Some(std::env::var("GITHUB_TOKEN").unwrap_or_default()),
        after_create_hook: None,
        before_run_hook: None,
        after_run_hook: None,
    };

    // Launch container
    let launched = backend.launch(req).await.unwrap();
    assert_eq!(launched.handle.backend, ExecutionBackendKind::Cloudflare);
    assert!(!launched.handle.job_id.is_empty());

    // Observe container status
    let obs = backend.observe(&launched.handle).await.unwrap();
    // Should be running, starting, or pending
    assert!(obs.is_alive || obs.cursor == "deleted");

    // Cancel container
    backend.cancel(&launched.handle).await.unwrap();

    // Verify it's gone
    let obs = backend.observe(&launched.handle).await.unwrap();
    assert!(!obs.is_alive);
}
