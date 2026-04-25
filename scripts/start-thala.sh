#!/usr/bin/env bash
# Start Thala with Discord + Modal
# Usage: bash scripts/start-thala.sh [fg|bg]

set -euo pipefail

MODE="${1:-fg}"  # fg = foreground, bg = background

BOLD='\033[1m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
fail() { echo -e "${RED}✗${NC} $*"; }
step() { echo -e "\n${BOLD}$*${NC}"; }
info() { echo -e "${CYAN}→${NC} $*"; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo -e "${BOLD}Thala Discord + Modal Startup${NC}"
echo ""

# ── Check Environment ─────────────────────────────────────────────────────
step "1. Checking Environment"

MISSING=()

for var in DISCORD_BOT_TOKEN DISCORD_PUBLIC_KEY OPENROUTER_API_KEY THALA_GITHUB_TOKEN; do
    if [[ -z "${!var:-}" ]]; then
        MISSING+=("$var")
        fail "$var not set"
    else
        ok "$var is set"
    fi
done

if [[ ${#MISSING[@]} -gt 0 ]]; then
    echo ""
    fail "Missing required environment variables: ${MISSING[*]}"
    echo ""
    echo "Export them first:"
    echo "  export DISCORD_BOT_TOKEN='your_token'"
    echo "  export DISCORD_PUBLIC_KEY='your_key'"
    echo "  export OPENROUTER_API_KEY='your_key'"
    echo "  export THALA_GITHUB_TOKEN='your_token'"
    exit 1
fi

# ── Check Dependencies ──────────────────────────────────────────────────────
step "2. Checking Dependencies"

if ! command -v modal &>/dev/null; then
    fail "modal CLI not found"
    info "Install: uv tool install modal && modal token new"
    exit 1
fi
ok "modal CLI found"

if ! modal profile current &>/dev/null; then
    fail "Modal not authenticated"
    info "Run: modal token new"
    exit 1
fi
ok "Modal authenticated"

if ! command -v bd &>/dev/null; then
    warn "bd (Beads CLI) not found"
    info "Install: curl -fsSL https://raw.githubusercontent.com/steveyegge/beads/main/scripts/install.sh | bash"
else
    ok "bd found"
fi

# ── Build if Needed ────────────────────────────────────────────────────────
step "3. Checking Build"

if [[ ! -f "$REPO_ROOT/target/release/thala" ]]; then
    info "Building Thala..."
    cd "$REPO_ROOT"
    cargo build --release
fi
ok "Thala binary ready"

# ── Initialize State ───────────────────────────────────────────────────────
step "4. Initializing State"

mkdir -p ~/.local/share/thala ~/.thala
ok "State directories ready"

# ── Start Thala ────────────────────────────────────────────────────────────
step "5. Starting Thala"

export MODAL_APP_FILE="${MODAL_APP_FILE:-dev/infra/modal_worker.py::run_worker}"
export THALA_CALLBACK_BIND="${THALA_CALLBACK_BIND:-127.0.0.1:8788}"
export DISCORD_ALERTS_CHANNEL_ID="${DISCORD_ALERTS_CHANNEL_ID:-1291764435225600060}"

cd "$REPO_ROOT"

if [[ "$MODE" == "bg" ]]; then
    info "Starting in background (systemd)..."
    systemctl --user daemon-reload
    systemctl --user start thala
    sleep 2
    if systemctl --user is-active thala &>/dev/null; then
        ok "Thala started successfully"
        info "Check status: systemctl --user status thala"
        info "View logs: journalctl --user -u thala -f"
    else
        fail "Thala failed to start"
        systemctl --user status thala
        exit 1
    fi
else
    info "Starting in foreground..."
    info "Press Ctrl+C to stop"
    echo ""
    ./target/release/thala run
fi
