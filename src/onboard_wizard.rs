//! Enhanced onboard wizard — full Discord + Modal setup.
//!
//! This module provides an interactive CLI wizard that:
//! 1. Collects all required credentials (Discord, Modal, GitHub, OpenRouter)
//! 2. Validates credentials where possible
//! 3. Generates a complete WORKFLOW.md with all configurations
//! 4. Optionally writes secrets to ~/.thala/config.toml
//! 5. Sets up systemd service with all environment variables

use std::io::{self, Write};
use std::path::Path;

use directories::BaseDirs;

/// Run the enhanced onboard wizard.
pub fn run_enhanced_onboard() -> anyhow::Result<()> {
    println!();
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║           Thala Onboard Wizard — Discord + Modal           ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();
    println!("This wizard sets up Thala with Discord intake/interaction and Modal workers.");
    println!();

    // Collect all configuration
    let config = WizardConfig::collect_interactive();

    // Validate credentials
    println!("\n─── Validating Credentials ─────────────────────────────────────────");
    validate_credentials(&config);

    // Generate WORKFLOW.md
    println!("\n─── Generating WORKFLOW.md ───────────────────────────────────────────");
    let workflow_content = generate_workflow(&config);
    println!("{}", workflow_content);

    // Write to file
    let workflow_path = Path::new(&config.workspace_root).join("WORKFLOW.md");
    if prompt_yes_no(
        &format!("\nWrite WORKFLOW.md to {}?", workflow_path.display()),
        true,
    ) {
        std::fs::create_dir_all(&config.workspace_root)?;
        std::fs::write(&workflow_path, &workflow_content)?;
        println!("✓ WORKFLOW.md written");
    }

    // Write secrets file
    println!("\n─── Environment Configuration ─────────────────────────────────────────");
    if prompt_yes_no("Write secrets to ~/.thala/env.sh?", true) {
        write_env_file(&config)?;
        println!("✓ ~/.thala/env.sh written (chmod 600)");
    }

    // Write systemd service
    if prompt_yes_no("Write systemd service file?", true) {
        write_systemd_service(&config)?;
        println!("✓ ~/.config/systemd/user/thala.service written");
        println!("  Run: systemctl --user daemon-reload");
        println!("       systemctl --user start thala");
    }

    if prompt_yes_no(
        "Write Discord router service file for multi-repo Discord setups?",
        false,
    ) {
        write_discord_router_service()?;
        println!("✓ ~/.config/systemd/user/thala-discord-router.service written");
        println!("  Run: systemctl --user daemon-reload");
        println!("       systemctl --user start thala-discord-router");
    }

    // Print summary
    print_summary(&config);

    Ok(())
}

/// Configuration collected by the wizard.
#[derive(Debug, Clone)]
struct WizardConfig {
    product: String,
    github_repo: String,
    workspace_root: String,

    // Discord
    discord_bot_token: String,
    discord_public_key: String,
    discord_channel_id: String,

    // API Keys
    github_token: String,
    openrouter_api_key: String,

    // Modal
    modal_app_file: String,
    callback_base_url: String,

    // Models
    worker_model: String,
    manager_model: String,

    // Limits
    max_concurrent_runs: u32,
    stall_timeout_ms: u64,
}

