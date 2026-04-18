//! Workflow configuration types.
//!
//! WorkflowConfig is loaded from WORKFLOW.md YAML front-matter (the block
//! between the opening `---` and the closing `---` or `...`).
//! It controls how the orchestrator behaves for a specific product/repo.
//!
//! No vendor-specific details (Modal image names, CF account IDs) belong here —
//! those live in adapter-level config structs.

use serde::{Deserialize, Serialize};

use crate::core::run::ExecutionBackendKind;

// ── WorkflowConfig ────────────────────────────────────────────────────────────

/// Top-level config loaded from WORKFLOW.md front-matter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// Task tracker configuration. Beads is the only supported backend for now.
    #[serde(default)]
    pub tracker: TrackerConfig,

    /// Which execution backend to use for new runs.
    #[serde(default)]
    pub execution: ExecutionConfig,

    /// Concurrency and timing limits.
    #[serde(default)]
    pub limits: LimitsConfig,

    /// Model selection for workers and the manager.
    #[serde(default)]
    pub models: ModelConfig,

    /// Retry and reroute policy.
    #[serde(default)]
    pub retry: RetryPolicy,

    /// Merge policy for completed PRs.
    #[serde(default)]
    pub merge: MergePolicy,

    /// Policy for stuck task handling.
    #[serde(default)]
    pub stuck: StuckPolicy,

    /// Lifecycle hooks (shell commands run at key points).
    #[serde(default)]
    pub hooks: HooksConfig,

    /// Human product name used in notifications (e.g. "example-app").
    pub product: String,

    /// GitHub repository slug (e.g. "org/repo"). Used for PR operations.
    pub github_repo: String,

    /// Optional Slack interaction config (bot_token, signing_secret, alerts_channel).
    #[serde(default)]
    pub slack: Option<SlackConfig>,

    /// Optional Discord interaction config (bot_token, alerts_channel_id).
    #[serde(default)]
    pub discord: Option<DiscordConfig>,
}

impl WorkflowConfig {
    /// Parse WorkflowConfig from a WORKFLOW.md string.
    ///
    /// Accepts two formats:
    /// 1. YAML front-matter: content between `---` delimiters at the top of the file.
    /// 2. A bare YAML document (the entire file is YAML).
    pub fn from_markdown(content: &str) -> Result<Self, serde_yaml::Error> {
        let yaml = extract_front_matter(content);
        serde_yaml::from_str(yaml)
    }
}

/// Extract YAML front-matter from a Markdown document.
///
/// Looks for content between the first `---` line and the next `---` or `...` line.
/// If no front-matter delimiters are found, returns the entire string.
fn extract_front_matter(content: &str) -> &str {
    // Split once on the first newline: part[0] is the opening "---", part[1] is
    // everything else. Using splitn(3) would give only the *second* line as rest,
    // not the whole block.
    let mut parts = content.splitn(2, '\n');
    if parts.next().map(str::trim) != Some("---") {
        return content;
    }
    if let Some(rest) = parts.next() {
        // Find the closing --- or ... delimiter.
        if let Some(close) = rest.find("\n---").or_else(|| rest.find("\n...")) {
            return &rest[..close];
        }
        return rest;
    }
    content
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerConfig {
    #[serde(default = "default_tracker_backend")]
    pub backend: String,

    #[serde(default)]
    pub active_states: Vec<String>,

    #[serde(default)]
    pub terminal_states: Vec<String>,

    #[serde(default)]
    pub beads_workspace_root: Option<String>,

    #[serde(default = "default_beads_ready_status")]
    pub beads_ready_status: String,
}

fn default_tracker_backend() -> String {
    "beads".into()
}

fn default_beads_ready_status() -> String {
    "open".into()
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            backend: default_tracker_backend(),
            active_states: Vec::new(),
            terminal_states: Vec::new(),
            beads_workspace_root: None,
            beads_ready_status: default_beads_ready_status(),
        }
    }
}

// ── Interaction surface configs ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    pub bot_token: String,
    pub signing_secret: String,
    pub alerts_channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    pub bot_token: String,
    /// Discord application public key for verifying interaction signatures.
    pub public_key: String,
    pub alerts_channel_id: String,
}

