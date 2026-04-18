import type { ErrorResponse } from "./types";
import type { Sandbox } from "@cloudflare/sandbox";

export interface Env {
  THALA_SHARED_AUTH_TOKEN: string;
  TASK_ATTEMPTS: DurableObjectNamespace;
  Sandbox: DurableObjectNamespace<Sandbox>;
  GITHUB_TOKEN?: string;
  THALA_GITHUB_TOKEN?: string;
  OPENROUTER_API_KEY?: string;
  ANTHROPIC_API_KEY?: string;
  OPENAI_API_KEY?: string;
  SANDBOX_TRANSPORT?: string;
}

export async function authorize(request: Request, env: Env): Promise<Response | null> {
  const expected = env.THALA_SHARED_AUTH_TOKEN;
  const actual = request.headers.get("authorization") ?? "";
  const prefix = "Bearer ";

  if (!expected || !actual.startsWith(prefix)) {
    return jsonError("unauthorized", "missing or invalid bearer token", 401);
  }

  return (await constantTimeEqual(actual.slice(prefix.length), expected))
    ? null
    : jsonError("unauthorized", "missing or invalid bearer token", 401);
}

export function jsonError(code: string, message: string, status: number): Response {
  const body: ErrorResponse = {
    error: { code, message },
  };
  return json(body, status);
}

export function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

async function constantTimeEqual(a: string, b: string): Promise<boolean> {
  const [left, right] = await Promise.all([sha256(a), sha256(b)]);
  let diff = left.length ^ right.length;
  const length = Math.max(left.length, right.length);

  for (let i = 0; i < length; i += 1) {
    diff |= (left[i] ?? 0) ^ (right[i] ?? 0);
  }

  return diff === 0;
}

async function sha256(value: string): Promise<Uint8Array> {
  return new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)),
  );
}
