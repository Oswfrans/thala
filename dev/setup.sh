#!/usr/bin/env bash
# Thala host setup script — installs system deps, sets up PATH in the systemd
# service, and validates that all required binaries are reachable.
#
# Usage:
#   bash dev/setup.sh                              # check/install binaries (local backend)
#   bash dev/setup.sh --backend modal              # check/install for Modal backend
#   bash dev/setup.sh --backend cloudflare         # check/install for Cloudflare backend
#   bash dev/setup.sh --backend opencode-zen       # check/install for OpenCode Zen backend
#   bash dev/setup.sh --configure                  # interactive API key + config setup
#   bash dev/setup.sh --backend modal --configure  # both
#
# Safe to re-run — all steps are idempotent.
set -euo pipefail

BOLD='\033[1m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'; RED='\033[0;31m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}!${NC} $*"; }
fail() { echo -e "${RED}✗${NC} $*"; }
step() { echo -e "\n${BOLD}$*${NC}"; }
info() { echo -e "${CYAN}→${NC} $*"; }

# ── Parse args ────────────────────────────────────────────────────────────────
BACKEND="local"
CONFIGURE=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --backend)      BACKEND="$2"; shift 2 ;;
        --backend=*)    BACKEND="${1#--backend=}"; shift ;;
        --configure)    CONFIGURE=true; shift ;;
        *) echo "Usage: $0 [--backend local|modal|cloudflare|opencode-zen] [--configure]" >&2; exit 1 ;;
    esac
done

case "$BACKEND" in
    local|modal|cloudflare|opencode-zen) ;;
    *) echo "Unknown backend '$BACKEND'. Valid: local, modal, cloudflare, opencode-zen" >&2; exit 1 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo -e "${BOLD}Thala setup${NC} — backend: ${CYAN}${BACKEND}${NC}"

# ── Helper: prompt for a value ────────────────────────────────────────────────
# Usage: prompt_value VAR_NAME "Prompt text" "default_or_empty"
# Sets the named variable in the caller's scope.
prompt_value() {
    local var_name="$1"
    local prompt_text="$2"
    local default_val="${3:-}"
    local display_default=""
    [[ -n "$default_val" ]] && display_default=" [${default_val}]"
    printf "  %s%s: " "$prompt_text" "$display_default"
    local input
    read -r input
    if [[ -z "$input" && -n "$default_val" ]]; then
        printf -v "$var_name" '%s' "$default_val"
    else
        printf -v "$var_name" '%s' "$input"
    fi
}

# Usage: prompt_secret VAR_NAME "Prompt text"
prompt_secret() {
    local var_name="$1"
    local prompt_text="$2"
    printf "  %s (hidden): " "$prompt_text"
    local input
    read -rs input
    echo ""
    printf -v "$var_name" '%s' "$input"
}

find_bin() {
    local bin="$1"
    if command -v "$bin" &>/dev/null; then
        command -v "$bin"
        return 0
    fi
    for loc in "$HOME/.opencode/bin/$bin" "$HOME/.local/bin/$bin" "$HOME/go/bin/$bin" "$HOME/.cargo/bin/$bin" "$HOME/.bun/bin/$bin"; do
        if [[ -x "$loc" ]]; then
            echo "$loc"
            return 0
        fi
    done
    return 1
}

install_bd() {
    if find_bin bd >/dev/null; then
        ok "bd found at $(find_bin bd)"
        return 0
    fi

    step "Installing bd (Beads CLI)..."
    if command -v brew &>/dev/null; then
        brew install beads && ok "bd installed with Homebrew" && return 0
        warn "Homebrew install failed; falling back to official install script"
    fi

    if command -v apt-get &>/dev/null; then
        sudo apt-get install -y curl ca-certificates libicu-dev libzstd-dev || warn "Could not install optional Beads build dependencies"
    fi

    local script="/tmp/thala-install-beads.sh"
    curl -fsSL https://raw.githubusercontent.com/steveyegge/beads/main/scripts/install.sh -o "$script"
    bash "$script"

    if find_bin bd >/dev/null; then
        ok "bd installed at $(find_bin bd)"
    else
        fail "bd install completed but bd is still not on PATH"
        info "Add ~/.local/bin, ~/go/bin, or ~/.cargo/bin to PATH if the installer placed bd there."
        return 1
    fi
}