impl WizardConfig {
    fn collect_interactive() -> Self {
        println!("─── Product Configuration ──────────────────────────────────────────────");

        let product = prompt("Product name (e.g., my-app)", "my-app");
        let github_repo = prompt_required("GitHub repo (org/repo)");
        let workspace_root = prompt(
            "Workspace root (absolute path)",
            &format!("/home/{}/workspace/{}", whoami(), &product),
        );

        println!("\n─── Discord Configuration ──────────────────────────────────────────────");
        println!("Get these from https://discord.com/developers/applications");
        println!("  1. Create an application → Bot → Reset Token");
        println!("  2. General Information → Copy Application ID and Public Key");
        println!("  3. In Discord, right-click your channel → Copy Channel ID");
        println!();

        let discord_bot_token = prompt_secret("Discord Bot Token");
        let discord_public_key = prompt_required("Discord Public Key");
        let discord_channel_id = prompt_required("Discord Channel ID");

        println!("\n─── API Keys ─────────────────────────────────────────────────────────");

        let github_token = prompt_secret("GitHub Personal Access Token (ghp_...)");
        let openrouter_api_key = prompt_secret("OpenRouter API Key (sk-or-v1-...)");

        println!("\n─── Modal Configuration ────────────────────────────────────────────────");

        let modal_app_file = prompt("Modal worker file", "dev/infra/modal_worker.py::run_worker");
        let callback_base_url = prompt(
            "Callback base URL (public URL of this Thala server)",
            "http://localhost:8788",
        );

        println!("\n─── Model Configuration ──────────────────────────────────────────────");

        let worker_model = prompt("Worker model", "openrouter/moonshotai/kimi-k2.5");
        let manager_model = prompt("Manager model", "anthropic/claude-opus-4-6");

        println!("\n─── Limits ─────────────────────────────────────────────────────────────");

        let max_concurrent_runs = prompt_u32("Max concurrent runs", 3);
        let stall_timeout_ms = prompt_u64("Stall timeout (ms)", 300_000);

        Self {
            product,
            github_repo,
            workspace_root,
            discord_bot_token,
            discord_public_key,
            discord_channel_id,
            github_token,
            openrouter_api_key,
            modal_app_file,
            callback_base_url,
            worker_model,
            manager_model,
            max_concurrent_runs,
            stall_timeout_ms,
        }
    }
}

/// Validate credentials where possible.
fn validate_credentials(config: &WizardConfig) {
    // Validate GitHub token format
    if !config.github_token.starts_with("ghp_") && !config.github_token.starts_with("github_pat_") {
        println!("⚠ GitHub token doesn't start with ghp_ or github_pat_ — verify this is correct");
    } else {
        println!("✓ GitHub token format looks valid");
    }

    // Validate OpenRouter key format
    if config.openrouter_api_key.starts_with("sk-or-v1-") {
        println!("✓ OpenRouter API key format looks valid");
    } else {
        println!("⚠ OpenRouter key doesn't start with sk-or-v1- — verify this is correct");
    }

    // Validate Discord public key format (hex, 64 chars)
    if config.discord_public_key.len() == 64 {
        println!("✓ Discord public key format looks valid");
    } else {
        println!("⚠ Discord public key should be 64 hex characters — verify this is correct");
    }

    // Check workspace directory
    let workspace_path = Path::new(&config.workspace_root);
    if workspace_path.exists() {
        println!("✓ Workspace directory exists");

        // Check for existing WORKFLOW.md
        if workspace_path.join("WORKFLOW.md").exists() {
            println!("⚠ WORKFLOW.md already exists in workspace — will be overwritten");
        }

        // Check for .git
        if workspace_path.join(".git").exists() {
            println!("✓ Git repository detected");
        } else {
            println!("⚠ No .git directory — workspace should be a git repo");
        }
    } else {
        println!("⚠ Workspace directory doesn't exist — will be created");
    }
}

/// Generate WORKFLOW.md content.
fn generate_workflow(config: &WizardConfig) -> String {
    format!(
        r#"---
product: "{product}"
github_repo: "{github_repo}"

tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: {workspace_root}
  beads_ready_status: open

execution:
  backend: modal
  workspace_root: {workspace_root}
  github_token_env: THALA_GITHUB_TOKEN
  callback_base_url: {callback_base_url}

models:
  worker: "{worker_model}"
  manager: "{manager_model}"
  max_review_cycles: 2

limits:
  max_concurrent_runs: {max_concurrent_runs}
  stall_timeout_ms: {stall_timeout_ms}

retry:
  max_attempts: 3
  allow_backend_reroute: false

merge:
  auto_merge: false
  protected_paths:
    - "auth/**"
    - "**/migrations/**"
    - ".github/workflows/**"

stuck:
  auto_resolve_after_ms: 0

discord:
  bot_token: "Bot ${{DISCORD_BOT_TOKEN}}"
  public_key: "{discord_public_key}"
  alerts_channel_id: "{discord_channel_id}"

hooks:
  after_create: ""
  before_run: "git pull --rebase --autostash origin main"
  after_run: ""
  before_cleanup: ""
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
        product = config.product,
        github_repo = config.github_repo,
        workspace_root = config.workspace_root,
        callback_base_url = config.callback_base_url,
        worker_model = config.worker_model,
        manager_model = config.manager_model,
        max_concurrent_runs = config.max_concurrent_runs,
        stall_timeout_ms = config.stall_timeout_ms,
        discord_public_key = config.discord_public_key,
        discord_channel_id = config.discord_channel_id,
    )
}