// ── Sub-configs ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Default backend for new runs.
    #[serde(default = "default_backend")]
    pub backend: ExecutionBackendKind,

    /// Callback base URL for remote backends (e.g. "https://thala.example.com").
    /// Not required for the local backend.
    #[serde(default)]
    pub callback_base_url: Option<String>,

    /// Env var name holding the GitHub token for remote branch push.
    #[serde(default = "default_github_token_env")]
    pub github_token_env: String,

    /// Root directory of the repository (where `.beads/` and `.git/` live).
    /// Defaults to the current working directory.
    #[serde(default = "default_workspace_root")]
    pub workspace_root: String,
}

fn default_backend() -> ExecutionBackendKind {
    ExecutionBackendKind::Local
}

fn default_github_token_env() -> String {
    "GITHUB_TOKEN".into()
}

fn default_workspace_root() -> String {
    ".".into()
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            backend: default_backend(),
            callback_base_url: None,
            github_token_env: default_github_token_env(),
            workspace_root: default_workspace_root(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Maximum number of concurrently active runs.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_runs: usize,

    /// Milliseconds without output change before a run is declared stalled.
    #[serde(default = "default_stall_timeout_ms")]
    pub stall_timeout_ms: u64,
}

fn default_max_concurrent() -> usize {
    3
}
fn default_stall_timeout_ms() -> u64 {
    300_000
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_concurrent_runs: default_max_concurrent(),
            stall_timeout_ms: default_stall_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model for worker execution sessions (e.g. "opencode/kimi-k2.5").
    #[serde(default = "default_worker_model")]
    pub worker: String,

    /// Model for the manager role: review AI, intake planning, etc.
    #[serde(default = "default_manager_model")]
    pub manager: String,

    /// Maximum review-feedback cycles before forcing a PR creation.
    #[serde(default = "default_max_review_cycles")]
    pub max_review_cycles: u32,
}

fn default_worker_model() -> String {
    "opencode/kimi-k2.5".into()
}
fn default_manager_model() -> String {
    "anthropic/claude-opus-4-6".into()
}
fn default_max_review_cycles() -> u32 {
    2
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            worker: default_worker_model(),
            manager: default_manager_model(),
            max_review_cycles: default_max_review_cycles(),
        }
    }
}

// ── Policies ──────────────────────────────────────────────────────────────────

/// Controls how failed or stuck runs are retried.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum total attempts for a task before marking it Failed.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,

    /// If true, retries may be rerouted to a different backend.
    #[serde(default)]
    pub allow_backend_reroute: bool,

    /// Preferred reroute backend when `allow_backend_reroute` is true.
    #[serde(default)]
    pub reroute_to: Option<ExecutionBackendKind>,
}

fn default_max_attempts() -> u32 {
    3
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            allow_backend_reroute: false,
            reroute_to: None,
        }
    }
}

/// Controls autonomous PR merging.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergePolicy {
    /// If true, Thala will merge PRs automatically once CI passes.
    /// Always overridden to false for the "thala-core" product.
    #[serde(default)]
    pub auto_merge: bool,

    /// Path patterns that block auto-merge even when `auto_merge = true`.
    #[serde(default)]
    pub protected_paths: Vec<String>,

    /// CI check names that must all pass. Empty means any passing CI is accepted.
    #[serde(default)]
    pub required_checks: Vec<String>,
}

/// Controls behavior for tasks stuck past the stall timeout.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StuckPolicy {
    /// Milliseconds a task may remain Stuck before Thala auto-resolves it.
    /// 0 means never auto-resolve.
    #[serde(default)]
    pub auto_resolve_after_ms: u64,
}

/// Shell commands executed at lifecycle points.
/// All commands are run in the workspace root.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    /// Run after the worktree/clone is ready, before the worker starts (e.g. npm install).
    #[serde(default)]
    pub after_create: Option<String>,

    /// Run in the worktree immediately before OpenCode starts.
    #[serde(default)]
    pub before_run: Option<String>,

    /// Run after the worker signals completion.
    #[serde(default)]
    pub after_run: Option<String>,

    /// Run before the worktree is deleted.
    #[serde(default)]
    pub before_cleanup: Option<String>,
}
