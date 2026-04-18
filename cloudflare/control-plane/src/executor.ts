import { getSandbox, type ExecOptions, type ExecResult } from "@cloudflare/sandbox";
import type { Env } from "./auth";
import type { StartTaskRequest, TaskState, TaskStatus } from "./types";
import { terminalStatuses } from "./types";

const WORKSPACE = "/workspace/task";
const PROMPT_PATH = "/workspace/THALA_PROMPT.md";
const RUNNER_PATH = "/workspace/thala-runner.sh";

export interface Executor {
  start(task: StartTaskRequest): Promise<void>;
  cancel(): Promise<void>;
}

export interface StateStore {
  load(): Promise<TaskState>;
  save(state: TaskState): Promise<void>;
  appendLog(stream: "stdout" | "stderr", message: string): Promise<TaskState>;
  transition(status: TaskStatus): Promise<TaskState>;
  fail(code: string, message: string): Promise<TaskState>;
  complete(result: { commit_sha?: string; branch: string; summary: string }): Promise<TaskState>;
}

export class SandboxExecutor implements Executor {
  constructor(
    private readonly env: Env,
    private readonly store: StateStore,
    private readonly remoteRunId: string,
  ) {}

  async start(task: StartTaskRequest): Promise<void> {
    if (!task.execution_policy.allow_network) {
      const message = "Network-disabled execution is not implemented for Cloudflare Sandbox yet";
      await this.store.appendLog("stderr", message);
      await this.store.fail("unsupported_execution_policy", message);
      return;
    }

    let sandbox: ReturnType<typeof getSandbox> | undefined;

    try {
      sandbox = getSandbox(this.env.Sandbox, this.remoteRunId, {
        normalizeId: true,
        keepAlive: true,
      });

      await this.ensureNotCancelled();
      await this.store.transition("booting");
      await this.store.appendLog("stdout", "starting Cloudflare Sandbox");
      await sandbox.exec("mkdir -p /workspace", this.execOptions(task, "/workspace", 30_000));
      await sandbox.writeFile(PROMPT_PATH, task.instruction.prompt);
      await sandbox.writeFile(RUNNER_PATH, runnerScript());
      await this.execChecked(
        sandbox,
        "chmod +x /workspace/thala-runner.sh",
        task,
        "/workspace",
        "prepare runner",
      );

      await this.ensureNotCancelled();
      await this.store.transition("cloning");
      await this.execChecked(
        sandbox,
        "/workspace/thala-runner.sh clone",
        task,
        "/workspace",
        "clone repository",
      );

      await this.ensureNotCancelled();
      if (task.instruction.after_create_hook) {
        await this.execChecked(
          sandbox,
          "/workspace/thala-runner.sh after-create",
          task,
          WORKSPACE,
          "after_create hook",
        );
      }
      if (task.instruction.before_run_hook) {
        await this.execChecked(
          sandbox,
          "/workspace/thala-runner.sh before-run",
          task,
          WORKSPACE,
          "before_run hook",
        );
      }

      await this.ensureNotCancelled();
      await this.store.transition("running");
      await this.execChecked(
        sandbox,
        "/workspace/thala-runner.sh run-agent",
        task,
        WORKSPACE,
        "opencode worker",
      );

      await this.ensureNotCancelled();
      if (task.instruction.after_run_hook) {
        await this.execChecked(
          sandbox,
          "/workspace/thala-runner.sh after-run",
          task,
          WORKSPACE,
          "after_run hook",
        );
      }

      await this.ensureNotCancelled();
      await this.store.transition("pushing");
      const push = await this.execChecked(
        sandbox,
        "/workspace/thala-runner.sh commit-push",
        task,
        WORKSPACE,
        "commit and push",
      );

      const commitSha = extractCommitSha(push.stdout);
      await this.store.complete({
        commit_sha: commitSha,
        branch: task.repo.branch,
        summary: commitSha
          ? `Pushed changes to ${task.repo.branch} at ${commitSha}.`
          : `No changes to push on ${task.repo.branch}.`,
      });
    } catch (error) {
      const current = await this.store.load().catch(() => undefined);
      if (current?.cancelled || current?.status === "cancelled") {
        await this.store.appendLog("stderr", "task cancelled");
        return;
      }

      const message = this.redact(error instanceof Error ? error.message : String(error));
      await this.store.appendLog("stderr", message);
      await this.store.fail("sandbox_execution_failed", message);
    } finally {
      await this.destroySandbox(sandbox);
    }
  }