install_opencode() {
    if find_bin opencode >/dev/null; then
        ok "opencode found at $(find_bin opencode)"
        return 0
    fi

    step "Installing opencode..."
    local script="/tmp/thala-install-opencode.sh"
    curl -fsSL https://opencode.ai/install -o "$script"
    bash "$script" || warn "Official opencode installer failed; trying npm fallback"

    if ! find_bin opencode >/dev/null && command -v npm &>/dev/null; then
        npm install -g opencode-ai
    fi

    if find_bin opencode >/dev/null; then
        ok "opencode installed at $(find_bin opencode)"
    else
        fail "opencode install failed"
        info "Install manually with: curl -fsSL https://opencode.ai/install | bash"
        return 1
    fi
}

ensure_beads_workspace() {
    local root="$1"
    if [[ ! -d "$root" ]]; then
        warn "Workspace does not exist yet: $root"
        info "Create or clone the product repo, then run: cd $root && bd init --quiet"
        return 0
    fi
    if [[ -d "$root/.beads" ]]; then
        ok "Beads workspace already initialized at $root/.beads"
        return 0
    fi
    info "Initializing Beads workspace in $root"
    (cd "$root" && bd init --quiet) || (cd "$root" && bd init)
    ok "Beads workspace initialized"
}

# ── Helper: set or replace an env var in the systemd service file ─────────────
set_service_env() {
    local key="$1" val="$2" svc_file="$3"
    if grep -q "^Environment=\"${key}=" "$svc_file"; then
        sed -i "s|^Environment=\"${key}=.*\"|Environment=\"${key}=${val}\"|" "$svc_file"
    else
        # Append after [Service] section header
        sed -i "/^\[Service\]/a Environment=\"${key}=${val}\"" "$svc_file"
    fi
}

# ── 1. Common system packages (all backends) ──────────────────────────────────
step "1. Common system packages (git, bd, gh, gcloud)..."

if ! command -v git &>/dev/null; then
    sudo apt-get install -y git && ok "git installed" || fail "git install failed"
else
    ok "git found at $(command -v git)"
fi

if ! command -v gh &>/dev/null; then
    warn "gh (GitHub CLI) not found"
    info "Install: https://cli.github.com — then run: gh auth login"
else
    ok "gh found at $(command -v gh)"
fi

install_bd

if ! command -v gcloud &>/dev/null; then
    warn "gcloud not found"
    info "Install: https://cloud.google.com/sdk — then run: gcloud auth login"
else
    ok "gcloud found at $(command -v gcloud)"
fi

# ── 2. Backend-specific tools ─────────────────────────────────────────────────
if [[ "$BACKEND" == "local" ]]; then
    step "2. Local backend tools (tmux, opencode, bun, unzip)..."

    if ! command -v tmux &>/dev/null; then
        sudo apt-get install -y tmux && ok "tmux installed" || fail "tmux install failed"
    else
        ok "tmux found at $(command -v tmux)"
    fi

    if ! command -v unzip &>/dev/null; then
        sudo apt-get install -y unzip && ok "unzip installed" || fail "unzip install failed"
    else
        ok "unzip already installed"
    fi

    if ! command -v bun &>/dev/null && [[ ! -x "$HOME/.bun/bin/bun" ]]; then
        curl -fsSL https://bun.sh/install | bash
        ok "bun installed to $HOME/.bun/bin"
    else
        ok "bun already installed"
    fi

    install_opencode

elif [[ "$BACKEND" == "opencode-zen" ]]; then
    step "2. OpenCode Zen backend tools..."
    info "Workers run in OpenCode Zen — tmux and local opencode are NOT needed on this host."

elif [[ "$BACKEND" == "modal" ]]; then
    step "2. Modal backend tools (modal CLI)..."

    if ! command -v uv &>/dev/null && ! command -v python3 &>/dev/null; then
        warn "Neither uv nor python3 found — required for Modal CLI"
        info "Install uv (recommended): curl -LsSf https://astral.sh/uv/install.sh | sh"
        info "Then: uv tool install modal && modal token new"
    elif ! command -v modal &>/dev/null; then
        warn "modal CLI not found"
        if command -v uv &>/dev/null; then
            info "Install: uv tool install modal"
        else
            info "Install: pip install modal"
        fi
        info "Then authenticate: modal token new"
    else
        ok "modal found at $(command -v modal)"
        MODAL_VERSION=$(modal --version 2>/dev/null || echo "unknown")
        info "version: $MODAL_VERSION"
    fi

    info "Workers run inside Modal containers — tmux and opencode are NOT needed on this host."

