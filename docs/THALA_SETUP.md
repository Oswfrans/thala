# Thala Production Setup Guide

For a first local run, start with [QUICKSTART.md](QUICKSTART.md). This guide is
for running Thala as a Beads-backed development-task orchestrator with local or
remote worker backends and human escalation.

## Production quick start

```bash
# 1. Install system deps for the chosen backend
bash dev/setup.sh

# 2. Build
cargo build --release

# 3. Generate a product WORKFLOW.md
./target/release/thala onboard

# 4. Validate the generated workflow
./target/release/thala --workflow /path/to/your-app/WORKFLOW.md validate

# 5. Run the orchestrator
./target/release/thala --workflow /path/to/your-app/WORKFLOW.md run
```

The current binary does not install a service or expose `doctor`, `status`,
`agent`, or `gateway` commands. Run it under your process supervisor of choice
for production.

## System dependencies

Run `bash dev/setup.sh` to install/check the common tools. It installs `bd` when missing, and for the local backend it installs `opencode` when missing. For the local backend you need `git`, `bd`, `gh`, `tmux`, and `opencode` on PATH. Remote backends also need the credentials documented in their workflow blocks below.

## Beads setup

Beads is the supported tracker. It stores issues in the product repo under
`.beads/` and is accessed through the `bd` CLI.

```bash
cd /path/to/your-app
bd init --quiet
bd create "Add a GET /hello endpoint" \
  --description 'Acceptance Criteria:
- GET /hello returns {"message":"hello"}
- Existing tests still pass'
```

Tasks without acceptance criteria are skipped. Thala onboarding and runtime preflight initialize `.beads/` automatically when `bd` is installed and the configured workspace exists. Set `THALA_AUTO_INIT_BEADS=false` to require manual `bd init`. Set `THALA_AUTO_INSTALL_TOOLS=true` only when you explicitly want runtime preflight to install missing CLIs.

## WORKFLOW.md

Each product repo needs a `WORKFLOW.md` with YAML front matter and a Tera prompt
body. Missing template variables fail dispatch instead of rendering empty text.

### Local backend

```yaml
---
product: "example-app"
github_repo: "example/example-app"

tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /path/to/your-app
  beads_ready_status: open

execution:
  backend: local
  workspace_root: /path/to/your-app
  github_token_env: THALA_GITHUB_TOKEN

hooks:
  after_create: ""
  before_run: "git pull --rebase --autostash origin main"
  after_run: "npm test"
  before_cleanup: ""

models:
  worker: "opencode/kimi-k2.5"
  manager: "anthropic/claude-opus-4-6"
  max_review_cycles: 2

limits:
  max_concurrent_runs: 2
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
---
You are an expert developer working on **{{ product_name }}**.

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

When complete, write `DONE` to `.thala/signals/{{ issue.identifier }}.signal`.
```

## Remote backends

`opencode-zen`, `modal`, and `cloudflare` are configured through the same
`execution.backend` field. Remote backends push a task branch to GitHub before
launching. Set `THALA_GITHUB_TOKEN` when the selected backend requires GitHub
credentials.

For callback-based backends, Thala listens on `THALA_CALLBACK_BIND`
(default `127.0.0.1:8788`) and exposes `POST /api/worker/callback`. Put your
reverse proxy or tunnel URL in `execution.callback_base_url`.

### OpenCode Zen

```yaml
execution:
  backend: opencode-zen
  workspace_root: /path/to/your-app
  callback_base_url: "https://thala.yourdomain.com"
  github_token_env: THALA_GITHUB_TOKEN
```

Required env vars:

```ini
OPENCODE_API_KEY=sk-...
THALA_GITHUB_TOKEN=ghp_...
THALA_CALLBACK_BIND=127.0.0.1:8788
```

### Modal

```yaml
execution:
  backend: modal
  workspace_root: /path/to/your-app
  callback_base_url: "https://thala.yourdomain.com"
  github_token_env: THALA_GITHUB_TOKEN
```

Required env vars:

```ini
THALA_GITHUB_TOKEN=ghp_...
THALA_CALLBACK_BIND=127.0.0.1:8788
MODAL_APP_FILE=dev/infra/modal_worker.py::run_worker
```

### Cloudflare

```yaml
execution:
  backend: cloudflare
  workspace_root: /path/to/your-app
  github_token_env: THALA_GITHUB_TOKEN
```

Required env vars:

```ini
THALA_CF_BASE_URL=https://thala-cloudflare-control-plane.<subdomain>.workers.dev
THALA_CF_TOKEN=<shared Worker bearer token>
THALA_GITHUB_TOKEN=ghp_...
```

Deploy the control plane from `cloudflare/control-plane` with Wrangler and store
sensitive Worker values as Cloudflare secrets.

## Security notes

- Do not commit real tokens or customer data.
- Keep credentials in environment variables or your process supervisor, not in
  `WORKFLOW.md`.
- `thala-core` is permanently human-reviewed and must not be auto-merged.
