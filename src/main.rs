//! Thala orchestration kernel — main entry point.
//!
//! Parses WORKFLOW.md, wires all adapters, and starts the OrchestratorEngine.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

use thala::adapters::beads::{BeadsTaskSink, BeadsTaskSource};
use thala::adapters::execution::router::DefaultBackendRouter;
use thala::adapters::execution::{
    CloudflareBackend, CloudflareConfig, LocalBackend, ModalBackend, ModalConfig,
};
use thala::adapters::interaction::discord::{DiscordInteraction, DiscordInteractionConfig};
use thala::adapters::interaction::slack::{SlackInteraction, SlackInteractionConfig};
use thala::adapters::repo::GitRepoProvider;
use thala::adapters::state::SqliteStateStore;
use thala::adapters::validation::NoopValidator;
use thala::core::workflow::WorkflowConfig;
use thala::orchestrator::dispatcher::DispatcherConfig;
use thala::orchestrator::engine::{EngineConfig, OrchestratorEngine};
use thala::orchestrator::human_loop::HumanLoopConfig;
use thala::orchestrator::monitor::MonitorConfig;
use thala::orchestrator::prompt_builder::extract_template_body;
use thala::orchestrator::scheduler::SchedulerConfig;
use thala::ports::interaction::InteractionLayer;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "thala",
    about = "Thala — orchestration kernel for managed coding tasks",
    version
)]
struct Cli {
    /// Path to WORKFLOW.md (defaults to ./WORKFLOW.md)
    #[arg(long, default_value = "WORKFLOW.md")]
    workflow: PathBuf,

    /// Log filter (e.g. "thala=debug,info")
    #[arg(long, default_value = "thala=info")]
    log: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the orchestration engine (default)
    Run,
    /// Validate WORKFLOW.md without starting the engine
    Validate,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Tracing
    fmt()
        .with_env_filter(EnvFilter::new(&cli.log))
        .with_target(false)
        .compact()
        .init();

    // Load WORKFLOW.md
    let workflow_path = cli.workflow.canonicalize().with_context(|| {
        format!(
            "WORKFLOW.md not found at '{}'. Create one or pass --workflow <path>.",
            cli.workflow.display()
        )
    })?;
    let raw = std::fs::read_to_string(&workflow_path)
        .with_context(|| format!("Failed to read {}", workflow_path.display()))?;
    let workflow = WorkflowConfig::from_markdown(&raw)
        .with_context(|| format!("Failed to parse {}", workflow_path.display()))?;

    info!(path = %workflow_path.display(), product = %workflow.product, "Loaded WORKFLOW.md");

    match cli.command.unwrap_or(Command::Run) {
        Command::Validate => {
            println!("WORKFLOW.md is valid.");
            return Ok(());
        }
        Command::Run => {}
    }

    // ── Wire adapters ─────────────────────────────────────────────────────────

    let workspace_root = PathBuf::from(&workflow.execution.workspace_root);

    // Beads
    let source = Arc::new(BeadsTaskSource::new(&workspace_root));
    let sink = Arc::new(BeadsTaskSink::new(&workspace_root));

    // Execution backends
    let local = Arc::new(LocalBackend::new());
    let modal = Arc::new(ModalBackend::new(ModalConfig::default()));
    let cloudflare = Arc::new(CloudflareBackend::new(CloudflareConfig::default()));
    let router = Arc::new(DefaultBackendRouter::new(local, modal, cloudflare));

    // Data directory — used by both the state store and the Slack inbox.
    let state_dir = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/share")
        })
        .join("thala");
    std::fs::create_dir_all(&state_dir).context("Failed to create Thala data dir")?;

    // Interaction layers (optional — only added when config is present)
    let mut interaction_layers: Vec<Arc<dyn InteractionLayer>> = Vec::new();
    if let Some(slack_cfg) = &workflow.slack {
        interaction_layers.push(Arc::new(
            SlackInteraction::new(SlackInteractionConfig {
                bot_token: slack_cfg.bot_token.clone(),
                signing_secret: slack_cfg.signing_secret.clone(),
                alerts_channel: slack_cfg.alerts_channel.clone(),
                db_path: state_dir.join("slack-interactions.db"),
            })
            .context("Failed to open Slack interactions database")?,
        ));
    }
    if let Some(discord_cfg) = &workflow.discord {
        interaction_layers.push(Arc::new(DiscordInteraction::new(
            DiscordInteractionConfig {
                bot_token: discord_cfg.bot_token.clone(),
                public_key: discord_cfg.public_key.clone(),
                alerts_channel_id: discord_cfg.alerts_channel_id.clone(),
            },
        )));
    }

    // State store — SQLite in the same data dir established above.
    let store = Arc::new(
        SqliteStateStore::open(state_dir.join("state.db"))
            .context("Failed to open state database")?,
    );

    // Repo provider
    let repo = Arc::new(GitRepoProvider::new(
        &workflow.github_repo,
        &workflow.execution.github_token_env,
    ));

    // Validator — defaults to noop; swap in ReviewAiValidator when ready
    let review_ai = Arc::new(NoopValidator);

    // ── Engine config ─────────────────────────────────────────────────────────

    let engine_config = EngineConfig {
        workflow: workflow.clone(),
        scheduler: SchedulerConfig {
            poll_interval: std::time::Duration::from_secs(30),
            max_concurrent_runs: workflow.limits.max_concurrent_runs,
        },
        monitor: MonitorConfig {
            poll_interval: std::time::Duration::from_secs(60),
            stall_timeout_ms: workflow.limits.stall_timeout_ms,
        },
        human_loop: HumanLoopConfig {
            poll_interval: std::time::Duration::from_secs(15),
        },
        dispatcher: DispatcherConfig {
            workspace_root: workspace_root.clone(),
            product: workflow.product.clone(),
            // Extract the Tera template body from WORKFLOW.md (everything after the front matter).
            prompt_template: {
                let body = extract_template_body(&raw).trim().to_string();
                if body.is_empty() {
                    None
                } else {
                    Some(body)
                }
            },
        },
    };

    // ── Start engine ──────────────────────────────────────────────────────────

    info!("Starting Thala orchestration engine");
    OrchestratorEngine::new(
        engine_config,
        source,
        sink,
        store,
        router,
        repo,
        review_ai,
        interaction_layers,
    )
    .run()
    .await
    .context("Orchestration engine failed")?;

    Ok(())
}