elif [[ "$BACKEND" == "cloudflare" ]]; then
    step "2. Cloudflare backend tools (docker)..."

    if ! command -v docker &>/dev/null; then
        warn "docker not found — needed to build and push dev/docker/Dockerfile.worker"
        info "Install: https://docs.docker.com/engine/install/"
    else
        ok "docker found at $(command -v docker)"
    fi

    info "Workers run inside Cloudflare Containers — tmux and opencode are NOT needed on this host."
    info "Build and push the worker image before enabling this backend:"
    info "  docker build -f dev/docker/Dockerfile.worker -t <registry>/thala-worker:latest ."
    info "  docker push <registry>/thala-worker:latest"
fi

# ── 3. Systemd service PATH ───────────────────────────────────────────────────
step "3. Updating systemd service PATH..."

SERVICE_FILE="$HOME/.config/systemd/user/thala.service"

case "$BACKEND" in
    local)
        NEW_PATH="$HOME/.opencode/bin:$HOME/.bun/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
        ;;
    modal|opencode-zen)
        NEW_PATH="$HOME/.local/bin:$HOME/go/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
        ;;
    cloudflare)
        NEW_PATH="$HOME/.local/bin:$HOME/go/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
        ;;
esac

if [[ ! -f "$SERVICE_FILE" ]]; then
    warn "Systemd service not found at $SERVICE_FILE — skipping PATH update"
    warn "Run './target/release/thala service install' first, then re-run this script"
else
    if grep -q "Environment=\"PATH=$NEW_PATH\"" "$SERVICE_FILE"; then
        ok "Systemd PATH already correct"
    else
        if grep -q "^Environment=\"PATH=" "$SERVICE_FILE"; then
            sed -i "s|^Environment=\"PATH=.*\"|Environment=\"PATH=$NEW_PATH\"|" "$SERVICE_FILE"
        else
            sed -i "/^\[Service\]/a Environment=\"PATH=$NEW_PATH\"" "$SERVICE_FILE"
        fi
        systemctl --user daemon-reload
        ok "Systemd PATH updated to: $NEW_PATH"
    fi
fi

