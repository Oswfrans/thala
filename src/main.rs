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
    CloudflareBackend, LocalBackend, ModalBackend, ModalConfig, OpenCodeZenBackend,
    OpenCodeZenConfig,
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
        return run_onboard();
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

    let workspace_root = PathBuf::from(&workflow.execution.workspace_root);

    // Beads
    let source = Arc::new(BeadsTaskSource::new(&workspace_root));
    let sink = Arc::new(BeadsTaskSink::new(&workspace_root));

    // Execution backends
    let local = Arc::new(LocalBackend::new());
    let modal = Arc::new(ModalBackend::new(ModalConfig::from_env()));
    let cloudflare = Arc::new(CloudflareBackend::from_env());
    let opencode_zen = Arc::new(OpenCodeZenBackend::new(OpenCodeZenConfig::from_env()));
    let router = Arc::new(DefaultBackendRouter::new(
        local,
        modal,
        cloudflare,
        opencode_zen,
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

// ── Onboarding wizard ─────────────────────────────────────────────────────────

fn run_onboard() -> Result<()> {
    use std::io::{self, Write};

    fn prompt(label: &str, default: &str) -> String {
        let hint = if default.is_empty() {
            String::new()
        } else {
            format!(" [{default}]")
        };
        print!("{label}{hint}: ");
        io::stdout().flush().ok();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).ok();
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            default.to_string()
        } else {
            trimmed
        }
    }

    fn choose(label: &str, options: &[&str]) -> usize {
        println!("{label}");
        for (i, opt) in options.iter().enumerate() {
            println!("  {}. {opt}", i + 1);
        }
        loop {
            print!("Choice [1]: ");
            io::stdout().flush().ok();
            let mut buf = String::new();
            io::stdin().read_line(&mut buf).ok();
            let s = buf.trim();
            if s.is_empty() {
                return 0;
            }
            if let Ok(n) = s.parse::<usize>() {
                if n >= 1 && n <= options.len() {
                    return n - 1;
                }
            }
            println!("Please enter a number between 1 and {}.", options.len());
        }
    }

    println!();
    println!("╔══════════════════════════════════════════╗");
    println!("║         Thala Onboarding Wizard          ║");
    println!("╚══════════════════════════════════════════╝");
    println!();
    println!("This wizard generates a WORKFLOW.md for your product repo.");
    println!("Press Enter to accept defaults shown in [brackets].");
    println!();

    // Product info
    let product = prompt("Product slug (e.g. my-app)", "my-app");
    let workspace_root = prompt(
        "Absolute path to the product workspace",
        "/workspaces/my-app",
    );
    let github_repo = prompt("GitHub repo slug (e.g. org/repo)", "");

    // Tracker
    let tracker_idx = choose(
        "Which task tracker?",
        &["Beads (default, no API key needed)", "Notion"],
    );
    let (tracker_backend, notion_fields) = if tracker_idx == 0 {
        ("beads".to_string(), String::new())
    } else {
        let db_id = prompt("Notion database ID", "");
        let api_key_note = "  # Set NOTION_API_TOKEN env var — do not put it here";
        (
            "notion".to_string(),
            format!("\n  database_id: {db_id}{api_key_note}"),
        )
    };

    // Execution backend
    let backend_idx = choose(
        "Which execution backend?",
        &[
            "local   — tmux sessions on this host (no extra credentials)",
            "opencode-zen — OpenCode Zen managed workers (OPENCODE_API_KEY)",
            "cloudflare   — Cloudflare Containers (CF_ACCOUNT_ID, CF_API_TOKEN, CF_WORKER_IMAGE)",
            "modal        — Modal serverless containers (modal CLI)",
        ],
    );
    let backend_names = ["local", "opencode-zen", "cloudflare", "modal"];
    let backend = backend_names[backend_idx];

    let (backend_block, env_note) = match backend {
        "opencode-zen" => {
            let cb = prompt(
                "Callback base URL (public URL of this Thala instance)",
                "https://thala.example.com",
            );
            (
                format!(
                    "\nexecution:\n  backend: opencode-zen\n  workspace_root: \"{workspace_root}\"\n  callback_base_url: \"{cb}\"\n  github_token_env: THALA_GITHUB_TOKEN"
                ),
                "\nRequired env vars:\n  OPENCODE_API_KEY=sk-...\n  THALA_GITHUB_TOKEN=ghp_...\n  THALA_CALLBACK_SECRET=$(openssl rand -hex 32)",
            )
        }
        "cloudflare" => {
            let cb = prompt(
                "Callback base URL (public URL of this Thala instance)",
                "https://thala.example.com",
            );
            (
                format!(
                    "\nexecution:\n  backend: cloudflare\n  workspace_root: \"{workspace_root}\"\n  callback_base_url: \"{cb}\"\n  github_token_env: THALA_GITHUB_TOKEN"
                ),
                "\nRequired env vars:\n  CF_ACCOUNT_ID=...\n  CF_API_TOKEN=...\n  CF_WORKER_IMAGE=registry.example.com/thala-worker:latest\n  THALA_GITHUB_TOKEN=ghp_...\n  THALA_CALLBACK_SECRET=$(openssl rand -hex 32)",
            )
        }
        "modal" => {
            let cb = prompt(
                "Callback base URL (public URL of this Thala instance)",
                "https://thala.example.com",
            );
            let app_file = prompt(
                "Modal worker file (relative to workspace root)",
                "dev/infra/modal_worker.py::run_worker",
            );
            (
                format!(
                    "\nexecution:\n  backend: modal\n  workspace_root: \"{workspace_root}\"\n  callback_base_url: \"{cb}\"\n  github_token_env: THALA_GITHUB_TOKEN\n\n# MODAL_APP_FILE={app_file}  # set this as an env var before starting Thala"
                ),
                "\nRequired env vars:\n  THALA_GITHUB_TOKEN=ghp_...\n  THALA_CALLBACK_SECRET=$(openssl rand -hex 32)\n  MODAL_APP_FILE=dev/infra/modal_worker.py::run_worker",
            )
        }
        _ => (String::new(), ""), // local — no extra env vars
    };

    // Models
    let worker_model = prompt("Worker model", "opencode/kimi-k2.5");
    let manager_model = prompt("Manager model (review AI)", "anthropic/claude-opus-4-6");

    // Notifications
    let discord_token = prompt("Discord bot token (leave blank to skip)", "");
    let discord_channel = if !discord_token.is_empty() {
        prompt("Discord alerts channel ID", "")
    } else {
        String::new()
    };
    let discord_block = if !discord_token.is_empty() && !discord_channel.is_empty() {
        format!(
            "\ndiscord:\n  bot_token: \"{discord_token}\"  # move to env var for production\n  public_key: \"\"  # fill in from Discord developer portal\n  alerts_channel_id: \"{discord_channel}\""
        )
    } else {
        String::new()
    };

    // Generate WORKFLOW.md
    let tracker_block = if tracker_backend == "beads" {
        format!(
            "tracker:\n  backend: beads\n  active_states: [\"open\"]\n  terminal_states: [\"Done\", \"Cancelled\"]\n  beads_workspace_root: {workspace_root}\n  beads_ready_status: open"
        )
    } else {
        format!(
            "tracker:\n  backend: notion\n  active_states: [\"Ready\"]\n  terminal_states: [\"Done\", \"Cancelled\"]{notion_fields}"
        )
    };

    let workflow_md = format!(
        r#"---
product: "{product}"
github_repo: "{github_repo}"

{tracker_block}
{backend_block}

models:
  worker: "{worker_model}"
  manager: "{manager_model}"
  max_review_cycles: 2

hooks:
  before_run: "git pull --rebase --autostash origin main"
  after_run: ""
  before_cleanup: ""

limits:
  max_concurrent_runs: 3
  stall_timeout_ms: 1800000

retry:
  max_attempts: 3
  allow_backend_reroute: false

merge:
  auto_merge: false
  protected_paths:
    - "auth/**"
    - "billing/**"
    - "infra/**"
    - "**/migrations/**"
    - ".github/workflows/**"

stuck:
  auto_resolve_after_ms: 0
{discord_block}
---

You are an expert developer working on **{product}**.

## Task

**ID:** {{{{ issue.identifier }}}}
**Title:** {{{{ issue.title }}}}
**Attempt:** {{{{ run.attempt }}}}

## Acceptance Criteria

{{{{ issue.acceptance_criteria }}}}

{{%- if issue.context %}}
## Context

{{{{ issue.context }}}}
{{%- endif %}}

When complete, write `DONE` to `.thala/signals/{{{{ issue.identifier }}}}.signal`.
"#,
    );

    let out_path = format!("{workspace_root}/WORKFLOW.md");
    println!();
    println!("─── Generated WORKFLOW.md preview ───────────────────────────────────────");
    println!("{workflow_md}");
    println!("─────────────────────────────────────────────────────────────────────────");

    print!("Write to {out_path}? [y/N]: ");
    io::stdout().flush().ok();
    let mut confirm = String::new();
    io::stdin().read_line(&mut confirm).ok();
    if confirm.trim().to_lowercase() == "y" {
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("✗ Could not create directory {}: {e}", parent.display());
                eprintln!("  Create the workspace directory first, then copy the WORKFLOW.md preview above.");
                eprintln!("  Or run: mkdir -p {} && cp /dev/stdin {out_path}", parent.display());
            } else if let Err(e) = std::fs::write(&out_path, &workflow_md) {
                eprintln!("✗ Failed to write {out_path}: {e}");
                eprintln!("  Copy the preview above manually.");
            } else {
                println!("✓ Written to {out_path}");
            }
        }
    } else {
        println!("Skipped write. Copy the preview above manually.");
    }

    if !env_note.is_empty() {
        println!();
        println!("─── Environment variables needed ─────────────────────────────────────────");
        println!("{env_note}");
        println!("─────────────────────────────────────────────────────────────────────────");
        println!("Set these in your systemd unit or shell environment before starting Thala.");
    }

    // Modal-specific: check for uv, install modal, run modal setup.
    if backend == "modal" {
        println!();
        println!("─── Modal setup ──────────────────────────────────────────────────────────");
        setup_modal();
        println!("─────────────────────────────────────────────────────────────────────────");
    }

    println!();
    println!("Next steps:");
    println!("  1. Review / edit {out_path}");
    println!("  2. cargo build --release");
    println!("  3. ./target/release/thala --workflow {out_path} validate");
    println!("  4. ./target/release/thala --workflow {out_path} run");
    println!();

    Ok(())
}

