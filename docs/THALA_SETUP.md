# Thala Production Setup Guide

For a first local run, start with [QUICKSTART.md](QUICKSTART.md). This guide is
for running Thala as a development-task orchestrator with Beads by default,
optional Notion support, worker backends, daemon/service management, and
escalation channels.

## Production quick start

```bash
# 1. Install system deps + fix systemd PATH in one shot
bash dev/setup.sh

# 2. Build
cargo build --release

# 3. Run onboarding and enable orchestrator setup when prompted
./target/release/thala onboard

# 4. Review generated config and fill any remaining production-only values
$EDITOR ~/.thala/config.toml

# 5. Install and start the daemon
./target/release/thala service install
systemctl --user start thala

# 6. Verify everything wired up correctly
./target/release/thala doctor
```

During onboarding, answer yes to orchestrator setup for the power use case:
Beads or Notion task intake, product workspace dispatch, OpenCode worker
execution, GitHub PR creation, validation, retries, and escalation. Beads is the
default tracker and needs no API key. Onboarding collects the product slug and
product workspace path, plus Notion credentials only if you choose Notion. This
guide covers the remaining production details such as Beads setup, optional
Notion schema, `WORKFLOW.md`, worker prerequisites, daemon environment
variables, and remote worker backends.

---

## System dependencies

Run `bash dev/setup.sh` — it installs and validates all of these:

| Tool | Local | OpenCode Zen | Cloudflare | Modal | Notes |
|------|:---:|:---:|:---:|:---:|-------|
| `tmux` | Required | Not needed | Not needed | Not needed | Worker sessions run inside tmux |
| `git` | Required | Required | Required | Required | Worktree / branch management |
| `bd` | Required | Required | Required | Required | Beads CLI for the default task tracker |
| `gh` | Required | Required | Required | Required | PR creation + CI status. Run `gh auth login` once |
| `gcloud` | Required | Required | Required | Required | GCP deployments. Run `gcloud auth login` once |
| `opencode` | Required | Not needed | Not needed | Not needed | Worker agent binary — runs inside the remote container |
| `modal` | Not needed | Not needed | Not needed | Required | Modal CLI. `pip install modal && modal setup` |
| Your build tools | Depends on hooks | Not needed | Not needed | Not needed | Whatever your `before_run` hooks call (e.g. `bun`, `npm`, `cargo`) |

> **Note on build tools:** Thala itself has no dependency on `bun`, `npm`, or any language runtime. The build tools in the table above are driven entirely by the `hooks` you define in your product's `WORKFLOW.md`. If your hooks run `bun install` you need `bun`; if they run `cargo test` you need Rust. Install whatever your hooks require.

All tools must be reachable by the **systemd service**. `dev/setup.sh` adds
`~/.opencode/bin` and `~/.bun/bin` to the service `Environment=PATH=...` automatically — edit the service file if your tools live elsewhere.

---

## Setting up Beads

Beads is Thala's default task tracker. It stores issues in the product repo
under `.beads/` and is accessed through the `bd` CLI. There is no hosted API key
for the default tracker path.

Install Beads and initialize it in each product repo:

```bash
# Install Beads using the upstream instructions for your platform.
# Then, in the product repo:
cd /path/to/your-app
bd init
```

Create tasks with enough structure for workers:

```bash
bd create "Add a GET /hello endpoint" \
  --description "Context: keep the endpoint small and covered by tests.

Acceptance Criteria:
- GET /hello returns {\"message\":\"hello\"}
- Existing tests still pass"
```

Tasks must include acceptance criteria in the description. Thala skips tracker
tasks that do not have acceptance criteria.

## Optional: using Notion instead of Beads

Use Notion when you want a hosted database and the Notion-specific Discord intake
flow. Follow these steps once per deployment.

### 1. Create a Notion integration