  async cancel(): Promise<void> {
    try {
      const sandbox = getSandbox(this.env.Sandbox, this.remoteRunId, {
        normalizeId: true,
      });
      await this.killSandboxProcesses(sandbox);
      await this.destroySandbox(sandbox);
    } finally {
      await this.store.transition("cancelled");
      await this.store.appendLog("stderr", "task cancelled");
    }
  }

  private async killSandboxProcesses(sandbox: ReturnType<typeof getSandbox>): Promise<void> {
    try {
      await sandbox.killAllProcesses();
    } catch {
      // Best-effort cleanup. Missing local container support should not block state transition.
    }
  }

  private async destroySandbox(sandbox?: ReturnType<typeof getSandbox>): Promise<void> {
    if (!sandbox) {
      return;
    }
    try {
      await sandbox.destroy();
    } catch {
      // Best-effort cleanup. Production containers are destroyed when the SDK call succeeds.
    }
  }

  private async ensureNotCancelled(): Promise<void> {
    const state = await this.store.load();
    if (state.cancelled || terminalStatuses.has(state.status)) {
      throw new Error("task cancelled");
    }
  }

  private async execChecked(
    sandbox: ReturnType<typeof getSandbox>,
    command: string,
    task: StartTaskRequest,
    cwd: string,
    label: string,
  ): Promise<ExecResult> {
    await this.store.appendLog("stdout", `$ ${label}`);
    const result = await sandbox.exec(
      command,
      this.execOptions(task, cwd, task.execution_policy.max_duration_seconds * 1_000),
    );
    await this.appendExecOutput(result);

    if (!result.success) {
      throw new Error(`${label} failed with exit code ${result.exitCode}`);
    }

    return result;
  }

  private async appendExecOutput(result: ExecResult): Promise<void> {
    for (const line of splitLines(result.stdout)) {
      await this.store.appendLog("stdout", this.redact(line));
    }
    for (const line of splitLines(result.stderr)) {
      await this.store.appendLog("stderr", this.redact(line));
    }
  }

  private execOptions(task: StartTaskRequest, cwd: string, timeout: number): ExecOptions {
    return {
      cwd,
      timeout: Math.max(timeout, 1_000),
      env: {
        THALA_TASK_ID: task.task_id,
        THALA_ATTEMPT: String(task.attempt),
        THALA_REPO_OWNER: task.repo.owner,
        THALA_REPO_NAME: task.repo.name,
        THALA_REPO_BRANCH: task.repo.branch,
        THALA_MODEL: task.instruction.model,
        THALA_WORKDIR: task.instruction.working_dir || ".",
        THALA_MAX_DURATION_SECONDS: String(task.execution_policy.max_duration_seconds),
        THALA_AFTER_CREATE_HOOK: task.instruction.after_create_hook ?? "",
        THALA_BEFORE_RUN_HOOK: task.instruction.before_run_hook ?? "",
        THALA_AFTER_RUN_HOOK: task.instruction.after_run_hook ?? "",
        THALA_ALLOW_NETWORK: task.execution_policy.allow_network ? "1" : "0",
        GIT_TERMINAL_PROMPT: "0",
        GITHUB_TOKEN: this.env.THALA_GITHUB_TOKEN ?? this.env.GITHUB_TOKEN,
        OPENROUTER_API_KEY: this.env.OPENROUTER_API_KEY,
        ANTHROPIC_API_KEY: this.env.ANTHROPIC_API_KEY,
        OPENAI_API_KEY: this.env.OPENAI_API_KEY,
      },
    };
  }