# ── 4. Interactive configuration (--configure only) ───────────────────────────
if $CONFIGURE; then
    step "4. API key and config setup..."
    echo ""

    CONFIG_DIR="$HOME/.thala"
    CONFIG_FILE="$CONFIG_DIR/config.toml"
    mkdir -p "$CONFIG_DIR"

    # ── 4a. config.toml ───────────────────────────────────────────────────────
    echo -e "${BOLD}  Thala config (${CONFIG_FILE})${NC}"
    echo "  Press Enter to keep the current value, or type a new one."
    echo ""

    # Read existing values if the file exists
    _existing() {
        grep -Po "(?<=^${1} = \")([^\"]*)" "$CONFIG_FILE" 2>/dev/null || true
    }

    prompt_secret OPENCODE_API_KEY "OpenCode Zen API key (opencode.ai/settings)"
    prompt_value  PRODUCT_SLUG   "Product slug (e.g. example-app)" "$(_existing "product" || echo "example-app")"
    prompt_value  WORKSPACE_ROOT "Workspace root (absolute path to product repo)" "$(_existing "workspace_root" || echo "/home/$(whoami)/example-app")"
    ensure_beads_workspace "$WORKSPACE_ROOT"
    prompt_secret DISCORD_TOKEN  "Discord bot token"

    # Write config.toml from template, substituting values
    sed \
        -e "s|REPLACE_OPENCODE_API_KEY|${OPENCODE_API_KEY}|g" \
        -e "s|REPLACE_PRODUCT_SLUG|${PRODUCT_SLUG}|g" \
        -e "s|REPLACE_WORKSPACE_ROOT|${WORKSPACE_ROOT}|g" \
        -e "s|REPLACE_DISCORD_BOT_TOKEN|${DISCORD_TOKEN}|g" \
        "$REPO_ROOT/dev/config.template.toml" > "$CONFIG_FILE"

    ok "Written to $CONFIG_FILE"
    echo ""

    # ── 4b. Systemd env vars ──────────────────────────────────────────────────
    echo -e "${BOLD}  Systemd environment variables${NC}"
    echo "  These go in the [Service] section of $SERVICE_FILE."
    echo ""

    prompt_value  DISCORD_WEBHOOK    "Discord alerts webhook URL" ""
    prompt_value  TELEGRAM_TOKEN     "Telegram bot token (leave blank to skip)" ""
    prompt_value  TELEGRAM_CHAT_IDS  "Telegram escalation chat IDs (comma-separated)" ""
    prompt_value  GCP_PROJECT        "GCP project ID" ""
    prompt_value  GCP_REGION         "GCP region" "europe-west4"

    # Backend-specific secrets
    if [[ "$BACKEND" == "modal" || "$BACKEND" == "cloudflare" || "$BACKEND" == "opencode-zen" ]]; then
        echo ""
        echo -e "${BOLD}  Remote backend secrets${NC}"
        prompt_secret GH_TOKEN "GitHub PAT (repo read/write)"
    fi

    if [[ "$BACKEND" == "modal" ]]; then
        prompt_value CALLBACK_BIND "Callback listen address" "127.0.0.1:8788"
        prompt_secret OR_API_KEY "OpenRouter API key (for worker containers)"
    fi

    if [[ "$BACKEND" == "opencode-zen" ]]; then
        prompt_value CALLBACK_BIND "Callback listen address" "127.0.0.1:8788"
    fi

    if [[ "$BACKEND" == "cloudflare" ]]; then
        prompt_value  CF_BASE_URL "Cloudflare control-plane Worker URL" "http://localhost:8787"
        prompt_secret CF_TOKEN    "Cloudflare control-plane shared bearer token (THALA_CF_TOKEN)"
        info "Set Worker-side secrets with: cd cloudflare/control-plane && npx wrangler secret put THALA_SHARED_AUTH_TOKEN"
        info "Also set THALA_GITHUB_TOKEN and your OpenCode provider key as Worker secrets."
    fi

    if [[ -f "$SERVICE_FILE" ]]; then
        # Always-required vars
        [[ -n "$DISCORD_WEBHOOK" ]] && set_service_env "DISCORD_ALERTS_WEBHOOK"    "$DISCORD_WEBHOOK" "$SERVICE_FILE"
        [[ -n "$OPENCODE_API_KEY" ]]&& set_service_env "OPENCODE_API_KEY"          "$OPENCODE_API_KEY" "$SERVICE_FILE"
        [[ -n "$TELEGRAM_TOKEN" ]]  && set_service_env "TELEGRAM_BOT_TOKEN"        "$TELEGRAM_TOKEN"  "$SERVICE_FILE"
        [[ -n "$TELEGRAM_CHAT_IDS" ]] && set_service_env "TELEGRAM_ESCALATION_CHAT_IDS" "$TELEGRAM_CHAT_IDS" "$SERVICE_FILE"
        [[ -n "$GCP_PROJECT" ]]     && set_service_env "GCP_PROJECT"               "$GCP_PROJECT"     "$SERVICE_FILE"
        [[ -n "$GCP_REGION" ]]      && set_service_env "GCP_REGION"                "$GCP_REGION"      "$SERVICE_FILE"

        # Remote backend vars
        if [[ "$BACKEND" == "modal" || "$BACKEND" == "cloudflare" || "$BACKEND" == "opencode-zen" ]]; then
            [[ -n "$GH_TOKEN" ]] && set_service_env "THALA_GITHUB_TOKEN" "$GH_TOKEN" "$SERVICE_FILE"
        fi

        if [[ "$BACKEND" == "modal" ]]; then
            [[ -n "${CALLBACK_BIND:-}" ]] && set_service_env "THALA_CALLBACK_BIND" "$CALLBACK_BIND" "$SERVICE_FILE"
            [[ -n "${OR_API_KEY:-}" ]]    && set_service_env "OPENROUTER_API_KEY"  "$OR_API_KEY"      "$SERVICE_FILE"
        fi

        if [[ "$BACKEND" == "opencode-zen" ]]; then
            [[ -n "${CALLBACK_BIND:-}" ]] && set_service_env "THALA_CALLBACK_BIND" "$CALLBACK_BIND" "$SERVICE_FILE"
        fi

        if [[ "$BACKEND" == "cloudflare" ]]; then
            [[ -n "${CF_BASE_URL:-}" ]] && set_service_env "THALA_CF_BASE_URL" "$CF_BASE_URL" "$SERVICE_FILE"
            [[ -n "${CF_TOKEN:-}" ]]    && set_service_env "THALA_CF_TOKEN"    "$CF_TOKEN"    "$SERVICE_FILE"
        fi

        systemctl --user daemon-reload
        ok "Systemd service updated with environment variables"
    else
        warn "Systemd service not found — skipping service env update"
        warn "Install your process supervisor unit first, then re-run with --configure"
        echo ""
        echo "  Env vars to add manually to the [Service] section:"
        [[ -n "$DISCORD_WEBHOOK" ]]    && echo "    Environment=\"DISCORD_ALERTS_WEBHOOK=$DISCORD_WEBHOOK\""
        [[ -n "$OPENCODE_API_KEY" ]]   && echo "    Environment=\"OPENCODE_API_KEY=$OPENCODE_API_KEY\""
        [[ -n "${GH_TOKEN:-}" ]]       && echo "    Environment=\"THALA_GITHUB_TOKEN=$GH_TOKEN\""
        [[ -n "${CALLBACK_BIND:-}" ]] && echo "    Environment=\"THALA_CALLBACK_BIND=$CALLBACK_BIND\""
        [[ -n "${CF_BASE_URL:-}" ]] && echo "    Environment=\"THALA_CF_BASE_URL=$CF_BASE_URL\""
        [[ -n "${CF_TOKEN:-}" ]]    && echo "    Environment=\"THALA_CF_TOKEN=$CF_TOKEN\""
    fi
