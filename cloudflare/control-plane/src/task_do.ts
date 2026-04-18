import { SandboxExecutor, type StateStore } from "./executor";
import { json, jsonError, type Env } from "./auth";
import type {
  CancelTaskResponse,
  LogsResponse,
  StartTaskRequest,
  StartTaskResponse,
  TaskResultResponse,
  TaskState,
  TaskStatus,
  TaskStatusResponse,
} from "./types";
import { remoteRunId, terminalStatuses } from "./types";

export class TaskAttemptDurableObject implements DurableObject {
  private stateStore: DurableStateStore;

  constructor(
    private readonly state: DurableObjectState,
    private readonly env: Env,
  ) {
    this.stateStore = new DurableStateStore(state.storage);
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const operation = url.pathname.split("/").filter(Boolean).at(-1);

    try {
      if (request.method === "POST" && operation === "start") {
        return this.start(await request.json());
      }
      if (request.method === "GET" && operation === "status") {
        return this.status();
      }
      if (request.method === "GET" && operation === "logs") {
        const cursor = Number(url.searchParams.get("cursor") ?? "0");
        return this.logs(Number.isFinite(cursor) ? cursor : 0);
      }
      if (request.method === "POST" && operation === "cancel") {
        return this.cancel();
      }
      if (request.method === "GET" && operation === "result") {
        return this.result();
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "unknown error";
      return jsonError("internal_error", message, 500);
    }

    return jsonError("not_found", "unknown Durable Object operation", 404);
  }

  async alarm(): Promise<void> {
    const state = await this.stateStore.load().catch(() => undefined);
    if (!state || state.cancelled || terminalStatuses.has(state.status)) {
      return;
    }
    if (!state.task) {
      await this.stateStore.fail(
        "missing_task_payload",
        "task payload missing from Durable Object state",
      );
      return;
    }

    const executor = new SandboxExecutor(this.env, this.stateStore, state.remote_run_id);
    await executor.start(state.task);
  }

  private async start(raw: unknown): Promise<Response> {
    let task: StartTaskRequest;
    try {
      task = validateStartTaskRequest(raw);
    } catch (error) {
      const message = error instanceof Error ? error.message : "invalid task request";
      return jsonError("bad_request", message, 400);
    }

    const expectedRunId = remoteRunId(task.task_id, task.attempt);
    const current = await this.stateStore.loadOrCreate(expectedRunId);

    if (current.started) {
      const response: StartTaskResponse = {
        remote_run_id: current.remote_run_id,
        status: current.status,
      };
      return json(response);
    }

    const initialized: TaskState = {
      ...current,
      remote_run_id: expectedRunId,
      task,
      status: "queued",
      phase: "queued",
      started: true,
      updated_at: now(),
    };
    await this.stateStore.save(initialized);
    await this.stateStore.appendLog("stdout", "task queued");

    await this.state.storage.setAlarm(Date.now());

    const response: StartTaskResponse = {
      remote_run_id: expectedRunId,
      status: "queued",
    };
    return json(response);
  }

  private async status(): Promise<Response> {
    const state = await this.stateStore.load();
    const response: TaskStatusResponse = {
      remote_run_id: state.remote_run_id,
      status: state.status,
      phase: state.phase,
      updated_at: state.updated_at,
    };
    return json(response);
  }

  private async logs(cursor: number): Promise<Response> {
    const state = await this.stateStore.load();
    const lines = state.logs.filter((line) => line.index > cursor);
    const nextCursor = lines.at(-1)?.index ?? cursor;
    const response: LogsResponse = {
      remote_run_id: state.remote_run_id,
      lines,
      next_cursor: nextCursor,
      has_more: false,
    };
    return json(response);
  }

  private async cancel(): Promise<Response> {
    const state = await this.stateStore.load();
    if (!terminalStatuses.has(state.status)) {
      const executor = new SandboxExecutor(this.env, this.stateStore, state.remote_run_id);
      await this.stateStore.setCancelled();
      await executor.cancel();
    }

    const updated = await this.stateStore.load();
    const response: CancelTaskResponse = {
      remote_run_id: updated.remote_run_id,
      status: updated.status,
    };
    return json(response);
  }

  private async result(): Promise<Response> {
    const state = await this.stateStore.load();
    const response: TaskResultResponse = {
      remote_run_id: state.remote_run_id,
      status: state.status,
      result: state.result,
      error: state.error,
    };
    return json(response);
  }
}

class DurableStateStore implements StateStore {
  constructor(private readonly storage: DurableObjectStorage) {}