1. Go to [notion.so/my-integrations](https://www.notion.so/my-integrations) and click **New integration**.
2. Give it a name (e.g. "Thala"), select the workspace, and set capabilities to **Read content**, **Update content**, and **Insert content**.
3. Copy the **Internal Integration Token** — this is your `NOTION_API_TOKEN` (`ntn_...`).

### 2. Create the tasks database

Create a new Notion database (full-page or inline) with exactly these properties:

| Property name       | Type     | Notes |
|---------------------|----------|-------|
| Title               | Title    | Built-in — rename to "Title" if needed |
| Status              | Select   | Options: **Todo**, **Ready**, **In Progress**, **Done**, **Blocked** |
| Product             | Select   | One entry per product you'll run Thala on |
| Priority            | Select   | Options: **P0**, **P1**, **P2**, **P3** |
| Model               | Select   | Optional per-task model override |
| PR                  | Number   | GitHub PR number — Thala fills this in |
| Context             | Text     | Background notes for the worker |
| Acceptance Criteria | Text     | **Required** — Thala skips tasks without this |
| Always Human Review | Checkbox | Billing, auth, migrations — never auto-merged |
| Attempt             | Number   | Retry count — Thala manages this |

> **Important:** Tasks missing `Acceptance Criteria` are silently skipped by Thala. Fill it in for every task you want dispatched.

### 3. Share the database with your integration

1. Open the database in Notion.
2. Click **…** (top right) → **Connections** → **Connect to** → select your integration.

### 4. Get the database ID

The database ID is the 32-character hex string in the database URL:

```
https://notion.so/myworkspace/a1b2c3d4e5f6...?v=...
                              ^^^^^^^^^^^^^^^^ ← this is the database ID
```

Copy it and use it as `database_id` in your config and WORKFLOW.md.

---

## Config file (`~/.thala/config.toml`)

```toml
default_provider    = "opencode"
default_model       = "opencode/claude-sonnet-4-6"
api_key             = "sk-xxxx"        # OpenCode Zen key (opencode.ai/settings)
default_temperature = 0.7

[orchestrator]
enabled                = true
discord_intake_enabled = false         # Notion-only intake; Beads is default
product                = "example-app"    # Product slug written to tracker tasks
workspace_root         = "/path/to/your-app"
planning_model         = "claude-sonnet-4-6"        # bare name — OpenCode Zen API
api_base_url           = "https://opencode.ai/zen/v1"

[notion]
enabled         = false                # Set true only when using tracker.backend=notion
api_key         = ""
database_id     = ""
status_property = "Status"
input_property  = "Input"
result_property = "Result"
status_pending  = "Todo"
status_running  = "In Progress"
status_done     = "Done"

[channels_config.discord]
bot_token     = "your-discord-bot-token"
allowed_users = ["*"]                  # "*" = anyone; or list Discord user IDs
mention_only  = false
```

**Key provider note:** Thala defaults to **OpenCode Zen** (`opencode.ai/zen/v1`) and also supports OpenRouter.
For OpenCode Zen defaults, keep the top-level `api_key` and `[orchestrator].api_base_url`
pointed at OpenCode Zen. Model names in this config use bare names (`claude-sonnet-4-6`);
model names in `WORKFLOW.md` use full worker IDs (`opencode/kimi-k2.5`).

If you prefer OpenRouter, set `default_provider = "openrouter"` and provide `OPENROUTER_API_KEY`
in your runtime environment.

---

## WORKFLOW.md — one per product repo

Place at the root of each product workspace (e.g. `/path/to/your-app/WORKFLOW.md`).

Each `WORKFLOW.md` has **two distinct sections** separated by `---` delimiters:

1. **YAML front matter** (between the first `---` and second `---`) — configuration read by Thala: tracker settings, workspace path, hooks, agent limits, model selection, polling interval, and merge policy.
2. **Tera template body** (everything after the second `---`) — the prompt rendered and sent to the OpenCode worker for each task. Variables like `{{ issue.identifier }}` are substituted at dispatch time. Missing variables cause a dispatch error and a `#thala-alerts` warning — they are never silently emptied.

Think of the front matter as "instructions for Thala" and the template body as "instructions for the worker."

### Quickstart — let the wizard do it

Run the interactive wizard instead of writing WORKFLOW.md by hand:

```bash
./target/release/thala onboard
```

The wizard asks for your product name, workspace path, tracker, backend choice, and
Discord details, then generates a ready-to-use WORKFLOW.md. Run
`thala validate --workflow path/to/WORKFLOW.md` afterwards to confirm it parses.

---

### Local backend (default)

Workers run as tmux sessions on the same host. No extra credentials needed.

```yaml
---
tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /path/to/your-app
  beads_ready_status: open

workspace:
  root: /path/to/your-app

hooks:
  after_create: "bun install"
  before_run: "git pull --rebase --autostash origin main"   # --autostash required — see note below
  after_run: "bun run typecheck && bun run lint"
  before_remove: ""

worker:
  backend: local   # default — omit this block entirely if local is fine

agent:
  max_concurrent_agents: 2
  stall_timeout_ms: 1800000
  max_retries: 3
  model_default:    "opencode/kimi-k2.5"
  model_hard_tasks: "opencode/claude-sonnet-4-6"

polling:
  interval_ms: 60000
---
You are an expert TypeScript/Bun developer working on **ExampleApp**.

## Task

**ID:** {{ issue.identifier }}
**Title:** {{ issue.title }}
**Attempt:** {{ issue.attempt }}

## Acceptance Criteria

{{ issue.acceptance_criteria }}

{% if issue.context %}
## Context

{{ issue.context }}
{% endif %}

When complete, write `DONE` to `.thala/signals/{{ issue.identifier }}.signal`.
```

To use Notion instead, set the tracker block to:

```yaml
tracker:
  backend: notion
  database_id: "notion-db-id"
  active_states: ["Ready"]
  terminal_states: ["Done", "Cancelled"]
```

**Critical:** `before_run` must use `--autostash`. The `after_create` hook (`bun install`)
writes `bun.lockb` into the worktree before `before_run` runs. Without `--autostash`,
`git pull --rebase` refuses with "You have unstaged changes".

---

### OpenCode Zen backend

Workers run as managed sessions on OpenCode Zen's infrastructure. No container
image to build, no separate CLI to install — Zen handles execution end-to-end.

`tmux` and `opencode` are **not** required on the Thala host.

```yaml
---
tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /path/to/your-app
  beads_ready_status: open

workspace:
  root: /path/to/your-app

execution:
  backend: opencode-zen
  callback_base_url: "https://thala.yourdomain.com"   # public URL of Thala's gateway
  github_token_env: THALA_GITHUB_TOKEN

hooks:
  before_run: "git pull --rebase --autostash origin main"
  after_run: ""

models:
  worker: "opencode/kimi-k2.5"
  manager: "anthropic/claude-opus-4-6"
  max_review_cycles: 2

limits:
  max_concurrent_runs: 5
  stall_timeout_ms: 3600000

retry:
  max_attempts: 3
---
(same Tera prompt template as local backend)
```

Required environment variables:

```ini
Environment="OPENCODE_API_KEY=sk-..."
Environment="THALA_GITHUB_TOKEN=ghp_xxxxxxxxxxxx"
Environment="THALA_CALLBACK_SECRET=<openssl rand -hex 32>"
```

Optional:

```ini
Environment="OPENCODE_ZEN_BASE_URL=https://opencode.ai/zen/v1"   # override for private deployments
```

The Thala gateway **must be publicly reachable** at `callback_base_url` so Zen
can POST completion signals. If running locally, use:

```bash
thala gateway tunnel cloudflare
```

Per-task backend override via task label: `backend:opencode-zen` or the alias `backend:zen`.

---

### Modal backend

Workers run as serverless Modal containers. Thala pushes a task branch to GitHub,
Modal clones it, runs OpenCode inside the container, pushes changes back, and calls
the Thala gateway callback endpoint when done.

`tmux` and `opencode` are **not** required on the Thala host.

```yaml
---
tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /path/to/your-app
  beads_ready_status: open

workspace:
  root: /path/to/your-app

hooks:
  after_create: "bun install"
  before_run: "git pull --rebase --autostash origin main"
  after_run: ""
  before_remove: ""

worker:
  backend: modal
  modal:
    app_file: dev/infra/modal_worker.py    # path relative to this repo
    function_name: run_worker
    timeout_secs: 3600
    # gpu: "T4"                        # uncomment for GPU-heavy tasks
  callback_base_url: "https://thala.yourdomain.com"   # public URL of Thala's gateway
  callback_secret_env: THALA_CALLBACK_SECRET           # env var holding the shared secret
  github_repo: "example/example-app"
  github_token_env: THALA_GITHUB_TOKEN                 # env var holding a GitHub PAT

agent:
  max_concurrent_agents: 5    # Modal can scale more freely than local tmux
  stall_timeout_ms: 3600000
  max_retries: 3
  model_default:    "opencode/kimi-k2.5"
  model_hard_tasks: "opencode/claude-sonnet-4-6"

polling:
  interval_ms: 60000
---
(same Tera prompt template as above)
```

Additional env vars required in the systemd service (see [Required environment variables](#required-environment-variables-systemd)):

```
THALA_GITHUB_TOKEN=ghp_xxxxxxxxxxxx
THALA_CALLBACK_SECRET=<random 32-byte hex string>
```

The Thala gateway **must be publicly reachable** at `callback_base_url` so Modal containers
can POST their completion signal. If running behind a firewall, use a tunnel
(e.g. `thala gateway tunnel cloudflare`).

---

### Cloudflare Containers backend

Workers run as Cloudflare Containers. Same push/callback flow as Modal.

```yaml
worker:
  backend: cloudflare
  cloudflare:
    image: "registry.example.com/thala-worker:latest"   # your built Dockerfile.worker image
    cpu: 1
    memory_mb: 2048
  callback_base_url: "https://thala.yourdomain.com"
  callback_secret_env: THALA_CALLBACK_SECRET
  github_repo: "example/example-app"
  github_token_env: THALA_GITHUB_TOKEN
```

Required environment variables:

```ini
Environment="CF_ACCOUNT_ID=your-cloudflare-account-id"
Environment="CF_API_TOKEN=your-cloudflare-api-token"
Environment="CF_WORKER_IMAGE=registry.example.com/thala-worker:latest"
Environment="THALA_GITHUB_TOKEN=ghp_xxxxxxxxxxxx"
Environment="THALA_CALLBACK_SECRET=<openssl rand -hex 32>"
```

Optional resource overrides (both default to Cloudflare's platform defaults when unset):

```ini
Environment="CF_WORKER_VCPUS=1"
Environment="CF_WORKER_MEMORY_MB=2048"
```

Thala reads `CF_ACCOUNT_ID`, `CF_WORKER_IMAGE`, `CF_WORKER_VCPUS`, and
`CF_WORKER_MEMORY_MB` automatically at startup — you do **not** need to set
`cloudflare.account_id` or `cloudflare.image` in WORKFLOW.md unless you want to
override per-workflow.

Build and push `Dockerfile.worker` to a container registry accessible by Cloudflare before
enabling this backend.

---

## Required environment variables (systemd)

Set these in `~/.config/systemd/user/thala.service` under `[Service]`.

> **Credentials: two mechanisms, one precedence rule.** You can put credentials in `~/.thala/config.toml` (the `api_key` field, or `[notion] api_key` when using Notion) for local dev and single-shot runs. For the daemon, use `Environment=` in the systemd unit file instead — the env var always takes precedence over the config file value. Do not put real secrets in config.toml for production; use the systemd unit or a secrets manager that exports env vars.

### Always required

```ini
Environment="OPENCODE_API_KEY=sk-xxxx"
Environment="DISCORD_ALERTS_WEBHOOK=https://discord.com/api/webhooks/..."
Environment="TELEGRAM_BOT_TOKEN=..."
Environment="TELEGRAM_ESCALATION_CHAT_IDS=123456"
Environment="GCP_PROJECT=your-project"
Environment="GCP_REGION=europe-west4"
```

When using Notion instead of Beads, also set:

```ini
Environment="NOTION_API_TOKEN=ntn_xxxxxxxxxxxx"
```

### Modal backend (additional)

```ini
Environment="THALA_GITHUB_TOKEN=ghp_xxxxxxxxxxxx"
Environment="THALA_CALLBACK_SECRET=<random 32-byte hex>"
# OPENROUTER_API_KEY is forwarded to the container automatically if set
Environment="OPENROUTER_API_KEY=sk-or-xxxx"
```

Generate `THALA_CALLBACK_SECRET` with: `openssl rand -hex 32`

### OpenCode Zen backend (additional)

```ini
Environment="OPENCODE_API_KEY=sk-..."
Environment="THALA_GITHUB_TOKEN=ghp_xxxxxxxxxxxx"
Environment="THALA_CALLBACK_SECRET=<openssl rand -hex 32>"
# Optional: override the Zen API base URL (e.g. for private deployments)
# Environment="OPENCODE_ZEN_BASE_URL=https://opencode.ai/zen/v1"
```

### Cloudflare backend (additional)

```ini
Environment="CF_ACCOUNT_ID=your-cloudflare-account-id"
Environment="CF_API_TOKEN=your-cloudflare-api-token"
Environment="CF_WORKER_IMAGE=registry.example.com/thala-worker:latest"
Environment="THALA_GITHUB_TOKEN=ghp_xxxxxxxxxxxx"
Environment="THALA_CALLBACK_SECRET=<openssl rand -hex 32>"
# Optional resource limits:
# Environment="CF_WORKER_VCPUS=1"
# Environment="CF_WORKER_MEMORY_MB=2048"
```

---

## Discord intake

When `discord_intake_enabled = true`, messages to the bot are routed through a
planning LLM call that structures them into a Notion task with Title,
Acceptance Criteria, and Priority, then replies with a confirmation.

Discord intake is currently Notion-specific. Keep `discord_intake_enabled = false`
when using the default Beads tracker.

Two things that will silently break this:
1. **`allowed_users = []`** — empty list blocks everyone. Use `["*"]` or list specific Discord user IDs.
2. **`discord_intake_enabled = false`** — the default. Must be explicitly set to `true`.

---

## Notion task schema

This section applies only when `tracker.backend = "notion"`. Thala reads these
fields. `Acceptance Criteria` is mandatory — tasks without it are silently skipped.

| Field               | Type     | Notes                                           |
|---------------------|----------|-------------------------------------------------|
| Title               | Text     | Clear, actionable task name                     |
| Status              | Select   | Todo / Ready / In Progress / Done / Blocked     |
| Product             | Select   | One entry per active product                    |
| Priority            | Select   | P0 / P1 / P2 / P3                               |
| Model               | Select   | Override model per task (optional)              |
| PR                  | Number   | GitHub PR number, populated on completion       |
| Context             | Text     | Customer notes, meeting excerpts                |
| Acceptance Criteria | Text     | Mandatory — Thala skips tasks missing this        |
| Always Human Review | Checkbox | Billing, auth, migrations — never auto-merged   |
| Attempt             | Number   | Retry count, managed by Thala                     |

---

## Hard rules (non-configurable)

- Tasks with `product == "thala-core"` always require human review — Thala never auto-merges herself.
- Tasks missing `Acceptance Criteria` are invisible to Thala.
- Template rendering failures skip that task and post a warning to `#thala-alerts`.

---

## Verifying your setup

After starting the daemon, run diagnostics:

```bash
thala doctor          # quick checks: config, workspace, SQLite, tool registry
thala doctor full     # also checks gateway health and memory round-trip
thala status          # live daemon/channel/scheduler status
```

Check daemon logs if something is wrong:

```bash
journalctl --user -u thala -f          # follow live logs
journalctl --user -u thala -n 100      # last 100 lines
```

---

## Your first task — end-to-end walkthrough

Once the daemon is running and `thala doctor` passes:

### 1. Create a test task in Beads

In your product repo, create an issue:

```bash
cd /path/to/your-app
bd create "Add a hello-world endpoint" \
  --description "Acceptance Criteria:
- A GET /hello endpoint returns {\"message\":\"hello\"} with status 200"
```

For Notion, create the equivalent row with `Status = Ready`, matching `Product`,
and a filled `Acceptance Criteria` field.

### 2. Wait for the dispatch cycle

Thala polls the tracker every 60 seconds (configurable via `polling.interval_ms` in WORKFLOW.md). Within one poll interval, you should see:

- A Discord message in `#thala-alerts` confirming the task was dispatched
- A new tmux session: `tmux ls` shows `thala-<product>-<task-id>`
- A git worktree created in your product workspace

### 3. Monitor progress

```bash
# Watch the worker's tmux session live
tmux attach -t thala-<product>-<task-id>

# Check signal file (written when worker finishes)
cat /path/to/your-app/.thala/signals/<task-id>.signal

# Check orchestrator state
cat /path/to/your-app/.thala/active-tasks.json
```

### 4. What success looks like

1. Worker writes `DONE` to `.thala/signals/<task-id>.signal`
2. Thala opens a GitHub PR and posts the PR link to `#thala-alerts`
3. CI runs on the PR; Thala checks status via `gh run status`
4. If CI passes and no protected paths were touched, Thala posts to Discord for merge approval
5. Tracker task status updates to `Done` or closed, PR number is recorded when supported

### 5. What to do if a task gets stuck

- **Stall timeout:** Thala posts to `#thala-alerts` and Telegram with the session name and last output. Attach to the tmux session to investigate.
- **Max retries reached:** Task status becomes blocked/closed in the tracker. Fix the underlying issue and reset it to the ready/open state.
- **Template error:** Check `#thala-alerts` for the rendering failure message. Validate your WORKFLOW.md Tera syntax.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| Tasks not dispatched | `Status` not in `active_states` | Check `tracker.active_states` in WORKFLOW.md |
| Tasks silently skipped | Missing `Acceptance Criteria` | Add acceptance criteria to the Beads description or Notion field |
| Worker stalls immediately | `opencode` not on PATH | Run `which opencode`; re-run `dev/setup.sh` |
| Callback never received (Modal/Zen/CF) | Gateway not publicly reachable | Run `thala gateway tunnel cloudflare` |
| `OPENCODE_API_KEY` not found | OpenCode Zen backend selected but key missing | Set `OPENCODE_API_KEY` in the systemd unit |
| `CF_WORKER_IMAGE` empty | Cloudflare backend has no image to deploy | Set `CF_WORKER_IMAGE` env var or WORKFLOW.md field |
| `NOTION_API_TOKEN` not found | Notion tracker selected but env var not in systemd unit | Add `Environment="NOTION_API_TOKEN=..."` to the service file |
