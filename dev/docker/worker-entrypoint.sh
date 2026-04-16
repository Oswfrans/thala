#!/usr/bin/env bash
# Thala Worker Entrypoint
#
# Clones the product repo at the task branch, runs OpenCode with the task
# prompt, then POSTs a completion callback with a patch to the Thala gateway.
#
# All required env vars are documented in Dockerfile.worker.
# GitHub credentials are intentionally not provided; Thala applies and pushes the
# returned patch from the trusted orchestrator side.
set -euo pipefail

# ── Validate required env vars ────────────────────────────────────────────────
required_vars=(
    THALA_TASK_ID
    THALA_TASK_BRANCH
    THALA_GITHUB_REPO
    THALA_CALLBACK_URL
    THALA_RUN_TOKEN
    THALA_MODEL
)
for var in "${required_vars[@]}"; do
    if [[ -z "${!var:-}" ]]; then
        echo "ERROR: required env var $var is not set" >&2
        exit 1
    fi
done

echo "[thala-worker] Starting task ${THALA_TASK_ID} on branch ${THALA_TASK_BRANCH}"

# ── Clone product repo ────────────────────────────────────────────────────────
REPO_URL="https://github.com/${THALA_GITHUB_REPO}.git"
WORK_DIR="/workspace/repo"

git clone \
    --branch "${THALA_TASK_BRANCH}" \
    --single-branch \
    --depth 50 \
    "${REPO_URL}" \
    "${WORK_DIR}"

cd "${WORK_DIR}"

# ── Read task prompt ──────────────────────────────────────────────────────────
PROMPT_FILE=".thala/prompts/${THALA_TASK_ID}.md"
if [[ ! -f "${PROMPT_FILE}" ]]; then
    echo "ERROR: prompt file not found: ${PROMPT_FILE}" >&2
    send_callback "error" 1 "Prompt file not found: ${PROMPT_FILE}"
    exit 1
fi

PROMPT="$(cat "${PROMPT_FILE}")"

# ── Run after_create hook (if present) ────────────────────────────────────────
AFTER_CREATE_HOOK="${THALA_AFTER_CREATE_HOOK:-}"
if [[ -n "${AFTER_CREATE_HOOK}" ]]; then
    echo "[thala-worker] Running after_create hook: ${AFTER_CREATE_HOOK}"
    eval "${AFTER_CREATE_HOOK}" || {
        echo "WARNING: after_create hook failed (continuing)" >&2
    }
fi

# ── Run before_run hook (if present) ─────────────────────────────────────────
BEFORE_RUN_HOOK="${THALA_BEFORE_RUN_HOOK:-}"
if [[ -n "${BEFORE_RUN_HOOK}" ]]; then
    echo "[thala-worker] Running before_run hook: ${BEFORE_RUN_HOOK}"
    eval "${BEFORE_RUN_HOOK}" || {
        echo "WARNING: before_run hook failed (continuing)" >&2
    }
fi

# ── Helper: send signed callback ─────────────────────────────────────────────
send_callback() {
    local status="$1"
    local exit_code="$2"
    local error_message="${3:-}"
    local patch_base64="${4:-}"

    local body
    body="$(jq -n \
        --arg task_id "${THALA_TASK_ID}" \
        --arg status "${status}" \
        --argjson exit_code "${exit_code}" \
        --arg error_message "${error_message}" \
        --arg patch_base64 "${patch_base64}" \
        '{task_id: $task_id, status: $status, exit_code: $exit_code, error_message: ($error_message | if . == "" then null else . end), patch_base64: ($patch_base64 | if . == "" then null else . end)}'
    )"

    local http_code
    http_code="$(curl -s -o /dev/null -w "%{http_code}" \
        -X POST "${THALA_CALLBACK_URL}" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${THALA_RUN_TOKEN}" \
        -d "${body}" \
        --max-time 30 \
        --retry 3 \
        --retry-delay 5 \
    )"

    echo "[thala-worker] Callback sent (status=${status}, http=${http_code})"
}

# ── Run OpenCode ──────────────────────────────────────────────────────────────
echo "[thala-worker] Launching OpenCode (model=${THALA_MODEL})"

OPENCODE_EXIT=0
opencode --model "${THALA_MODEL}" --no-session -p "${PROMPT}" || OPENCODE_EXIT=$?

echo "[thala-worker] OpenCode exited with code ${OPENCODE_EXIT}"

# ── Run after_run hook (if present) ──────────────────────────────────────────
AFTER_RUN_HOOK="${THALA_AFTER_RUN_HOOK:-}"
if [[ -n "${AFTER_RUN_HOOK}" ]]; then
    echo "[thala-worker] Running after_run hook: ${AFTER_RUN_HOOK}"
    eval "${AFTER_RUN_HOOK}" || {
        echo "WARNING: after_run hook failed (continuing)" >&2
    }
fi

# ── Produce patch and signal file ────────────────────────────────────────────
# Write the signal file so the monitoring loop can detect completion when
# polling local paths. For remote runs this is a belt-and-suspenders measure —
# the callback below is the primary completion signal.
git config user.email "thala-worker@example.invalid"
git config user.name "Thala Worker"

SIGNAL_FILE=".thala/signals/${THALA_TASK_ID}.signal"
mkdir -p ".thala/signals"
echo "DONE" > "${SIGNAL_FILE}"

PATCH_BASE64="$(git diff --binary HEAD | base64 -w 0)"
if [[ -z "${PATCH_BASE64}" ]]; then
    echo "[thala-worker] No changes to return"
fi

# ── Send completion callback ──────────────────────────────────────────────────
if [[ "${OPENCODE_EXIT}" -eq 0 ]]; then
    send_callback "success" 0 "" "${PATCH_BASE64}"
else
    send_callback "error" "${OPENCODE_EXIT}" "OpenCode exited with code ${OPENCODE_EXIT}" "${PATCH_BASE64}"
fi

exit "${OPENCODE_EXIT}"