fi

# ── 5. Validate ───────────────────────────────────────────────────────────────
step "5. Validating..."

ALL_OK=true

check_required() {
    local bin="$1"
    if command -v "$bin" &>/dev/null; then
        ok "$bin → $(command -v "$bin")"
    else
        fail "$bin not found — required for $BACKEND backend"
        ALL_OK=false
    fi
}

check_optional() {
    local bin="$1" note="$2"
    local found=""
    command -v "$bin" &>/dev/null && found=$(command -v "$bin")
    if [[ -z "$found" ]]; then
        for loc in "$HOME/.opencode/bin/$bin" "$HOME/.bun/bin/$bin" "$HOME/.local/bin/$bin"; do
            [[ -x "$loc" ]] && found="$loc" && break
        done
    fi
    if [[ -n "$found" ]]; then
        ok "$bin → $found"
    else
        warn "$bin not found — $note"
    fi
}

check_required git
check_required bd
check_optional gh     "PR creation and CI checks will not work"
check_optional gcloud "GCP deployments will not work"

case "$BACKEND" in
    local)
        check_required tmux
        check_required opencode
        check_optional bun      "workspace hooks using bun will fail"
        ;;
    opencode-zen)
        ;;
    modal)
        check_optional modal "Modal dispatch will fail — run: uv tool install modal && modal token new"
        ;;
    cloudflare)
        check_optional docker "local wrangler dev for Cloudflare Sandbox requires Docker"
        ;;
esac

# Config file check
CONFIG_FILE="$HOME/.thala/config.toml"
if [[ -f "$CONFIG_FILE" ]]; then
    if grep -q "REPLACE_" "$CONFIG_FILE"; then
        warn "$CONFIG_FILE exists but still has unfilled REPLACE_ placeholders"
        info "Run: bash dev/setup.sh --configure"
    else
        ok "$CONFIG_FILE is configured"
    fi
else
    warn "$CONFIG_FILE not found"
    info "Run: bash dev/setup.sh --configure"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
if $ALL_OK; then
    echo -e "${GREEN}${BOLD}Setup complete.${NC}"
    if systemctl --user is-active thala &>/dev/null 2>&1; then
        echo "Restart Thala to apply changes:"
        echo "  systemctl --user restart thala"
    else
        echo "Start Thala:"
        echo "  systemctl --user start thala"
    fi
else
    echo -e "${RED}${BOLD}Some required tools are missing.${NC} Fix the above and re-run."
    exit 1
fi
