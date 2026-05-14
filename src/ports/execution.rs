//! ExecutionBackend port — launch, observe, and cancel worker runs.
//!
//! All execution backends implement this trait.
//! Backend-specific types (tmux session details, Modal API shapes,
//! Cloudflare container specs) must not appear in this interface.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::core::error::ThalaError;
use crate::core::run::{ExecutionBackendKind, RunObservation, WorkerHandle};

// ── LaunchRequest ─────────────────────────────────────────────────────────────

/// Everything a backend needs to start a worker run.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    /// Thala's run identifier (for logging and callback routing).
    pub run_id: String,

    /// Human-readable task identifier (for naming worktrees/sessions).
    pub task_id: String,

    /// One-based attempt number for this task.
    pub attempt: u32,

    /// Product/repo name (e.g. "example-app").
    pub product: String,

    /// Fully rendered prompt text delivered to the worker.
    pub prompt: String,

    /// Model string passed to the worker (e.g. "openrouter/moonshotai/kimi-k2.5").
    pub model: String,

    /// Absolute path to the workspace root on the Thala host.
    pub workspace_root: PathBuf,

    // ── Remote-backend fields ─────────────────────────────────────────────────
    /// Branch pre-pushed to origin for remote backends to clone.
    /// None for the local backend (worktree is created locally).
    pub remote_branch: Option<String>,

    /// Callback URL for remote backends to POST completion/failure signals.
    /// Format: "https://thala.example.com/api/worker/callback"
    /// None for the local backend (uses signal files instead).
    pub callback_url: Option<String>,

    /// Per-run bearer token for authenticating callback POSTs.
    /// The raw token is passed to the worker; Thala stores only the hash.
    pub callback_token: Option<String>,

    /// GitHub repo slug ("org/repo") for remote backends.
    pub github_repo: Option<String>,

    /// GitHub token for remote branch operations.
    pub github_token: Option<String>,

    // ── Lifecycle hooks forwarded to remote workers ───────────────────────────
    /// Trusted WORKFLOW.md shell snippet run after the worktree/clone is ready,
    /// before OpenCode starts. Non-zero exits fail the run.
    pub after_create_hook: Option<String>,

    /// Trusted WORKFLOW.md shell snippet run in the worktree before OpenCode
    /// starts. Non-zero exits fail the run.
    pub before_run_hook: Option<String>,

    /// Trusted WORKFLOW.md shell snippet run after OpenCode exits, before the
    /// callback is sent. Non-zero exits fail the run.
    pub after_run_hook: Option<String>,
}

// ── LaunchedRun ───────────────────────────────────────────────────────────────

/// What the backend returns after successfully launching a worker.
#[derive(Debug)]
pub struct LaunchedRun {
    /// Opaque backend-specific job handle.
    pub handle: WorkerHandle,

    /// Absolute path to the local git worktree.
    /// Some for LocalBackend; None for remote backends.
    pub worktree_path: Option<PathBuf>,

    /// Branch name (for remote backends that create or receive a branch).
    pub remote_branch: Option<String>,
}

// ── ExecutionBackend ──────────────────────────────────────────────────────────

/// Abstraction over local and remote worker execution environments.
///
/// Implementations include LocalBackend, ModalBackend, and CloudflareBackend.
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Which kind of backend this is.
    fn kind(&self) -> ExecutionBackendKind;

    /// Whether this backend creates a local worktree on the Thala host.
    /// When false, the dispatcher pushes a per-run task branch before spawning.
    fn is_local(&self) -> bool;

    /// Human-readable name for logs and notifications.
    fn name(&self) -> &'static str;

    /// Launch a worker run. Returns a handle and optional worktree path.
    ///
    /// On success the run is underway; the caller should update the TaskRun
    /// record with the returned handle and transition the run to Active
    /// once the first observation confirms the worker is alive.
    async fn launch(&self, req: LaunchRequest) -> Result<LaunchedRun, ThalaError>;

    /// Poll recent output for activity detection.
    ///
    /// The monitor compares the returned cursor between ticks.
    /// If the cursor changes, the worker is making progress.
    ///
    /// `prev_cursor` is the cursor from the last observation. Polling backends
    /// (e.g. Cloudflare) use it to resume incremental log fetches instead of
    /// re-fetching from the beginning on every tick.
    async fn observe(
        &self,
        handle: &WorkerHandle,
        prev_cursor: Option<&str>,
    ) -> Result<RunObservation, ThalaError>;

    /// Forcefully terminate a worker.
    async fn cancel(&self, handle: &WorkerHandle) -> Result<(), ThalaError>;

    /// Full cleanup: terminate if still running and remove local worktree (if any).
    async fn cleanup(
        &self,
        handle: &WorkerHandle,
        workspace_root: &Path,
        task_id: &str,
    ) -> Result<(), ThalaError>;
}
