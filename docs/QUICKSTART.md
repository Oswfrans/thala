# Quickstart

This gets Thala running locally as a CLI agent first. The Beads/Notion task
orchestrator, OpenCode workers, Discord/Telegram escalation, systemd service,
and remote worker backends are covered in [THALA_SETUP.md](THALA_SETUP.md).

## 1. Build Thala

```bash
git clone https://github.com/oswfrans/thala.git
cd thala
cargo build --release
```

Use the local binary while you are trying the repo:

```bash
./target/release/thala --help
```

## 2. Create a Local Config

For the most guided path, run the wizard:

```bash
./target/release/thala onboard
```

The wizard asks for a provider, model, memory backend, optional channels, and
optional orchestrator setup.

Choose based on what you want today:

| Goal | Onboarding choice |
|---|---|
| Try Thala as a local CLI agent | Skip orchestrator setup. |
| Run the full development-task orchestrator with Beads | Enable orchestrator setup and have your product repo path ready. |
| Run the full development-task orchestrator with Notion | Enable orchestrator setup, choose Notion, and have your Notion token, database ID, and product repo path ready. |

If you skip the orchestrator on the first run, you can enable it later by editing
`~/.thala/config.toml` or rerunning onboarding with `--force`.

For a fast hosted setup, pass the provider and key directly:

```bash
export OPENCODE_API_KEY="sk-..."
./target/release/thala onboard \
  --provider opencode \
  --api-key "${OPENCODE_API_KEY:?set OPENCODE_API_KEY first}" \
  --memory sqlite
```

This uses sane local defaults: encrypted secrets, SQLite memory, supervised
workspace-scoped tool access, no public tunnel, and the orchestrator disabled.
Use the interactive wizard or [THALA_SETUP.md](THALA_SETUP.md) when you want
Beads/Notion task dispatch and worker PR automation.

For a local-only model path, start Ollama first and use:

```bash
ollama serve
ollama pull llama3.2
./target/release/thala onboard --provider ollama --memory sqlite
```

Good first providers:

| Provider | Setup |
|---|---|
| `opencode` | Default hosted path. Set `OPENCODE_API_KEY`, then pass `--api-key`. |
| `openrouter` | Flexible hosted routing. Set `OPENROUTER_API_KEY`, then pass `--api-key`. |
| `anthropic` | Direct Anthropic API. Set `ANTHROPIC_API_KEY`, then pass `--api-key`. |
| `ollama` | Local model path. Start Ollama separately; no hosted API key required. |

Thala writes config to `~/.thala/config.toml` and a starter workspace to
`~/.thala/workspace` unless you set `THALA_CONFIG_DIR` or `THALA_WORKSPACE`.

## 3. Send One Message

```bash
./target/release/thala agent -m "Hello, Thala. Give me a one-sentence status check."
```

If you chose a hosted provider and did not pass an API key during onboarding,
either export the provider key before running the agent or edit
`~/.thala/config.toml`.

## 4. Check the Install

```bash
./target/release/thala status
./target/release/thala doctor
```

`status` shows the active config, provider, model, memory backend, and security
mode. `doctor` checks local configuration, workspace state, storage, tools, and
daemon/channel freshness.

## 5. Optional: Start the Local Gateway

The gateway exposes local HTTP/WebSocket endpoints for webhooks, dashboards, and
programmatic clients.

```bash
./target/release/thala gateway start
```

By default it binds locally and requires pairing. In another terminal:

```bash
./target/release/thala gateway get-paircode
```

Use the daemon only when you want a long-running runtime:

```bash
./target/release/thala daemon
```

## 6. Next: Use Thala as a Development-Task Orchestrator

After the CLI agent works, configure the development-task orchestration stack:

1. Rerun `./target/release/thala onboard --force` and answer yes to orchestrator setup, or edit `~/.thala/config.toml`.
2. Initialize Beads in the product repo with `bd init`, or configure Notion if you prefer the hosted tracker.
3. Put a `WORKFLOW.md` in each product repo.
4. Install worker prerequisites such as `bd`, `tmux`, `gh`, `gcloud`, and `opencode`.
5. Choose the local, Modal, or Cloudflare worker backend.
6. Run Thala as a daemon or service.

Use [THALA_SETUP.md](THALA_SETUP.md) for the full production setup.
