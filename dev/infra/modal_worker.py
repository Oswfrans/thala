"""
Thala Modal Worker
================

This Modal app is called by Thala's ModalBackend to run OpenCode worker sessions
inside Modal serverless containers.

Thala calls:
    modal run --detach infra/modal_worker.py::run_worker [--env KEY=VALUE ...]

The function:
1. Installs OpenCode from npm (or uses a pre-built image layer)
2. Clones the public product repo at the task branch
3. Reads the prompt from .thala/prompts/<task_id>.md
4. Runs OpenCode with the task prompt
5. Commits and pushes changes back to the task branch
6. POSTs an authenticated completion callback to Thala

Required env vars (passed via --env or set in Modal secrets):
    THALA_RUN_ID           Thala run ID
    THALA_TASK_ID          task ID (e.g. "MKT-42")
    THALA_TASK_BRANCH      git branch to clone and push
    THALA_GITHUB_REPO      "org/repo" of the product repo
    THALA_CALLBACK_URL     Thala callback endpoint
    THALA_RUN_TOKEN        Per-run bearer token for callback auth
    THALA_MODEL            OpenCode model string
    OPENROUTER_API_KEY   (or OPENAI_API_KEY / ANTHROPIC_API_KEY)

Usage from WORKFLOW.md worker config:
    worker:
      backend: modal
      modal:
        app_file: infra/modal_worker.py
        function_name: run_worker
        gpu: null          # "T4" / "A10G" / "A100" for GPU tasks
        timeout_secs: 3600
      callback_base_url: https://thala.yourdomain.com
      github_repo: example/example-app
"""

import base64
import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

import modal

# ── Modal image ───────────────────────────────────────────────────────────────
# Build a reusable image layer with OpenCode pre-installed.
# Uses the v2 image builder (faster, reproducible layer caching).
# To enable v2 workspace-wide: modal.com/settings/image-config
worker_image = (
    modal.Image.debian_slim(python_version="3.12")
    .apt_install("git", "curl", "jq", "openssl")
    .run_commands(
        "curl -fsSL https://deb.nodesource.com/setup_22.x | bash -",
        "apt-get install -y nodejs",
        "npm install -g opencode-ai@latest",
    )
)

# ── Task-specific secrets ─────────────────────────────────────────────────────
# Thala injects per-run env vars via modal.Secret.from_dict(env_vars) when
# calling run_worker.remote() from the ModalBackend Rust adapter.
# For local testing, set THALA_* vars in your shell and run:
#   THALA_TASK_ID=TEST-1 ... modal run dev/infra/modal_worker.py::run_worker
# The _task_secret below picks them up automatically when present.
_LOCAL_TASK_VARS = [
    "THALA_RUN_ID", "THALA_TASK_ID", "THALA_TASK_BRANCH", "THALA_GITHUB_REPO",
    "THALA_CALLBACK_URL", "THALA_RUN_TOKEN", "THALA_MODEL",
    "THALA_PROMPT_B64", "THALA_AFTER_CREATE_HOOK", "THALA_BEFORE_RUN_HOOK",
    "THALA_AFTER_RUN_HOOK", "GITHUB_TOKEN",
    "OPENROUTER_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY",
]
_task_secret = modal.Secret.from_dict(
    {k: os.environ[k] for k in _LOCAL_TASK_VARS if k in os.environ}
)

# ── Modal app ─────────────────────────────────────────────────────────────────
app = modal.App("thala-worker", image=worker_image)


