import { Sandbox } from "@cloudflare/sandbox";
import { authorize, jsonError, type Env } from "./auth";
import { TaskAttemptDurableObject } from "./task_do";
import { durableObjectName, remoteRunId } from "./types";

export { Sandbox, TaskAttemptDurableObject };

const taskRoute =
  /^\/tasks\/(?<id>[^/]+)\/(?<operation>status|logs|cancel|result)$/;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const authFailure = await authorize(request, env);
    if (authFailure) {
      return authFailure;
    }

    const url = new URL(request.url);

    if (request.method === "POST" && url.pathname === "/tasks/start") {
      return startTask(request, env);
    }

    const match = taskRoute.exec(url.pathname);
    if (!match?.groups) {
      return jsonError("not_found", "unknown route", 404);
    }

    const remoteRunId = decodePathSegment(match.groups.id);
    if (!remoteRunId) {
      return jsonError("bad_request", "invalid remote run ID", 400);
    }
    const operation = match.groups.operation;
    const target = new URL(`https://task.local/${operation}${url.search}`);
    const stub = durableObjectStub(env, remoteRunId);

    return stub.fetch(
      new Request(target, {
        method: request.method,
        headers: request.headers,
        body: request.body,
      }),
    );
  },
};

async function startTask(request: Request, env: Env): Promise<Response> {
  let body: { task_id?: string; attempt?: number };
  try {
    body = await request.clone().json();
  } catch {
    return jsonError("bad_request", "invalid JSON body", 400);
  }

  if (typeof body.task_id !== "string" || body.task_id.length === 0) {
    return jsonError("bad_request", "task_id is required", 400);
  }
  if (typeof body.attempt !== "number" || !Number.isInteger(body.attempt) || body.attempt < 1) {
    return jsonError("bad_request", "attempt must be a positive integer", 400);
  }

  const runId = remoteRunId(body.task_id, body.attempt);
  const target = new URL("https://task.local/start");
  const stub = durableObjectStub(env, runId);

  return stub.fetch(
    new Request(target, {
      method: "POST",
      headers: request.headers,
      body: JSON.stringify(body),
    }),
  );
}

function durableObjectStub(env: Env, remoteRunIdValue: string): DurableObjectStub {
  const id = env.TASK_ATTEMPTS.idFromName(durableObjectName(remoteRunIdValue));
  return env.TASK_ATTEMPTS.get(id);
}


function decodePathSegment(value: string): string | null {
  try {
    return decodeURIComponent(value);
  } catch {
    return null;
  }
}