  async load(): Promise<TaskState> {
    const state = await this.storage.get<TaskState>("state");
    if (!state) {
      throw new Error("task has not been started");
    }
    return state;
  }

  async loadOrCreate(remoteRunId: string): Promise<TaskState> {
    const state = await this.storage.get<TaskState>("state");
    if (state) {
      return state;
    }

    const created = now();
    return {
      remote_run_id: remoteRunId,
      status: "queued",
      phase: "queued",
      logs: [],
      next_log_index: 1,
      cancelled: false,
      started: false,
      created_at: created,
      updated_at: created,
    };
  }

  async save(state: TaskState): Promise<void> {
    await this.storage.put("state", state);
  }

  async appendLog(stream: "stdout" | "stderr", message: string): Promise<TaskState> {
    const state = await this.load();
    const updated: TaskState = {
      ...state,
      logs: [
        ...state.logs,
        {
          index: state.next_log_index,
          ts: now(),
          stream,
          message,
        },
      ],
      next_log_index: state.next_log_index + 1,
      updated_at: now(),
    };
    await this.save(updated);
    return updated;
  }

  async transition(status: TaskStatus): Promise<TaskState> {
    const state = await this.load();
    if (terminalStatuses.has(state.status)) {
      return state;
    }

    const updated: TaskState = {
      ...state,
      status,
      phase: status,
      updated_at: now(),
    };

    await this.save(updated);
    return updated;
  }

  async fail(code: string, message: string): Promise<TaskState> {
    const state = await this.load();
    const updated: TaskState = {
      ...state,
      status: "failed",
      phase: "failed",
      error: { code, message },
      updated_at: now(),
    };
    await this.save(updated);
    return updated;
  }

  async complete(result: {
    commit_sha?: string;
    branch: string;
    summary: string;
  }): Promise<TaskState> {
    const state = await this.load();
    const updated: TaskState = {
      ...state,
      status: "completed",
      phase: "completed",
      result,
      updated_at: now(),
    };
    await this.save(updated);
    return updated;
  }

  async setCancelled(): Promise<TaskState> {
    const state = await this.load();
    const updated: TaskState = {
      ...state,
      cancelled: true,
      updated_at: now(),
    };
    await this.save(updated);
    return updated;
  }
}

function validateStartTaskRequest(raw: unknown): StartTaskRequest {
  if (!isRecord(raw)) {
    throw new Error("request body must be an object");
  }

  const taskId = raw.task_id;
  const attempt = raw.attempt;
  const repo = raw.repo;
  const instruction = raw.instruction;
  const policy = raw.execution_policy;

  if (typeof taskId !== "string" || taskId.length === 0) {
    throw new Error("task_id is required");
  }
  if (typeof attempt !== "number" || !Number.isInteger(attempt) || attempt < 1) {
    throw new Error("attempt must be a positive integer");
  }
  if (!isRecord(repo) || repo.provider !== "github") {
    throw new Error("repo.provider must be github");
  }
  if (
    typeof repo.owner !== "string" ||
    typeof repo.name !== "string" ||
    typeof repo.branch !== "string"
  ) {
    throw new Error("repo owner, name, and branch are required");
  }
  if (
    !isRecord(instruction) ||
    typeof instruction.prompt !== "string" ||
    typeof instruction.working_dir !== "string" ||
    typeof instruction.model !== "string"
  ) {
    throw new Error("instruction prompt, working_dir, and model are required");
  }
  if (
    !isRecord(policy) ||
    typeof policy.max_duration_seconds !== "number" ||
    typeof policy.allow_network !== "boolean"
  ) {
    throw new Error("execution_policy is invalid");
  }

  return raw as unknown as StartTaskRequest;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function now(): string {
  return new Date().toISOString();
}
