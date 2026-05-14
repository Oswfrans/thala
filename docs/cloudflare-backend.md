# Cloudflare Remote Worker Backend

Thala's Cloudflare backend keeps Rust as the orchestrator and uses Cloudflare as
a narrow remote execution surface.

```text
Rust Thala orchestrator
  -> JSON HTTP API
Cloudflare Worker control plane
  -> Durable Object, one per task attempt
Executor adapter
  -> Cloudflare Sandbox container running OpenCode
```

The Worker does not own product logic, retry policy, model routing, validation,
or merge decisions. Those remain in Rust.

## Monorepo Layout

```text
src/adapters/execution/cloudflare.rs   Rust HTTP client and typed contracts
cloudflare/control-plane/              Cloudflare Worker package
cloudflare/control-plane/src/index.ts  Worker routes and auth forwarding
cloudflare/control-plane/src/task_do.ts Durable Object state machine
cloudflare/control-plane/src/types.ts    TypeScript mirror of the JSON contract
cloudflare/control-plane/src/executor.ts Cloudflare Sandbox execution adapter
cloudflare/control-plane/Dockerfile      OpenCode-enabled sandbox image
```

## HTTP API

All requests use JSON and require:

```http
Authorization: Bearer <token>
```

Endpoints:

```http
POST /tasks/start
GET  /tasks/:remote_run_id/status
GET  /tasks/:remote_run_id/logs?cursor=<number>
POST /tasks/:remote_run_id/cancel
GET  /tasks/:remote_run_id/result
```

`POST /tasks/start` is idempotent for the same task attempt. The remote run ID
is deterministic:

```text
cf-<task_id>-<attempt>
```

The Durable Object name is derived from that run ID, so one task attempt maps to
one supervisor.

## Lifecycle

The explicit remote lifecycle is:

```text
queued -> booting -> cloning -> running -> pushing -> completed
```

Terminal states are:

```text
completed
failed
cancelled
```

Logs are append-only and cursor based. A request with `cursor=42` returns log
lines with `index > 42`.

## Local Development

Run Thala with the Cloudflare backend configured in `WORKFLOW.md`:

```yaml
execution:
  backend: cloudflare
  workspace_root: "."
  github_token_env: THALA_GITHUB_TOKEN
```

Cloudflare completion is detected by polling the control plane status/result
endpoints. Unlike Modal, this backend does not require `callback_base_url` for
normal operation.

Create local Worker secrets in `.dev.vars`:

```bash
cd cloudflare/control-plane
printf 'THALA_SHARED_AUTH_TOKEN=dev-token
THALA_GITHUB_TOKEN=ghp_xxxxxxxxxxxx
OPENROUTER_API_KEY=sk-or-...
' > .dev.vars
```

Start the Worker locally:

```bash
npm install
npx wrangler dev
```

Cloudflare Sandbox uses Containers, so local `wrangler dev` requires Docker.
The first run builds the sandbox image and can take several minutes.

Point Rust at the local Worker:

```bash
export THALA_CF_BASE_URL=http://localhost:8787
export THALA_CF_TOKEN=dev-token
```

Do not put `THALA_SHARED_AUTH_TOKEN`, GitHub tokens, or provider API keys in
`wrangler.jsonc` `vars`; use `.dev.vars` locally and Wrangler secrets for
deployments.

## Environment Variables

Rust side:

```text
THALA_CF_BASE_URL
THALA_CF_TOKEN
THALA_CF_MAX_DURATION_SECONDS optional, defaults to 1800
THALA_CF_ALLOW_NETWORK optional, defaults to true; false is rejected until Sandbox network isolation is implemented
```

Slack support is available at the same Thala process when `slack:` is present
in `WORKFLOW.md`:

```yaml
slack:
  bot_token: "${SLACK_BOT_TOKEN}"
  signing_secret: "${SLACK_SIGNING_SECRET}"
  alerts_channel: "C0123456789"
```

Slack routes:

```text
POST /api/slack/command
POST /api/slack/interaction
```

`THALA_SLACK_BIND` controls the bind address and defaults to
`127.0.0.1:8790`. `SLACK_INTAKE_ENABLED=false` disables slash-command task
creation. `SLACK_INTERACTION_ENABLED=false` disables button callbacks.

Worker side secrets:

```text
THALA_SHARED_AUTH_TOKEN
THALA_GITHUB_TOKEN, required for clone and push
OPENROUTER_API_KEY, ANTHROPIC_API_KEY, or OPENAI_API_KEY, depending on OpenCode provider config
```

Worker side non-secret vars:

```text
SANDBOX_TRANSPORT optional, defaults to websocket in wrangler.jsonc
```

Set Worker secrets for deployment with Wrangler, for example:

```bash
cd cloudflare/control-plane
npx wrangler secret put THALA_SHARED_AUTH_TOKEN
npx wrangler secret put THALA_GITHUB_TOKEN
npx wrangler secret put OPENROUTER_API_KEY
```

## Execution

`SandboxExecutor` uses the official `@cloudflare/sandbox` SDK. For each task
attempt it:

1. Stores task state in a SQLite-backed Durable Object.
2. Schedules the task Durable Object alarm so the HTTP start request can return immediately.
3. Creates or reuses a sandbox by `remote_run_id` when the alarm fires.
4. Writes the rendered prompt to `/workspace/THALA_PROMPT.md`.
5. Clones the task branch into `/workspace/task`.
6. Runs `after_create`, `before_run`, and `after_run` hooks when present.
7. Runs `opencode --model <model> --no-session -p <prompt>`.
8. Commits changed files and pushes back to the task branch.
9. Records the final commit SHA, or records that there were no changes.

Lifecycle hooks are trusted shell snippets from `WORKFLOW.md`. They are
operator-owned configuration and must not be populated from Beads task text or
chat input. The Cloudflare executor runs them through `bash -lc` for normal
shell semantics; a non-zero hook exit fails the sandbox task.

Secrets are not stored in Durable Object state or returned in API responses.
Worker secrets are passed only as per-command environment variables to the
sandbox and log output is redacted before it is appended to task logs.

## Cloudflare Alignment Notes

The control plane follows the current Cloudflare patterns that matter for this backend:

- Uses a Worker plus bindings to Durable Objects and Sandbox, rather than calling Cloudflare REST APIs from inside the Worker.
- Uses `new_sqlite_classes` migrations for the task Durable Object and Sandbox binding.
- Uses Durable Object alarms for per-task background execution instead of putting long OpenCode runs in `waitUntil`.
- Declares required Worker secrets in `wrangler.jsonc` and keeps local values in `.dev.vars`.
- Destroys sandboxes after task completion or cancellation so kept-alive containers do not count against limits indefinitely.

## Remaining Work

The implementation now uses a real Cloudflare Sandbox executor. The remaining
production hardening work is operational:

```text
configure OpenCode provider environment
deploy the Worker and sandbox container
exercise a real task against a test repository
tune sandbox instance size and timeout values
exercise cancellation behavior against long-running real OpenCode sessions
```