  private redact(text: string): string {
    let redacted = text;
    for (const secret of [
      this.env.THALA_GITHUB_TOKEN,
      this.env.GITHUB_TOKEN,
      this.env.OPENROUTER_API_KEY,
      this.env.ANTHROPIC_API_KEY,
      this.env.OPENAI_API_KEY,
    ]) {
      if (secret) {
        redacted = redacted.split(secret).join("[SECRET REDACTED]");
      }
    }
    return redacted;
  }
}

function splitLines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trimEnd())
    .filter((line) => line.length > 0);
}

function extractCommitSha(stdout: string): string | undefined {
  const match = /THALA_COMMIT_SHA=([0-9a-f]{7,40})/i.exec(stdout);
  return match?.[1];
}

function runnerScript(): string {
  return `#!/usr/bin/env bash
set -euo pipefail

workspace="${WORKSPACE}"
prompt_path="${PROMPT_PATH}"

require_var() {
  local name="$1"
  if [ -z "\${!name:-}" ]; then
    echo "Missing required environment variable: $name" >&2
    exit 2
  fi
}

repo_url() {
  require_var THALA_REPO_OWNER
  require_var THALA_REPO_NAME
  if [ -n "\${GITHUB_TOKEN:-}" ]; then
    printf 'https://x-access-token:%s@github.com/%s/%s.git' "$GITHUB_TOKEN" "$THALA_REPO_OWNER" "$THALA_REPO_NAME"
  else
    printf 'https://github.com/%s/%s.git' "$THALA_REPO_OWNER" "$THALA_REPO_NAME"
  fi
}

run_hook() {
  local hook="$1"
  if [ -n "$hook" ]; then
    run_timed bash -lc "$hook"
  fi
}

run_timed() {
  local seconds="\${THALA_MAX_DURATION_SECONDS:-1800}"
  timeout --kill-after=10s "\${seconds}s" "$@"
}

case "\${1:-}" in
  clone)
    require_var THALA_REPO_BRANCH
    rm -rf "$workspace"
    run_timed git clone --branch "$THALA_REPO_BRANCH" --depth 1 "$(repo_url)" "$workspace"
    cd "$workspace"
    git config user.email "thala-bot@users.noreply.github.com"
    git config user.name "thala-bot"
    ;;
  after-create)
    cd "$workspace"
    run_hook "\${THALA_AFTER_CREATE_HOOK:-}"
    ;;
  before-run)
    cd "$workspace"
    run_hook "\${THALA_BEFORE_RUN_HOOK:-}"
    ;;
  run-agent)
    cd "$workspace/\${THALA_WORKDIR:-.}"
    if [ "\${THALA_ALLOW_NETWORK:-1}" != "1" ]; then
      echo "Network-disabled execution is not implemented for Cloudflare Sandbox yet" >&2
      exit 2
    fi
    if command -v opencode >/dev/null 2>&1; then
      run_timed opencode --model "$THALA_MODEL" --no-session -p "$(cat "$prompt_path")"
    else
      echo "opencode binary not found in sandbox image" >&2
      exit 127
    fi
    ;;
  after-run)
    cd "$workspace"
    run_hook "\${THALA_AFTER_RUN_HOOK:-}"
    ;;
  commit-push)
    cd "$workspace"
    if git diff --quiet && git diff --cached --quiet; then
      echo "No changes to commit"
      exit 0
    fi
    git add -A
    run_timed git commit -m "chore: apply thala task \${THALA_TASK_ID:-unknown}"
    run_timed git push origin "HEAD:\${THALA_REPO_BRANCH}"
    echo "THALA_COMMIT_SHA=$(git rev-parse HEAD)"
    ;;
  *)
    echo "usage: $0 {clone|after-create|before-run|run-agent|after-run|commit-push}" >&2
    exit 2
    ;;
esac
`;
}
