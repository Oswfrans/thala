import { SELF } from "cloudflare:test";
import { describe, expect, test } from "vitest";
import type {
  LogsResponse,
  StartTaskRequest,
  StartTaskResponse,
  TaskResultResponse,
  TaskStatusResponse,
} from "../src/types";

const auth = { authorization: "Bearer dev-token" };

function task(taskId: string, attempt = 1): StartTaskRequest {
  return {
    task_id: taskId,
    attempt,
    repo: {
      provider: "github",
      owner: "Oswfrans",
      name: "thala",
      branch: `task/${taskId}`,
    },
    instruction: {
      prompt: "Implement the task",
      working_dir: ".",
      model: "test-model",
    },
    execution_policy: {
      max_duration_seconds: 60,
      allow_network: false,
    },
  };
}

describe("Cloudflare control plane", () => {
  test("rejects unauthorized requests", async () => {
    const response = await SELF.fetch("https://example.com/tasks/start", {
      method: "POST",
      body: JSON.stringify(task("unauthorized")),
    });

    expect(response.status).toBe(401);
  });


  test("rejects malformed start payloads", async () => {
    const response = await SELF.fetch("https://example.com/tasks/start", {
      method: "POST",
      headers: auth,
      body: JSON.stringify({ task_id: "bad", attempt: 0 }),
    });

    expect(response.status).toBe(400);
  });

  test("starts a sandbox task and treats repeated start as idempotent", async () => {
    const request = task("bd-route-start", 1);
    const first = await start(request);
    const second = await start(request);

    expect(first.remote_run_id).toBe("cf-bd-route-start-1");
    expect(first.status).toBe("queued");
    expect(second.remote_run_id).toBe(first.remote_run_id);
    await waitUntilTerminal(first.remote_run_id);
  });

  test("reports status, incremental logs, and policy errors", async () => {
    const request = task("bd-route-flow", 1);
    const started = await start(request);

    const initialStatus = await getJson<TaskStatusResponse>(
      `/tasks/${started.remote_run_id}/status`,
    );
    expect(["queued", "booting", "cloning", "running", "failed"]).toContain(
      initialStatus.status,
    );

    const terminal = await waitUntilTerminal(started.remote_run_id);
    expect(["completed", "failed"]).toContain(terminal.status);

    const logs = await getJson<LogsResponse>(
      `/tasks/${started.remote_run_id}/logs?cursor=0`,
    );
    expect(logs.lines.length).toBeGreaterThan(0);

    const cursor = logs.next_cursor;
    const nextLogs = await getJson<LogsResponse>(
      `/tasks/${started.remote_run_id}/logs?cursor=${cursor}`,
    );
    expect(nextLogs.lines.every((line) => line.index > cursor)).toBe(true);

    const result = await getJson<TaskResultResponse>(
      `/tasks/${started.remote_run_id}/result`,
    );
    expect(result.status).toBe(terminal.status);
    if (result.status === "completed") {
      expect(result.result?.branch).toBe("task/bd-route-flow");
    } else {
      expect(result.error?.code).toBe("unsupported_execution_policy");
    }
  });

  test("cancel moves a non-terminal task to cancelled", async () => {
    const request = task("bd-route-cancel", 1);
    const started = await start(request);

    const response = await SELF.fetch(
      `https://example.com/tasks/${started.remote_run_id}/cancel`,
      {
        method: "POST",
        headers: auth,
      },
    );
    expect(response.status).toBe(200);

    const status = await getJson<TaskStatusResponse>(
      `/tasks/${started.remote_run_id}/status`,
    );
    expect(["cancelled", "failed"]).toContain(status.status);
  });
});

async function start(request: StartTaskRequest): Promise<StartTaskResponse> {
  const response = await SELF.fetch("https://example.com/tasks/start", {
    method: "POST",
    headers: auth,
    body: JSON.stringify(request),
  });
  expect(response.status).toBe(200);
  return response.json();
}

async function getJson<T>(path: string): Promise<T> {
  const response = await SELF.fetch(`https://example.com${path}`, {
    headers: auth,
  });
  expect(response.status).toBe(200);
  return response.json();
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitUntilTerminal(
  remoteRunId: string,
): Promise<TaskStatusResponse> {
  for (let i = 0; i < 20; i += 1) {
    const status = await getJson<TaskStatusResponse>(
      `/tasks/${remoteRunId}/status`,
    );
    if (["completed", "failed", "cancelled"].includes(status.status)) {
      await sleep(20);
      return status;
    }
    await sleep(50);
  }
  throw new Error(`task ${remoteRunId} did not reach a terminal state`);
}