// ── Modal setup helper ────────────────────────────────────────────────────────

/// Check for uv, install Modal if needed, then authenticate interactively.
///
/// Called from the onboarding wizard when the user selects the Modal backend.
/// We prefer uv because it is significantly faster than pip.
fn setup_modal() {
    use std::process::Command;

    let has_uv = Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let modal_installed = Command::new("modal")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if modal_installed {
        println!("✓ modal CLI already installed — skipping install");
    } else if has_uv {
        println!("Installing modal via uv ...");
        let ok = Command::new("uv")
            .args(["tool", "install", "modal"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("✓ modal installed via uv");
            println!("  Note: if the auth step below fails with 'command not found',");
            println!("  open a new terminal (to refresh PATH) and run: modal token new");
        } else {
            eprintln!("✗ uv tool install modal failed — run it manually, then re-run onboarding");
            return;
        }
    } else {
        // Check for Python 3 before attempting pip
        let has_python = Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_python {
            eprintln!("✗ Neither uv nor python3 found — cannot install Modal CLI automatically.");
            eprintln!("  Install uv: curl -LsSf https://astral.sh/uv/install.sh | sh");
            eprintln!("  Then run: uv tool install modal");
            eprintln!("  Then re-run: ./target/release/thala onboard");
            return;
        }
        println!("uv not found — falling back to pip to install modal ...");
        println!("(Consider installing uv for faster tooling: https://astral.sh/uv)");
        // Try `python3 -m pip` first (reliable on Debian/Ubuntu where bare `pip`
        // is often missing), then fall back to bare `pip`.
        let ok = Command::new("python3")
            .args(["-m", "pip", "install", "modal"])
            .status()
            .map(|s| s.success())
            .unwrap_or_else(|_| {
                Command::new("pip")
                    .args(["install", "modal"])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            });
        if ok {
            println!("✓ modal installed via pip");
        } else {
            eprintln!("✗ pip install modal failed — install uv and run: uv tool install modal");
            return;
        }
    }

    println!();
    println!("You need a Modal account to continue.");
    println!("If you don't have one, sign up for free at https://modal.com before proceeding.");
    println!();
    // `modal token new` is the current auth command (Modal CLI >= 0.60).
    // Fall back to `modal setup` for older installs.
    println!("Running `modal token new` — a browser window will open for authentication.");
    println!();
    let auth_status = Command::new("modal").args(["token", "new"]).status();
    let auth_ok = match auth_status {
        Ok(s) if s.success() => {
            println!("✓ Modal authentication complete");
            true
        }
        Ok(_) => {
            // Older CLI versions use `modal setup`
            println!("  `modal token new` unavailable — trying `modal setup` (older CLI) ...");
            match Command::new("modal").arg("setup").status() {
                Ok(s) if s.success() => {
                    println!("✓ modal setup complete");
                    true
                }
                Ok(s) => {
                    eprintln!("✗ modal setup exited {s} — re-run `modal token new` manually");
                    false
                }
                Err(e) => {
                    eprintln!("✗ Failed to run modal: {e}");
                    eprintln!("  The modal binary may not be on PATH yet.");
                    eprintln!("  Open a new terminal and run: modal token new");
                    false
                }
            }
        }
        Err(e) => {
            eprintln!("✗ Failed to run modal: {e}");
            eprintln!("  The modal binary may not be on PATH yet (common after a fresh install).");
            eprintln!("  Open a new terminal and run: modal token new");
            false
        }
    };

    if auth_ok {
        println!();
        println!("Verifying Modal connectivity ...");
        match Command::new("modal").args(["app", "list"]).status() {
            Ok(s) if s.success() => println!("✓ Modal connected — remote compute is available"),
            _ => println!("  Run `modal app list` to verify the connection manually"),
        }
    }
}
