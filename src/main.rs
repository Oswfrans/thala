//! Thala orchestration kernel — main entry point.
//!
//! Parses WORKFLOW.md, wires all adapters, and starts the OrchestratorEngine.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

use thala::adapters::beads::{BeadsTaskSink, BeadsTaskSource};
use thala::adapters::execution::router::DefaultBackendRouter;
use thala::adapters::execution::{
    CloudflareBackend, KubernetesBackend, LocalBackend, ModalBackend, ModalConfig,
};
use thala::adapters::intake::discord::{DiscordIntake, DiscordIntakeConfig};
use thala::adapters::intake::discord_webhook::{DiscordWebhookConfig, DiscordWebhookServer};
use thala::adapters::intake::slack::{SlackIntake, SlackIntakeConfig};
use thala::adapters::intake::slack_webhook::{SlackWebhookConfig, SlackWebhookServer};
use thala::adapters::interaction::discord::{DiscordInteraction, DiscordInteractionConfig};
use thala::adapters::interaction::slack::{SlackInteraction, SlackInteractionConfig};
use thala::adapters::repo::GitRepoProvider;
use thala::adapters::state::SqliteStateStore;
use thala::adapters::validation::review_ai::ReviewAiValidator;
use thala::adapters::validation::NoopValidator;
use thala::core::run::ExecutionBackendKind;
use thala::core::workflow::WorkflowConfig;
use thala::onboard_wizard::run_enhanced_onboard;
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
    /// Interactive setup wizard — generates WORKFLOW.md and config.toml
    Onboard,
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

    // Onboard doesn't need WORKFLOW.md — handle it before the file load.
    if matches!(cli.command, Some(Command::Onboard)) {
        return run_enhanced_onboard();
    }

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
        Command::Onboard => unreachable!("handled above"),
        Command::Run => {}
    }

    // ── Wire adapters ─────────────────────────────────────────────────────────

    if workflow.tracker.backend != "beads" {
        anyhow::bail!(
            "Unsupported tracker backend '{}'. Only 'beads' is supported right now.",
            workflow.tracker.backend
        );
    }

    let workspace_root = PathBuf::from(&workflow.execution.workspace_root);
    let beads_root = workflow
        .tracker
        .beads_workspace_root
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.clone());

    preflight_workflow(&workflow, &beads_root)?;

    // Beads
    let source = Arc::new(
        BeadsTaskSource::new(&beads_root)
            .with_ready_status(workflow.tracker.beads_ready_status.clone()),
    );
    let sink = Arc::new(BeadsTaskSink::new(&beads_root));

    // Execution backends
    let local = Arc::new(LocalBackend::new());
    let modal = Arc::new(ModalBackend::new(ModalConfig::from_env()));
    let cloudflare = Arc::new(CloudflareBackend::from_env());
    let kubernetes = Arc::new(KubernetesBackend::from_env());
    let router = Arc::new(DefaultBackendRouter::new(
        local, modal, cloudflare, kubernetes,
    ));

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
    let mut slack_intake: Option<Arc<SlackIntake>> = None;
    let mut slack_interaction: Option<Arc<SlackInteraction>> = None;
    if let Some(slack_cfg) = &workflow.slack {
        let interaction = Arc::new(
            SlackInteraction::new(SlackInteractionConfig {
                bot_token: slack_cfg.bot_token.clone(),
                signing_secret: slack_cfg.signing_secret.clone(),
                alerts_channel: slack_cfg.alerts_channel.clone(),
                db_path: state_dir.join("slack-interactions.db"),
            })
            .context("Failed to open Slack interactions database")?,
        );
        interaction_layers.push(interaction.clone());
        slack_interaction = Some(interaction);

        if std::env::var("SLACK_INTAKE_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true)
        {
            let manager_api_key = std::env::var("OPENROUTER_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
                .unwrap_or_default();
            let manager_api_base = std::env::var("MANAGER_API_BASE")
                .unwrap_or_else(|_| "https://openrouter.ai/api/v1".into());

            let sink_trait: Arc<dyn thala::ports::task_sink::TaskSink> = sink.clone();
            slack_intake = Some(Arc::new(SlackIntake::new(
                SlackIntakeConfig {
                    bot_token: slack_cfg.bot_token.clone(),
                    signing_secret: slack_cfg.signing_secret.clone(),
                    manager_api_key,
                    manager_api_base,
                    planning_model: workflow.models.manager.clone(),
                    product: workflow.product.clone(),
                },
                sink_trait,
            )));

            info!("Slack intake enabled");
        }
    }
    // Discord interaction and intake (for approvals and task creation)
    let mut discord_intake: Option<Arc<DiscordIntake>> = None;
    let mut discord_interaction: Option<Arc<DiscordInteraction>> = None;

    if let Some(discord_cfg) = &workflow.discord {
        let interaction = Arc::new(DiscordInteraction::new(DiscordInteractionConfig {
            bot_token: discord_cfg.bot_token.clone(),
            public_key: discord_cfg.public_key.clone(),
            alerts_channel_id: discord_cfg.alerts_channel_id.clone(),
        }));
        discord_interaction = Some(Arc::clone(&interaction));
        interaction_layers.push(interaction);

        // Set up Discord intake if environment is configured
        if std::env::var("DISCORD_INTAKE_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(true)
        {
            let manager_api_key = std::env::var("OPENROUTER_API_KEY")
                .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
                .unwrap_or_default();
            let manager_api_base = std::env::var("MANAGER_API_BASE")
                .unwrap_or_else(|_| "https://openrouter.ai/api/v1".into());

            let sink_trait: Arc<dyn thala::ports::task_sink::TaskSink> = sink.clone();
            discord_intake = Some(Arc::new(DiscordIntake::new(
                DiscordIntakeConfig {
                    bot_token: discord_cfg.bot_token.clone(),
                    public_key: discord_cfg.public_key.clone(),
                    manager_api_key,
                    manager_api_base,
                    planning_model: workflow.models.manager.clone(),
                    product: workflow.product.clone(),
                },
                sink_trait,
            )));

            info!("Discord intake enabled");
        }
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

    // Validator — use ReviewAiValidator when ANTHROPIC_API_KEY is set, otherwise noop.
    let review_ai: Arc<dyn thala::ports::validator::Validator> =
        match ReviewAiValidator::from_env(&workflow.models.manager) {
            Ok(v) => {
                info!(model = %workflow.models.manager, "ReviewAiValidator enabled");
                Arc::new(v)
            }
            Err(_) => {
                tracing::warn!(
                    "ANTHROPIC_API_KEY not set — review AI disabled, using NoopValidator"
                );
                Arc::new(NoopValidator)
            }
        };

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

    // ── Start Discord webhook server (if configured) ──────────────────────────

    let mut discord_webhook_handle: Option<tokio::task::JoinHandle<()>> = None;
    if let (Some(discord_cfg), Some(intake), Some(interaction)) =
        (&workflow.discord, &discord_intake, &discord_interaction)
    {
        let bind_addr = std::env::var("THALA_DISCORD_BIND")
            .ok()
            .map(|raw| {
                raw.parse()
                    .map_err(|e| anyhow::anyhow!("invalid THALA_DISCORD_BIND '{raw}': {e}"))
            })
            .transpose()?;

        let webhook_config = DiscordWebhookConfig::from_workflow(discord_cfg, bind_addr);
        let prefix_len = webhook_config.public_key.len().min(16);
        tracing::info!(
            public_key_prefix = &webhook_config.public_key[..prefix_len],
            public_key_len = webhook_config.public_key.len(),
            "Discord webhook config loaded"
        );

        let webhook_server = DiscordWebhookServer::new(
            webhook_config,
            Some(Arc::clone(intake)),
            Some(Arc::clone(interaction)),
        );

        discord_webhook_handle = Some(tokio::spawn(async move {
            if let Err(e) = webhook_server.run().await {
                tracing::error!("Discord webhook server error: {}", e);
            }
        }));

        info!("Discord webhook server started");
    }

    // ── Start Slack webhook server (if configured) ────────────────────────────

    let mut slack_webhook_handle: Option<tokio::task::JoinHandle<()>> = None;
    if let Some(interaction) = &slack_interaction {
        let webhook_config = SlackWebhookConfig::from_env()?;
        let webhook_server = SlackWebhookServer::new(
            webhook_config,
            Arc::clone(interaction),
            slack_intake.clone(),
        );

        slack_webhook_handle = Some(tokio::spawn(async move {
            if let Err(e) = webhook_server.run().await {
                tracing::error!("Slack webhook server error: {}", e);
            }
        }));

        info!("Slack webhook server started");
    }

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

    // Clean up webhook server
    if let Some(handle) = discord_webhook_handle {
        handle.abort();
    }
    if let Some(handle) = slack_webhook_handle {
        handle.abort();
    }

    Ok(())
}

// ── Host tool and Beads setup helpers ─────────────────────────────────────────

fn preflight_workflow(workflow: &WorkflowConfig, beads_root: &Path) -> Result<()> {
    ensure_tool_installed("bd", workflow.execution.backend.clone(), false)?;
    if workflow.execution.backend == ExecutionBackendKind::Local {
        ensure_tool_installed("opencode", workflow.execution.backend.clone(), false)?;
        ensure_tool_installed("tmux", workflow.execution.backend.clone(), false)?;
    }
    ensure_beads_workspace(beads_root, true)
}

fn ensure_tool_installed(
    tool: &str,
    backend: ExecutionBackendKind,
    interactive: bool,
) -> Result<()> {
    if find_binary(tool).is_some() {
        return Ok(());
    }

    if auto_install_tools()
        || (interactive && prompt_yes_no(&format!("{tool} is missing. Install it now?"), true))
    {
        match tool {
            "bd" => install_bd()?,
            "opencode" => install_opencode()?,
            "tmux" => install_tmux()?,
            _ => anyhow::bail!("no installer is registered for {tool}"),
        }
    }

    if find_binary(tool).is_none() {
        let backend_note = if backend == ExecutionBackendKind::Local {
            "local backend"
        } else {
            "selected backend"
        };
        anyhow::bail!(
            "{tool} is required for the {backend_note}. Run `bash dev/setup.sh --backend {}` or install {tool} manually.",
            backend.as_str()
        );
    }

    Ok(())
}

fn ensure_beads_workspace(workspace_root: &Path, auto_init: bool) -> Result<()> {
    if workspace_root.join(".beads").exists() {
        return Ok(());
    }
    if !workspace_root.exists() {
        anyhow::bail!(
            "Beads workspace root does not exist: {}",
            workspace_root.display()
        );
    }

    let should_init = auto_init && auto_init_beads();
    if !should_init {
        anyhow::bail!(
            "No .beads workspace found at {}. Run `bd init` there before starting Thala.",
            workspace_root.display()
        );
    }

    println!(
        "Initializing Beads workspace at {} ...",
        workspace_root.display()
    );
    let bd = find_binary("bd").unwrap_or_else(|| PathBuf::from("bd"));
    let quiet = std::process::Command::new(&bd)
        .args(["init", "--quiet"])
        .current_dir(workspace_root)
        .status()
        .context("failed to run `bd init --quiet`")?;
    if quiet.success() {
        return Ok(());
    }

    let fallback = std::process::Command::new(&bd)
        .arg("init")
        .current_dir(workspace_root)
        .status()
        .context("failed to run `bd init`")?;
    if fallback.success() {
        Ok(())
    } else {
        anyhow::bail!("`bd init` failed in {}", workspace_root.display())
    }
}

fn install_bd() -> Result<()> {
    if find_binary("bd").is_some() {
        return Ok(());
    }

    if find_binary("brew").is_some() {
        println!("Installing bd with Homebrew ...");
        if run_status("brew", &["install", "beads"])? {
            return Ok(());
        }
        println!("! Homebrew install failed; falling back to official install script.");
    }

    println!("Installing bd with the official Beads install script ...");
    let script = download_script(
        "https://raw.githubusercontent.com/steveyegge/beads/main/scripts/install.sh",
        "thala-install-beads.sh",
    )?;
    if !std::process::Command::new("bash")
        .arg(&script)
        .status()
        .context("failed to run Beads install script")?
        .success()
    {
        anyhow::bail!("Beads install script failed")
    }
    Ok(())
}

fn install_opencode() -> Result<()> {
    if find_binary("opencode").is_some() {
        return Ok(());
    }

    println!("Installing opencode with the official install script ...");
    let script = download_script("https://opencode.ai/install", "thala-install-opencode.sh")?;
    let script_ok = std::process::Command::new("bash")
        .arg(&script)
        .status()
        .context("failed to run opencode install script")?
        .success();
    if script_ok && find_binary("opencode").is_some() {
        return Ok(());
    }

    if find_binary("npm").is_some() {
        println!("Official install script did not expose opencode on PATH; trying npm ...");
        if run_status("npm", &["install", "-g", "opencode-ai"])? {
            return Ok(());
        }
    }

    anyhow::bail!("opencode installation failed")
}

fn install_tmux() -> Result<()> {
    if find_binary("tmux").is_some() {
        return Ok(());
    }
    if find_binary("apt-get").is_some() {
        println!("Installing tmux with apt-get ...");
        if run_status("sudo", &["apt-get", "install", "-y", "tmux"])? {
            return Ok(());
        }
    }
    anyhow::bail!("tmux is missing and could not be installed automatically")
}

fn download_script(url: &str, name: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(name);
    let status = std::process::Command::new("curl")
        .args(["-fsSL", url, "-o"])
        .arg(&path)
        .status()
        .context("failed to run curl")?;
    if status.success() {
        Ok(path)
    } else {
        anyhow::bail!("failed to download installer from {url}")
    }
}

fn run_status(program: &str, args: &[&str]) -> Result<bool> {
    Ok(std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?
        .success())
}

fn find_binary(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH");
    let mut candidates: Vec<PathBuf> = path_var
        .as_ref()
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(name))
        .collect();

    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.extend([
            home.join(".opencode/bin").join(name),
            home.join(".local/bin").join(name),
            home.join("go/bin").join(name),
            home.join(".cargo/bin").join(name),
            home.join(".bun/bin").join(name),
        ]);
    }

    candidates.into_iter().find(|path| path.is_file())
}

fn auto_install_tools() -> bool {
    truthy_env("THALA_AUTO_INSTALL_TOOLS", false)
}

fn auto_init_beads() -> bool {
    truthy_env("THALA_AUTO_INIT_BEADS", true)
}

fn truthy_env(name: &str, default: bool) -> bool {
    std::env::var(name).map_or(default, |value| {
        matches!(
            value.as_str(),
            "1" | "true" | "TRUE" | "yes" | "YES" | "y" | "Y"
        )
    })
}

fn prompt_yes_no(question: &str, default_yes: bool) -> bool {
    use std::io::{self, Write};

    let default = if default_yes { "Y/n" } else { "y/N" };
    print!("{question} [{default}]: ");
    io::stdout().flush().ok();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().to_ascii_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    }
}
