#!/usr/bin/env bash
# Thala Worker Entrypoint
#
# Clones the product repo at the task branch, runs OpenCode with the task
# prompt, commits/pushes changes, then POSTs a completion callback to Thala.
#
# All required env vars are documented in Dockerfile.worker.
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

# ── Helper: send signed callback ─────────────────────────────────────────────
send_callback() {
    local status="$1"
    local exit_code="$2"
    local error_message="${3:-}"

    local body
    body="$(jq -n \
        --arg task_id "${THALA_TASK_ID}" \
        --arg run_id "${THALA_RUN_ID:-}" \
        --arg status "${status}" \
        --argjson exit_code "${exit_code}" \
        --arg error_message "${error_message}" \
        '{task_id: $task_id, run_id: $run_id, status: $status, exit_code: $exit_code, error_message: ($error_message | if . == "" then null else . end)}'
    )"

    local http_code
    http_code="$(curl -s -o /dev/null -w "%{http_code}" \
        -X POST "${THALA_CALLBACK_URL}" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${THALA_RUN_TOKEN}" \
        -d "${body}" \
        --max-time 30 \
        --retry 3 \
        --retry-delay 5
    )"

    echo "[thala-worker] Callback sent (status=${status}, http=${http_code})"
}

repo_url() {
    if [[ -n "${GITHUB_TOKEN:-}" ]]; then
        printf 'https://x-access-token:%s@github.com/%s.git' "${GITHUB_TOKEN}" "${THALA_GITHUB_REPO}"
    else
        printf 'https://github.com/%s.git' "${THALA_GITHUB_REPO}"
    fi
}

run_hook() {
    local hook="$1"
    if [[ -n "${hook}" ]]; then
        bash -lc "${hook}"
    fi
}

echo "[thala-worker] Starting task ${THALA_TASK_ID} on branch ${THALA_TASK_BRANCH}"

# ── Clone product repo ────────────────────────────────────────────────────────
WORK_DIR="/workspace/repo"

git clone \
    --branch "${THALA_TASK_BRANCH}" \
    --single-branch \
    --depth 50 \
    "$(repo_url)" \
    "${WORK_DIR}"

cd "${WORK_DIR}"
git config user.email "thala-worker@example.invalid"
git config user.name "Thala Worker"

# ── Read task prompt ──────────────────────────────────────────────────────────
if [[ -n "${THALA_PROMPT_B64:-}" ]]; then
    PROMPT="$(printf '%s' "${THALA_PROMPT_B64}" | base64 -d)"
else
    PROMPT_FILE=".thala/prompts/${THALA_TASK_ID}.md"
    if [[ ! -f "${PROMPT_FILE}" ]]; then
        echo "ERROR: prompt file not found: ${PROMPT_FILE}" >&2
        send_callback "error" 1 "Prompt file not found: ${PROMPT_FILE}"
        exit 1
    fi
    PROMPT="$(cat "${PROMPT_FILE}")"
fi

# ── Run after_create hook (if present) ────────────────────────────────────────
if [[ -n "${THALA_AFTER_CREATE_HOOK:-}" ]]; then
    echo "[thala-worker] Running after_create hook"
    run_hook "${THALA_AFTER_CREATE_HOOK}" || echo "WARNING: after_create hook failed (continuing)" >&2
fi

# ── Run before_run hook (if present) ─────────────────────────────────────────
if [[ -n "${THALA_BEFORE_RUN_HOOK:-}" ]]; then
    echo "[thala-worker] Running before_run hook"
    run_hook "${THALA_BEFORE_RUN_HOOK}" || echo "WARNING: before_run hook failed (continuing)" >&2
fi

# ── Run OpenCode ──────────────────────────────────────────────────────────────
echo "[thala-worker] Launching OpenCode (model=${THALA_MODEL})"

OPENCODE_EXIT=0
opencode --model "${THALA_MODEL}" --no-session -p "${PROMPT}" || OPENCODE_EXIT=$?

echo "[thala-worker] OpenCode exited with code ${OPENCODE_EXIT}"

# ── Run after_run hook (if present) ──────────────────────────────────────────
if [[ -n "${THALA_AFTER_RUN_HOOK:-}" ]]; then
    echo "[thala-worker] Running after_run hook"
    run_hook "${THALA_AFTER_RUN_HOOK}" || echo "WARNING: after_run hook failed (continuing)" >&2
fi

# ── Commit, push, and notify Thala ───────────────────────────────────────────
if [[ "${OPENCODE_EXIT}" -eq 0 ]]; then
    if git diff --quiet && git diff --cached --quiet; then
        echo "[thala-worker] No changes to commit"
    else
        git add -A
        git commit -m "chore: apply thala task ${THALA_TASK_ID}"
        git push origin "HEAD:${THALA_TASK_BRANCH}"
        echo "[thala-worker] Pushed commit $(git rev-parse HEAD) to ${THALA_TASK_BRANCH}"
    fi
    send_callback "success" 0 ""
else
    send_callback "error" "${OPENCODE_EXIT}" "OpenCode exited with code ${OPENCODE_EXIT}"
fi

exit "${OPENCODE_EXIT}"
