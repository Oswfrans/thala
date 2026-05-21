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

    /// Worker CLI runner selection and runner-specific options.
    #[serde(default)]
    pub worker: WorkerConfig,

    /// Opt-in goal-mode research fanout configuration.
    #[serde(default)]
    pub goals: GoalsConfig,

    /// Retry and reroute policy.
    #[serde(default)]
    pub retry: RetryPolicy,

    /// Merge policy for completed PRs.
    #[serde(default)]
    pub merge: MergePolicy,

    /// Policy for stuck task handling.
    #[serde(default)]
    pub stuck: StuckPolicy,

    /// Lifecycle hooks run at key points in the worker lifecycle.
    ///
    /// Hooks are trusted workflow-owner shell snippets from WORKFLOW.md, not
    /// task/user input. Backends intentionally execute them through a shell so
    /// operators can use normal shell composition. A non-zero hook exit fails
    /// the launch/run phase that invoked it.
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

    pub fn worker_runner_config(&self) -> WorkerConfig {
        let mut config = self.worker.clone();
        if let Some(runner) = self.execution.worker_runner {
            config.runner = runner;
        }
        config
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

    /// Compatibility alias for older/newly-written workflows that placed the
    /// runner selector under `execution`. Prefer top-level `worker.runner`.
    #[serde(default)]
    pub worker_runner: Option<WorkerRunnerKind>,
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
            worker_runner: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Worker CLI runner used inside the selected execution backend.
    #[serde(default)]
    pub runner: WorkerRunnerKind,

    /// Runner extension package identifiers enabled before execution.
    #[serde(
        default = "default_worker_extensions",
        deserialize_with = "deserialize_extensions"
    )]
    pub extensions: Vec<String>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            runner: WorkerRunnerKind::default(),
            extensions: default_worker_extensions(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerRunnerKind {
    #[default]
    Pi,
    Opencode,
}

impl WorkerRunnerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Opencode => "opencode",
        }
    }

    pub fn binary(self) -> &'static str {
        self.as_str()
    }
}

fn default_worker_extensions() -> Vec<String> {
    Vec::new()
}

fn deserialize_extensions<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let extensions = Vec::<String>::deserialize(deserializer)?;
    for extension in &extensions {
        if !is_valid_extension_name(extension) {
            return Err(serde::de::Error::custom(format!(
                "unsafe worker extension name `{extension}`"
            )));
        }
    }
    Ok(extensions)
}

pub fn is_valid_extension_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 214 {
        return false;
    }

    let segments: Vec<&str> = if let Some(scoped) = value.strip_prefix('@') {
        let Some((scope, name)) = scoped.split_once('/') else {
            return false;
        };
        vec![scope, name]
    } else {
        if value.contains('/') {
            return false;
        }
        vec![value]
    };

    segments.into_iter().all(is_valid_package_segment)
}

fn is_valid_package_segment(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalsConfig {
    /// Goal-mode tasks are ignored unless explicitly enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Optional backend override for child research runs. Defaults to the
    /// workflow execution backend.
    #[serde(default)]
    pub default_backend: Option<ExecutionBackendKind>,

    /// Number of child research variants to launch in one round.
    #[serde(default = "default_max_parallel_research_runs")]
    pub max_parallel_research_runs: usize,

    /// Minimum completed research reports required before synthesis. Defaults
    /// to one so a single surviving variant can still unblock the goal.
    #[serde(default = "default_min_successful_reports")]
    pub min_successful_reports: usize,

    /// Maximum research rounds before a human decision is required.
    #[serde(default = "default_max_research_rounds")]
    pub max_research_rounds: u32,

    /// Whether synthesized follow-up implementation tasks need human approval.
    #[serde(default = "default_require_human_before_followup_tasks")]
    pub require_human_before_followup_tasks: bool,

    /// Extensions enabled for goal research workers. This is intentionally
    /// separate from normal worker defaults.
    #[serde(default, deserialize_with = "deserialize_extensions")]
    pub worker_extensions: Vec<String>,
}

impl Default for GoalsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_backend: None,
            max_parallel_research_runs: default_max_parallel_research_runs(),
            min_successful_reports: default_min_successful_reports(),
            max_research_rounds: default_max_research_rounds(),
            require_human_before_followup_tasks: default_require_human_before_followup_tasks(),
            worker_extensions: Vec::new(),
        }
    }
}

fn default_max_parallel_research_runs() -> usize {
    3
}

fn default_min_successful_reports() -> usize {
    1
}

fn default_max_research_rounds() -> u32 {
    2
}

fn default_require_human_before_followup_tasks() -> bool {
    true
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
    /// Model for worker execution sessions (e.g. "openrouter/moonshotai/kimi-k2.5").
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
    "openrouter/moonshotai/kimi-k2.5".into()
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

/// Trusted shell snippets executed at lifecycle points.
///
/// Hooks are sourced only from WORKFLOW.md and are treated as operator-owned
/// configuration. They are intentionally evaluated by a shell across backends,
/// so they must not be populated from Beads task text or chat input.
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

    /// Reserved for future local cleanup support. Current backends do not
    /// execute this hook.
    #[serde(default)]
    pub before_cleanup: Option<String>,
}
