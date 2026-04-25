#!/usr/bin/env bash
# Thala environment setup script
# Source this file: source scripts/env.sh

export MODAL_APP_FILE="dev/infra/modal_worker.py::run_worker"
export THALA_CALLBACK_BIND="127.0.0.1:8788"
export DISCORD_ALERTS_CHANNEL_ID="1291764435225600060"

# Secrets - set these in your shell before running
if [[ -z "$DISCORD_BOT_TOKEN" ]]; then
    echo "WARNING: DISCORD_BOT_TOKEN is not set"
fi
if [[ -z "$DISCORD_PUBLIC_KEY" ]]; then
    echo "WARNING: DISCORD_PUBLIC_KEY is not set"
fi
if [[ -z "$OPENROUTER_API_KEY" ]]; then
    echo "WARNING: OPENROUTER_API_KEY is not set"
fi
if [[ -z "$THALA_GITHUB_TOKEN" ]]; then
    echo "WARNING: THALA_GITHUB_TOKEN is not set"
fi

echo "Thala environment configured"
echo "Discord channel: $DISCORD_ALERTS_CHANNEL_ID"
echo "Callback bind: $THALA_CALLBACK_BIND"
