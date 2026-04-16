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
5. Returns a git patch to Thala
6. POSTs an authenticated completion callback to the Thala gateway

Required env vars (passed via --env or set in Modal secrets):
    THALA_TASK_ID          Notion task ID (e.g. "MKT-42")
    THALA_TASK_BRANCH      git branch containing the prompt
    THALA_GITHUB_REPO      "org/repo" of the product repo
    THALA_CALLBACK_URL     Thala gateway callback endpoint
    THALA_RUN_TOKEN        Per-run bearer token for callback auth
    THALA_MODEL            OpenCode model string
    OPENROUTER_API_KEY   (or OPENAI_API_KEY / ANTHROPIC_API_KEY)

Note: GitHub credentials are intentionally not sent to the worker. This worker
can clone public repositories; private-repo support requires a Thala-side source
bundle/proxy so GitHub authority stays in the orchestrator.

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
worker_image = (
    modal.Image.debian_slim(python_version="3.12")
    .apt_install("git", "curl", "jq", "openssl")
    .run_commands(
        "curl -fsSL https://deb.nodesource.com/setup_22.x | bash -",
        "apt-get install -y nodejs",
        "npm install -g opencode-ai@latest",
    )
)

# ── Modal app ─────────────────────────────────────────────────────────────────
app = modal.App("thala-worker", image=worker_image)


def _send_callback(
    callback_url: str,
    run_token: str,
    task_id: str,
    status: str,
    exit_code: int,
    error_message: str | None,
    patch_base64: str | None = None,
) -> None:
    """POST a completion callback to the Thala gateway."""
    body = json.dumps(
        {
            "task_id": task_id,
            "status": status,
            "exit_code": exit_code,
            "error_message": error_message,
            "patch_base64": patch_base64,
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


@app.function(
    timeout=int(os.environ.get("THALA_WORKER_TIMEOUT_SECS", "3600")),
    # GPU can be overridden per-invocation via Modal's --gpu flag or by
    # setting the MODAL_GPU env var before calling `modal run`.
    # gpu=modal.gpu.T4(),  # uncomment for GPU-heavy tasks
)
def run_worker() -> int:
    """
    Main worker entrypoint. Reads all config from environment variables so
    Thala can pass task-specific values via `modal run --env KEY=VALUE`.
    """
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
    prompt_path = work_dir / ".thala" / "prompts" / f"{task_id}.md"
    if not prompt_path.exists():
        msg = f"Prompt file not found: {prompt_path}"
        print(f"ERROR: {msg}", file=sys.stderr)
        _send_callback(callback_url, run_token, task_id, "error", 1, msg)
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
        ["opencode", "--model", model, "--no-session", "-p", prompt],
        env={**os.environ},  # pass through API keys etc.
    )
    exit_code = opencode_result.returncode
    print(f"[thala-worker] OpenCode exited with code {exit_code}")

    # ── after_run hook ────────────────────────────────────────────────────────
    if after_run_hook:
        print(f"[thala-worker] Running after_run hook: {after_run_hook}")
        result = subprocess.run(after_run_hook, shell=True)  # noqa: S602
        if result.returncode != 0:
            print("WARNING: after_run hook failed (continuing)", file=sys.stderr)

    # ── Produce patch + signal file ───────────────────────────────────────────
    subprocess.run(["git", "config", "user.email", "thala-worker@example.invalid"], check=True)
    subprocess.run(["git", "config", "user.name", "Thala Worker"], check=True)

    signal_dir = work_dir / ".thala" / "signals"
    signal_dir.mkdir(parents=True, exist_ok=True)
    (signal_dir / f"{task_id}.signal").write_text("DONE\n")

    diff_result = subprocess.run(
        ["git", "diff", "--binary", "HEAD"],
        check=True,
        stdout=subprocess.PIPE,
    )
    patch_base64 = base64.b64encode(diff_result.stdout).decode()
    if not diff_result.stdout:
        print("[thala-worker] No changes to commit")

    # ── Send callback ─────────────────────────────────────────────────────────
    if exit_code == 0:
        _send_callback(callback_url, run_token, task_id, "success", 0, None, patch_base64)
    else:
        _send_callback(
            callback_url,
            run_token,
            task_id,
            "error",
            exit_code,
            f"OpenCode exited with code {exit_code}",
            patch_base64,
        )

    return exit_code


# ── Local test entry ──────────────────────────────────────────────────────────
if __name__ == "__main__":
    # Allow running locally for smoke-tests: python infra/modal_worker.py
    # Requires all THALA_* env vars to be set in the shell.
    with app.run():
        result = run_worker.remote()
    sys.exit(result)