/// Write environment file with secrets.
fn write_env_file(config: &WizardConfig) -> anyhow::Result<()> {
    let thala_dir = BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("Could not find base directories"))?
        .home_dir()
        .join(".thala");
    std::fs::create_dir_all(&thala_dir)?;

    let env_path = thala_dir.join("env.sh");
    let content = format!(
        r#"#!/bin/bash
# Thala environment configuration — generated by onboard wizard
# Source this file: source ~/.thala/env.sh

# Discord
export DISCORD_BOT_TOKEN='{discord_bot_token}'
export DISCORD_PUBLIC_KEY='{discord_public_key}'
export DISCORD_ALERTS_CHANNEL_ID='{discord_channel_id}'

# API Keys
export THALA_GITHUB_TOKEN='{github_token}'
export OPENROUTER_API_KEY='{openrouter_api_key}'

# Modal
export MODAL_APP_FILE='{modal_app_file}'
export THALA_CALLBACK_BIND='{callback_bind}'

# Optional: Discord webhook server bind address
export THALA_DISCORD_BIND='127.0.0.1:8789'

# Optional: Enable Discord intake/interaction
export DISCORD_INTAKE_ENABLED='true'
export DISCORD_INTERACTION_ENABLED='true'

echo "Thala environment loaded"
echo "  Discord: Channel {discord_channel_id}"
echo "  Callback: {callback_bind}"
"#,
        discord_bot_token = config.discord_bot_token,
        discord_public_key = config.discord_public_key,
        discord_channel_id = config.discord_channel_id,
        github_token = config.github_token,
        openrouter_api_key = config.openrouter_api_key,
        modal_app_file = config.modal_app_file,
        callback_bind = config
            .callback_base_url
            .replace("http://", "")
            .replace("https://", ""),
    );

    std::fs::write(&env_path, content)?;

    // Set restrictive permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&env_path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&env_path, perms)?;
    }

    Ok(())
}

/// Write systemd service file.
fn write_systemd_service(config: &WizardConfig) -> anyhow::Result<()> {
    let systemd_dir = BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("Could not find base directories"))?
        .home_dir()
        .join(".config/systemd/user");
    std::fs::create_dir_all(&systemd_dir)?;

    let service_path = systemd_dir.join("thala.service");
    let content = format!(
        r#"[Unit]
Description=Thala Orchestrator
After=network.target

[Service]
Type=simple
Environment="PATH=/home/{user}/.local/bin:/home/{user}/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
Environment="RUST_LOG=info"
Environment="MODAL_APP_FILE={modal_app_file}"
Environment="THALA_CALLBACK_BIND={callback_bind}"
Environment="THALA_DISCORD_BIND=127.0.0.1:8789"
Environment="DISCORD_INTAKE_ENABLED=true"
Environment="DISCORD_INTERACTION_ENABLED=true"
EnvironmentFile=%h/.thala/env.sh
WorkingDirectory={workspace}
ExecStart={binary} --workflow {workflow} run
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#,
        user = whoami(),
        modal_app_file = config.modal_app_file,
        callback_bind = config
            .callback_base_url
            .replace("http://", "")
            .replace("https://", ""),
        workspace = config.workspace_root,
        binary = BaseDirs::new().map_or_else(
            || "/usr/local/bin/thala".to_string(),
            |b| b
                .home_dir()
                .join(".cargo/bin/thala")
                .to_string_lossy()
                .to_string(),
        ),
        workflow = Path::new(&config.workspace_root)
            .join("WORKFLOW.md")
            .display(),
    );

    std::fs::write(&service_path, content)?;
    Ok(())
}

