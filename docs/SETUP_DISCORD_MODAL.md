# Thala Setup Guide: Discord + Modal

This guide walks you through setting up Thala with Discord for human interactions and Modal as the execution backend for serverless workers.

## Architecture Overview

```
┌─────────────┐     ┌────────────────┐     ┌──────────────┐     ┌─────────────┐
│   Discord   │────▶│ Discord Router │────▶│ Thala Server │────▶│    Modal    │
│  (single    │     │ optional :8792 │     │ per repo     │     │  Workers    │
│ interaction │◄────│ signed forward │◄────│              │◄────│             │
│ endpoint)   │     └────────────────┘     └──────────────┘     └─────────────┘
└─────────────┘                                     │
                                                    ▼
                                             ┌──────────────┐
                                             │    Beads     │
                                             │ per repo     │
                                             └──────────────┘
```

**Discord Roles:**
- **Intake**: Users can create tasks via Discord commands
- **Interaction**: Approval buttons, retry/escalate decisions, stuck notifications
- **Routing**: Optional `discord_router.py` lets one Discord app route to multiple
  Thala services by message hints and task-id prefixes.

**Modal Roles:**
- Executes workers in serverless containers
- Workers run OpenCode with the rendered prompt
- Completion signaled via HTTP callback

---

## Prerequisites

### 1. Discord Bot Setup

1. Go to https://discord.com/developers/applications
2. Create a new application
3. Go to **Bot** section:
   - Click **Reset Token** → Copy the token (save as `DISCORD_BOT_TOKEN`)
   - Enable **MESSAGE CONTENT INTENT** (for command parsing)
4. Go to **General Information**:
   - Copy **Application ID** (for invite URL)
   - Click **Reset Public Key** → Copy the key (save as `DISCORD_PUBLIC_KEY`)
5. Create an invite URL:
   ```
   https://discord.com/oauth2/authorize?client_id=YOUR_APP_ID&permissions=274877974528&scope=bot+applications.commands
   ```
   - `274877974528` = Send Messages, Embed Links, Add Reactions, Read Message History
6. Invite the bot to your server
7. Create a channel for Thala alerts and copy the **Channel ID** (right-click → Copy ID, needs Developer Mode enabled)

### 2. Modal Setup

```bash
# Install uv (recommended)
curl -LsSf https://astral.sh/uv/install.sh | sh

# Install Modal CLI
uv tool install modal

# Authenticate
modal token new
```

### 3. GitHub Token

Create a Personal Access Token at https://github.com/settings/tokens with:
- `repo` scope (full repository access)
- Save as `THALA_GITHUB_TOKEN`

### 4. OpenRouter API Key (for Modal Workers)

Get an API key at https://openrouter.ai/keys
- Save as `OPENROUTER_API_KEY` (used by workers in Modal containers)

### 5. Beads CLI

```bash
# Install Beads CLI (bd)
curl -fsSL https://raw.githubusercontent.com/steveyegge/beads/main/scripts/install.sh | bash

# Initialize your product repo
cd /path/to/your/repo
bd init
```

---

## Step-by-Step Setup

### Step 1: Run the Setup Script

```bash
cd /path/to/thala/repo
bash dev/setup.sh --backend modal --configure
```

When prompted:
- OpenCode Zen API key: Your OpenCode API key
- Product slug: Your product name (e.g., `my-app`)
- Workspace root: Absolute path to your product repo (e.g., `/home/user/my-app`)
- Discord bot token: From step 1 above
- Discord alerts webhook URL: Optional, for general alerts (different from the bot)
- GitHub PAT: From step 3 above
- Callback listen address: `127.0.0.1:8788` (or your public IP with port forwarding)
- OpenRouter API key: From step 4 above

### Step 2: Create WORKFLOW.md

Create a `WORKFLOW.md` file in your product repo root:

```yaml
---
product: "my-app"
github_repo: "your-org/my-app"

tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /path/to/your/repo
  beads_ready_status: open

execution:
  backend: modal
  workspace_root: /path/to/your/repo
  github_token_env: THALA_GITHUB_TOKEN
  callback_base_url: https://your-thala-server.example.com

models:
  worker: "opencode/kimi-k2.5"
  manager: "anthropic/claude-opus-4-6"
  max_review_cycles: 2

limits:
  max_concurrent_runs: 3
  stall_timeout_ms: 300000

retry:
  max_attempts: 3
  allow_backend_reroute: false

merge:
  auto_merge: false
  protected_paths:
    - "auth/**"
    - "**/migrations/**"
  required_checks:
    - "ci"

discord:
  bot_token: "Bot <YOUR_DISCORD_BOT_TOKEN>"
  public_key: "YOUR_DISCORD_PUBLIC_KEY"
  alerts_channel_id: "YOUR_DISCORD_CHANNEL_ID"

hooks:
  after_create: "npm install"
  before_run: "git pull --rebase --autostash origin main"
  after_run: "npm run build"
  before_cleanup: ""
---
You are an expert developer working on {{ product_name }}.

## Task

**ID:** {{ issue.identifier }}
**Title:** {{ issue.title }}
**Attempt:** {{ run.attempt }}

## Acceptance Criteria

{{ issue.acceptance_criteria }}

{% if issue.context %}
## Context

{{ issue.context }}
{% endif %}

Write DONE to `.thala/signals/{{ issue.identifier }}.signal` when complete.
```

