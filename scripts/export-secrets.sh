#!/usr/bin/env bash
set -euo pipefail

# Source this file after exporting secrets in your shell or loading them from a
# local secret manager. This script intentionally does not contain credential
# values.

required_vars=(
  DISCORD_BOT_TOKEN
  DISCORD_PUBLIC_KEY
  DISCORD_ALERTS_CHANNEL_ID
  OPENROUTER_API_KEY
  THALA_GITHUB_TOKEN
)

missing=()
for var in "${required_vars[@]}"; do
  if [ -z "${!var:-}" ]; then
    missing+=("$var")
  fi
done

if [ "${#missing[@]}" -gt 0 ]; then
  printf 'Missing required environment variables:\n' >&2
  printf '  %s\n' "${missing[@]}" >&2
  return 1 2>/dev/null || exit 1
fi

export MODAL_APP_FILE="${MODAL_APP_FILE:-dev/infra/modal_worker.py::run_worker}"
export THALA_CALLBACK_BIND="${THALA_CALLBACK_BIND:-127.0.0.1:8090}"

echo "Thala environment validated"
echo "  Discord channel: $DISCORD_ALERTS_CHANNEL_ID"
echo "  Callback bind: $THALA_CALLBACK_BIND"
echo ""
echo "Ready to start: bash scripts/start-thala.sh"