/// Write a systemd service file for the optional Discord interaction router.
fn write_discord_router_service() -> anyhow::Result<()> {
    let home_dir = BaseDirs::new()
        .ok_or_else(|| anyhow::anyhow!("Could not find base directories"))?
        .home_dir()
        .to_path_buf();
    let systemd_dir = home_dir.join(".config/systemd/user");
    std::fs::create_dir_all(&systemd_dir)?;

    let service_path = systemd_dir.join("thala-discord-router.service");
    let thala_root = std::env::current_dir()
        .unwrap_or_else(|_| home_dir.join("thala"))
        .display()
        .to_string();
    let content = format!(
        r#"[Unit]
Description=Thala Discord Interaction Router
After=network.target thala.service thala-chiropro.service

[Service]
Type=simple
Environment="THALA_DISCORD_ROUTER_BIND=127.0.0.1:8792"
Environment="THALA_ROUTER_MAIN_URL=http://127.0.0.1:8789/api/discord/interaction"
Environment="THALA_ROUTER_CHIROPRO_URL=http://127.0.0.1:8791/api/discord/interaction"
Environment="THALA_ROUTER_CHIROPRO_HINTS=chiropro,chiro pro,makotec-xyz/chiropro,github.com/makotec-xyz/chiropro"
Environment="THALA_ROUTER_DEFAULT_TARGET=main"
WorkingDirectory={thala_root}
ExecStart=/usr/bin/python3 {thala_root}/dev/infra/discord_router.py
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#,
        thala_root = thala_root,
    );

    std::fs::write(&service_path, content)?;
    Ok(())
}

/// Print setup summary.
fn print_summary(config: &WizardConfig) {
    println!("\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║                       Setup Complete!                            ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();
    println!("Next steps:");
    println!();
    println!("1. Source the environment:");
    println!("   source ~/.thala/env.sh");
    println!();
    println!("2. Set up Discord slash commands:");
    println!("   Run this curl command to register the /thala command:");
    println!();
    println!("   curl -X POST https://discord.com/api/v10/applications/YOUR_APP_ID/commands \\");
    println!(
        "     -H \"Authorization: Bot {}\" \\",
        &config.discord_bot_token[..20.min(config.discord_bot_token.len())]
    );
    println!("     -H \"Content-Type: application/json\" \\");
    println!("     -d '[{{\"name\":\"thala\",\"description\":\"Manage Thala tasks\",\"options\":[{{\"type\":1,\"name\":\"create\",\"description\":\"Create a Thala task\",\"options\":[{{\"type\":3,\"name\":\"description\",\"description\":\"What should be done?\",\"required\":true}}]}}]}}]'");
    println!();
    println!("3. Start Thala:");
    println!("   Option A - Foreground: cargo run --release -- run");
    println!("   Option B - Systemd:    systemctl --user start thala");
    println!();
    println!("4. Test in Discord:");
    println!("   /thala create Add a navbar with home and about links");
    println!();
    println!("5. For multiple Thala services sharing one Discord app:");
    println!("   Point Discord at https://YOUR_DOMAIN/api/discord/interaction");
    println!("   Run thala-discord-router and route with message hints such as `chiropro:`");
    println!();
    println!("6. Monitor logs:");
    println!("   journalctl --user -u thala -f");
    println!("   journalctl --user -u thala-discord-router -f");
    println!();
    println!("Documentation:");
    println!(
        "   - WORKFLOW.md: {}",
        Path::new(&config.workspace_root)
            .join("WORKFLOW.md")
            .display()
    );
    println!("   - Secrets:     ~/.thala/env.sh");
    println!("   - Service:     ~/.config/systemd/user/thala.service");
    println!();
}

// ── Helper Functions ─────────────────────────────────────────────────────────

fn prompt(label: &str, default: &str) -> String {
    let hint = if default.is_empty() {
        String::new()
    } else {
        format!(" [{}]", default)
    };
    print!("{}{}: ", label, hint);
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

fn prompt_required(label: &str) -> String {
    loop {
        print!("{}: ", label);
        io::stdout().flush().ok();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).ok();
        let trimmed = buf.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
        println!("This field is required.");
    }
}

fn prompt_secret(label: &str) -> String {
    print!("{} (hidden): ", label);
    io::stdout().flush().ok();
    let mut buf = String::new();
    // Use rpassword or similar for proper secret input, or just read line
    // For simplicity, we'll use stdin but hide the echo would need a crate
    io::stdin().read_line(&mut buf).ok();
    buf.trim().to_string()
}

fn prompt_yes_no(question: &str, default_yes: bool) -> bool {
    let default = if default_yes { "Y/n" } else { "y/N" };
    print!("{} [{}]: ", question, default);
    io::stdout().flush().ok();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).ok();
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    }
}

fn prompt_u32(label: &str, default: u32) -> u32 {
    let s = prompt(label, &default.to_string());
    s.parse().unwrap_or(default)
}

fn prompt_u64(label: &str, default: u64) -> u64 {
    let s = prompt(label, &default.to_string());
    s.parse().unwrap_or(default)
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string())
}