Lifecycle hooks are trusted shell snippets from `WORKFLOW.md`. They are
operator-owned configuration, not Discord/Beads task input. The Modal worker
executes them through the shell for normal shell semantics; a non-zero hook
exit sends an error callback and stops the run. `before_cleanup` is accepted in
workflow files for compatibility but is not executed by the Modal worker.

### Step 3: Configure Environment Variables

Add these to your systemd service or shell environment:

```bash
# Required for Modal backend
export MODAL_APP_FILE="dev/infra/modal_worker.py::run_worker"
export MODAL_ENVIRONMENT=""  # Optional: specify Modal environment

# Required for Discord interaction
export DISCORD_BOT_TOKEN="YOUR_DISCORD_BOT_TOKEN"
export DISCORD_PUBLIC_KEY="YOUR_DISCORD_PUBLIC_KEY"
export DISCORD_ALERTS_CHANNEL_ID="YOUR_DISCORD_CHANNEL_ID"

# Required for Thala
export THALA_GITHUB_TOKEN="ghp_YOUR_GITHUB_TOKEN"
export THALA_CALLBACK_BIND="127.0.0.1:8788"
export OPENROUTER_API_KEY="sk-or-v1-YOUR_OPENROUTER_KEY"

# Optional: Discord webhook for general alerts
export DISCORD_ALERTS_WEBHOOK="https://discord.com/api/webhooks/..."
```

### Step 4: Test Modal Worker Locally

```bash
# Set up test environment variables
export THALA_TASK_ID="TEST-1"
export THALA_TASK_BRANCH="task/TEST-1"
export THALA_GITHUB_REPO="your-org/my-app"
export THALA_CALLBACK_URL="http://localhost:8788/callback"
export THALA_RUN_TOKEN="test-token"
export THALA_MODEL="opencode/kimi-k2.5"
export THALA_PROMPT_B64=$(echo -n "Write a test file" | base64)
export GITHUB_TOKEN="$THALA_GITHUB_TOKEN"

# Run the worker
modal run dev/infra/modal_worker.py::run_worker
```

### Step 5: Build and Start Thala

```bash
# Build release binary
cargo build --release

# Install systemd service (if not already installed)
./target/release/thala service install

# Start Thala
systemctl --user start thala

# Check status
systemctl --user status thala
journalctl --user -u thala -f
```

### Step 6: Test Discord Intake

In your Discord server, type:
```
/thala create Add a navbar component with home and about links
```

You should see:
1. A reply from the bot with a created task ID
2. The task appearing in your Beads tracker (`bd list`)
3. Thala picking up the task and dispatching it to Modal
4. Discord messages when task needs approval or gets stuck

---

## Multi-Service Discord Routing

Use this when one Discord application should control multiple repos at the same
time. Each repo still gets its own Thala service, ports, `WORKFLOW.md`, Beads
workspace, and `XDG_DATA_HOME`. The router is only the public Discord ingress.

Example layout:

| Service | Callback bind | Discord bind | State root | Route hint |
|---|---:|---:|---|---|
| Main Thala | `127.0.0.1:8788` | `127.0.0.1:8789` | `~/.local/share/thala-main` | default |
| Chiropro | `127.0.0.1:8790` | `127.0.0.1:8791` | `~/.local/share/thala-chiropro` | `chiropro:` |
| Router | n/a | `127.0.0.1:8792` | n/a | public ingress |

Start the router:

```bash
THALA_DISCORD_ROUTER_BIND=127.0.0.1:8792 \
THALA_ROUTER_MAIN_URL=http://127.0.0.1:8789/api/discord/interaction \
THALA_ROUTER_CHIROPRO_URL=http://127.0.0.1:8791/api/discord/interaction \
THALA_ROUTER_CHIROPRO_HINTS="chiropro,chiro pro,makotec-xyz/chiropro" \
THALA_ROUTER_DEFAULT_TARGET=main \
python3 dev/infra/discord_router.py
```

