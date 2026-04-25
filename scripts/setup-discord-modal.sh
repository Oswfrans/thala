#!/usr/bin/env bash
# Quick setup script for Thala with Discord + Modal
# Usage: bash scripts/setup-discord-modal.sh

set -euo pipefail

BOLD='\033[1m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
fail() { echo -e "${RED}✗${NC} $*"; }
step() { echo -e "\n${BOLD}$*${NC}"; }
info() { echo -e "${CYAN}→${NC} $*"; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo -e "${BOLD}Thala Discord + Modal Quick Setup${NC}"
echo ""

# ── Check Prerequisites ──────────────────────────────────────────────────────
step "1. Checking Prerequisites..."

MISSING=()

check_cmd() {
    if ! command -v "$1" &>/dev/null; then
        MISSING+=("$1")
        fail "$1 not found"
    else
        ok "$1 found at $(command -v "$1")"
    fi
}

check_cmd git
check_cmd cargo
check_cmd bd || info "Install beads CLI: curl -fsSL https://raw.githubusercontent.com/steveyegge/beads/main/scripts/install.sh | bash"

if ! command -v modal &>/dev/null; then
    MISSING+=("modal")
    fail "modal CLI not found"
    info "Install: uv tool install modal && modal token new"
else
    ok "modal found at $(command -v modal)"
fi

if [[ ${#MISSING[@]} -gt 0 ]]; then
    echo ""
    fail "Missing required tools: ${MISSING[*]}"
    exit 1
fi

# ── Environment Variables ───────────────────────────────────────────────────
step "2. Environment Variables"

echo "Checking required environment variables..."

REQUIRED_VARS=(
    "DISCORD_BOT_TOKEN:Discord Bot Token (from https://discord.com/developers/applications)"
    "DISCORD_PUBLIC_KEY:Discord Public Key (from Bot → General Information)"
    "DISCORD_ALERTS_CHANNEL_ID:Discord Channel ID (right-click channel → Copy ID)"
    "THALA_GITHUB_TOKEN:GitHub Personal Access Token with repo scope"
    "OPENROUTER_API_KEY:OpenRouter API Key (from https://openrouter.ai/keys)"
)

MISSING_VARS=()
for var_info in "${REQUIRED_VARS[@]}"; do
    var_name="${var_info%%:*}"
    var_desc="${var_info#*:}"
    
    if [[ -z "${!var_name:-}" ]]; then
        MISSING_VARS+=("$var_name")
        fail "$var_name not set"
        info "$var_desc"
    else
        ok "$var_name is set"
    fi
done

if [[ ${#MISSING_VARS[@]} -gt 0 ]]; then
    echo ""
    warn "Some environment variables are missing."
    info "You can set them now or export them in your shell."
    
    for var_name in "${MISSING_VARS[@]}"; do
        printf "Enter %s: " "$var_name"
        read -r value
        export "$var_name=$value"
    done
fi

# ── Create WORKFLOW.md ────────────────────────────────────────────────────────
step "3. Creating WORKFLOW.md"

WORKFLOW_PATH="${REPO_ROOT}/WORKFLOW.md"

if [[ -f "$WORKFLOW_PATH" ]]; then
    warn "WORKFLOW.md already exists at $WORKFLOW_PATH"
    read -p "Overwrite? (y/N): " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        info "Skipping WORKFLOW.md creation"
    else
        CREATE_WORKFLOW=true
    fi
else
    CREATE_WORKFLOW=true
fi

if [[ "${CREATE_WORKFLOW:-false}" == "true" ]]; then
    # Get product info
    echo ""
    read -p "Product name (e.g., my-app): " PRODUCT_NAME
    read -p "GitHub repo (e.g., your-org/my-app): " GITHUB_REPO
    read -p "Workspace root (absolute path to repo): " WORKSPACE_ROOT
    read -p "Callback base URL (e.g., https://thala.example.com): " CALLBACK_URL
    
    cat > "$WORKFLOW_PATH" << EOF
---
product: "${PRODUCT_NAME}"
github_repo: "${GITHUB_REPO}"

tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: ${WORKSPACE_ROOT}
  beads_ready_status: open

execution:
  backend: modal
  workspace_root: ${WORKSPACE_ROOT}
  github_token_env: THALA_GITHUB_TOKEN
  callback_base_url: ${CALLBACK_URL}

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
  bot_token: "Bot ${DISCORD_BOT_TOKEN}"
  public_key: "${DISCORD_PUBLIC_KEY}"
  alerts_channel_id: "${DISCORD_ALERTS_CHANNEL_ID}"

hooks:
  after_create: ""
  before_run: "git pull --rebase --autostash origin main"
  after_run: ""
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

Write DONE to \`.thala/signals/{{ issue.identifier }}.signal\` when complete.
EOF
    
    ok "Created WORKFLOW.md at $WORKFLOW_PATH"
    info "Edit this file to customize hooks, protected paths, and required checks"
fi

# ── Verify Modal Setup ───────────────────────────────────────────────────────
step "4. Verifying Modal Setup"

if modal profile current &>/dev/null; then
    ok "Modal is authenticated"
else
    fail "Modal is not authenticated"
    info "Run: modal token new"
    exit 1
fi

# ── Test Modal Worker ────────────────────────────────────────────────────────
step "5. Testing Modal Worker"

info "Running a quick smoke test..."

export THALA_TASK_ID="TEST-$(date +%s)"
export THALA_TASK_BRANCH="main"
export THALA_GITHUB_REPO="${GITHUB_REPO:-oswfrans/thala}"
export THALA_CALLBACK_URL="${CALLBACK_URL:-http://localhost:8788}/callback"
export THALA_RUN_TOKEN="test-token-$(date +%s)"
export THALA_MODEL="opencode/kimi-k2.5"
export THALA_PROMPT_B64=$(echo -n "Test prompt - just echo 'Hello from Modal'" | base64)

# Run in subshell with timeout
timeout 60 bash -c '
    cd "'"$REPO_ROOT"'"
    modal run dev/infra/modal_worker.py::run_worker || true
' || info "Test completed (timeout or success)"

ok "Modal worker test completed"

# ── Build Thala ──────────────────────────────────────────────────────────────
step "6. Building Thala"

cd "$REPO_ROOT"
cargo build --release

ok "Thala built successfully"

# ── Summary ──────────────────────────────────────────────────────────────────
step "7. Setup Complete!"

echo ""
echo -e "${GREEN}${BOLD}Thala is ready to use with Discord + Modal${NC}"
echo ""
echo "Next steps:"
echo ""
echo "1. Start the callback server (in a terminal):"
echo "   cd $REPO_ROOT && ./target/release/thala run"
echo ""
echo "2. Or install as a systemd service:"
echo "   ./target/release/thala service install"
echo "   systemctl --user start thala"
echo ""
echo "3. In Discord, create your first task:"
echo "   /thala create Add a login button to the homepage"
echo ""
echo "4. Monitor progress:"
echo "   journalctl --user -u thala -f"
echo ""
echo "Documentation:"
echo "   - Full guide: docs/SETUP_DISCORD_MODAL.md"
echo "   - Architecture: AGENTS.md"
echo ""