def _send_callback(
    callback_url: str,
    run_token: str,
    run_id: str,
    task_id: str,
    status: str,
    exit_code: int,
    error_message: str | None,
) -> None:
    """POST a completion callback to the Thala."""
    body = json.dumps(
        {
            "task_id": task_id,
            "run_id": run_id,
            "status": status,
            "exit_code": exit_code,
            "error_message": error_message,
        }
    ).encode()

    req = urllib.request.Request(
        callback_url,
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {run_token}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            print(f"[thala-worker] Callback sent: status={status} http={resp.status}")
    except Exception as exc:  # noqa: BLE001
        print(f"[thala-worker] WARNING: callback failed: {exc}", file=sys.stderr)


def _commit_and_push(task_id: str, task_branch: str) -> str | None:
    subprocess.run(["git", "config", "user.email", "thala-worker@example.invalid"], check=True)
    subprocess.run(["git", "config", "user.name", "Thala Worker"], check=True)

    status = subprocess.run(
        ["git", "status", "--porcelain"],
        check=True,
        stdout=subprocess.PIPE,
    )
    if not status.stdout.strip():
        print("[thala-worker] No changes to commit")
        return None

    subprocess.run(["git", "add", "-A"], check=True)
    subprocess.run(["git", "commit", "-m", f"chore: apply thala task {task_id}"], check=True)
    sha = subprocess.run(["git", "rev-parse", "HEAD"], check=True, stdout=subprocess.PIPE)
    subprocess.run(["git", "push", "origin", f"HEAD:{task_branch}"], check=True)
    return sha.stdout.decode().strip()


@app.function(
    timeout=int(os.environ.get("THALA_WORKER_TIMEOUT_SECS", "3600")),
    secrets=[_task_secret],
    # GPU can be overridden per-invocation via Modal's --gpu flag or by
    # setting the MODAL_GPU env var before calling `modal run`.
    # gpu=modal.gpu.T4(),  # uncomment for GPU-heavy tasks
)
def run_worker() -> int:
    """
    Main worker entrypoint. Reads all config from environment variables.

    Thala's ModalBackend passes task-specific values via
    modal.Secret.from_dict(env_vars) when calling run_worker.remote().
    For local testing, export THALA_* vars in your shell before running:
        export THALA_TASK_ID=TEST-1 THALA_GITHUB_REPO=org/repo ...
        modal run dev/infra/modal_worker.py::run_worker
    """
    run_id = os.environ.get("THALA_RUN_ID", "")
    task_id = os.environ["THALA_TASK_ID"]
    task_branch = os.environ["THALA_TASK_BRANCH"]
    github_repo = os.environ["THALA_GITHUB_REPO"]
    callback_url = os.environ["THALA_CALLBACK_URL"]
    run_token = os.environ["THALA_RUN_TOKEN"]
    model = os.environ["THALA_MODEL"]

    after_create_hook = os.environ.get("THALA_AFTER_CREATE_HOOK", "")
    before_run_hook = os.environ.get("THALA_BEFORE_RUN_HOOK", "")
    after_run_hook = os.environ.get("THALA_AFTER_RUN_HOOK", "")

    print(f"[thala-worker] Starting task {task_id} on branch {task_branch}")

    # ── Clone repo ────────────────────────────────────────────────────────────
    github_token = os.environ.get("GITHUB_TOKEN", "")
    if github_token:
        # Embed the token in the URL so git doesn't prompt for credentials.
        repo_url = f"https://x-access-token:{github_token}@github.com/{github_repo}.git"
    else:
        repo_url = f"https://github.com/{github_repo}.git"
    work_dir = Path("/workspace/repo")

    subprocess.run(
        [
            "git", "clone",
            "--branch", task_branch,
            "--single-branch",
            "--depth", "50",
            repo_url,
            str(work_dir),
        ],
        check=True,
    )

    os.chdir(work_dir)

    # ── Read prompt ───────────────────────────────────────────────────────────
    # Prefer the base64-encoded prompt injected by Thala via env var (always
    # present when launched by ModalBackend). Fall back to the prompt file on
    # the branch for manual / legacy invocations.
    prompt_b64 = os.environ.get("THALA_PROMPT_B64", "")
    if prompt_b64:
        prompt = base64.b64decode(prompt_b64).decode()
    else:
        prompt_path = work_dir / ".thala" / "prompts" / f"{task_id}.md"
        if not prompt_path.exists():
            msg = f"Prompt not found: neither THALA_PROMPT_B64 nor {prompt_path}"
            print(f"ERROR: {msg}", file=sys.stderr)
            _send_callback(callback_url, run_token, run_id, task_id, "error", 1, msg)
            return 1
        prompt = prompt_path.read_text()

    # ── after_create hook ─────────────────────────────────────────────────────
    if after_create_hook:
        print(f"[thala-worker] Running after_create hook: {after_create_hook}")
        result = subprocess.run(after_create_hook, shell=True)  # noqa: S602
        if result.returncode != 0:
            print("WARNING: after_create hook failed (continuing)", file=sys.stderr)

    # ── before_run hook ───────────────────────────────────────────────────────
    if before_run_hook:
        print(f"[thala-worker] Running before_run hook: {before_run_hook}")
        result = subprocess.run(before_run_hook, shell=True)  # noqa: S602
        if result.returncode != 0:
            print("WARNING: before_run hook failed (continuing)", file=sys.stderr)

    # ── Run OpenCode ──────────────────────────────────────────────────────────
    print(f"[thala-worker] Launching OpenCode (model={model})")
    opencode_result = subprocess.run(
        ["opencode", "run", "--model", model, prompt],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env={**os.environ},  # pass through API keys etc.
    )
    opencode_output = opencode_result.stdout or ""
    print(opencode_output, end="")
    exit_code = opencode_result.returncode
    if "ProviderModelNotFoundError" in opencode_output or "Model not found" in opencode_output:
        exit_code = 1
    print(f"[thala-worker] OpenCode exited with code {exit_code}")

    # ── after_run hook ────────────────────────────────────────────────────────
    if after_run_hook:
        print(f"[thala-worker] Running after_run hook: {after_run_hook}")
        result = subprocess.run(after_run_hook, shell=True)  # noqa: S602
        if result.returncode != 0:
            print("WARNING: after_run hook failed (continuing)", file=sys.stderr)

    # ── Commit, push, and notify Thala ────────────────────────────────────────
    if exit_code == 0:
        try:
            commit_sha = _commit_and_push(task_id, task_branch)
            if commit_sha:
                print(f"[thala-worker] Pushed commit {commit_sha} to {task_branch}")
        except subprocess.CalledProcessError as exc:
            msg = f"commit/push failed with code {exc.returncode}"
            print(f"ERROR: {msg}", file=sys.stderr)
            _send_callback(callback_url, run_token, run_id, task_id, "error", exc.returncode, msg)
            return exc.returncode or 1

        _send_callback(callback_url, run_token, run_id, task_id, "success", 0, None)
    else:
        _send_callback(
            callback_url,
            run_token,
            run_id,
            task_id,
            "error",
            exit_code,
            f"OpenCode exited with code {exit_code}",
        )

    return exit_code


# ── Local test entry ──────────────────────────────────────────────────────────
if __name__ == "__main__":
    # Allow running locally for smoke-tests: python infra/modal_worker.py
    # Requires all THALA_* env vars to be set in the shell.
    with app.run():
        result = run_worker.remote()
    sys.exit(result)