For systemd, copy `dev/infra/thala-discord-router.service` to
`~/.config/systemd/user/`, edit the URLs/hints, then run:

```bash
systemctl --user daemon-reload
systemctl --user enable --now thala-discord-router
```

Proxy Discord's configured interaction URL to the router:

```nginx
location /api/discord/interaction {
    proxy_pass http://127.0.0.1:8792/api/discord/interaction;
    proxy_set_header X-Signature-Ed25519 $http_x_signature_ed25519;
    proxy_set_header X-Signature-Timestamp $http_x_signature_timestamp;
}
```

Keep repo-specific worker callbacks pointed directly at each service, for
example `https://YOUR_DOMAIN/api/worker/callback` for the main service and
`https://YOUR_DOMAIN/chiropro/api/worker/callback` for Chiropro.

## Discord Commands

Users can interact with Thala via these patterns:

### Create a Task
```
/thala create <description>
```

### Add Context to a Task
```
/thala context <task-id> <context>
```
Example:
```
/thala context my-app-42 Also update the footer
```

### Check Task Status
```
/thala status <task-id>
```

---

## Discord Interaction Buttons

When a task needs attention, Thala posts an embed with action buttons:

- **Approve** (green): Merge the PR and mark task as complete
- **Reject** (red): Reject the changes with feedback
- **Retry** (grey): Retry the task from scratch
- **Reroute** (grey): Move to a different backend
- **Escalate** (grey): Alert a human for manual intervention
- **Close** (red): Close/archive the task

Button clicks are verified using Discord's Ed25519 signature validation.

---

## Network Requirements

### For Local Development

The callback server runs on `THALA_CALLBACK_BIND` (default `127.0.0.1:8788`). Modal workers will POST completion to this address. For local testing, you may need:

```bash
# ngrok for exposing local server
ngrok http 8788

# Then set callback URL in WORKFLOW.md:
# callback_base_url: https://your-ngrok-url.ngrok.io
```

### For Production

Ensure your Thala server:
1. Has a public IP or domain
2. Port 8788 (or your chosen port) is open
3. HTTPS is configured (Modal requires HTTPS callbacks in production)
4. The callback URL matches `callback_base_url` in WORKFLOW.md

---

## Troubleshooting

### Discord Bot Not Responding

1. Check bot token is correct
2. Verify MESSAGE CONTENT INTENT is enabled
3. Ensure bot has permissions in the channel
4. Check `DISCORD_ALERTS_CHANNEL_ID` is correct

### Modal Workers Not Starting

```bash
# Check modal authentication
modal profile current

# Verify app file path
echo $MODAL_APP_FILE

# Test modal CLI
modal app list
```

### Callbacks Not Received

1. Verify `THALA_CALLBACK_BIND` is set correctly
2. Check firewall rules
3. Ensure `callback_base_url` in WORKFLOW.md is reachable from Modal
4. Check Thala logs: `journalctl --user -u thala -f`

### Tasks Stuck in "Running"

1. Check Modal app logs: `modal app logs <app-id>`
2. Verify OpenRouter API key is valid
3. Check GitHub token has repo access
4. Ensure task branch exists and is pushable

---

## Monitoring

### Check Thala Status

```bash
# Service status
systemctl --user status thala

# Logs
journalctl --user -u thala -f

# Active runs
sqlite3 ~/.local/share/thala/state.db "SELECT * FROM task_runs WHERE status = 'Running';"
```

### Check Modal Apps

```bash
# List running apps
modal app list

# View logs
modal app logs <app-id>

# Stop a stuck app
modal app stop <app-id>
```

### Check Beads Tasks

```bash
# List all tasks
bd list

# Get task details
bd show <task-id>

# Mark task as ready
bd ready <task-id>
```

---

## Security Notes

1. **Never commit secrets**: All tokens/keys go in environment variables
2. **Discord signature verification**: All interactions are verified with Ed25519
3. **Callback tokens**: Each run has a unique bearer token (only hash stored in DB)
4. **GitHub token**: Use fine-grained PAT with minimal repo scope
5. **Modal isolation**: Workers run in ephemeral containers with no persistence

---

## Next Steps

1. Create your first task via Discord
2. Watch the task flow through Thala → Modal → PR → Discord approval
3. Tune `stall_timeout_ms` and `max_concurrent_runs` based on your workload
4. Set up Cloudflare backend as a fallback for Modal outages

For more details, see:
- `AGENTS.md` — Full architecture documentation
- `examples/WORKFLOW.md` — Example configurations
- `docs/` — Additional documentation
