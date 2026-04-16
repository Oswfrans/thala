# AGENTS.md

This file provides guidance to agentic coding tools when working with code in this repository.

Read this entire file before writing any code. The architecture is specific and the order of operations matters.

---

## What this is

**Thala** is an opinionated open-source agent development framework. Thala autonomously executes development tasks: reads tasks from Beads by default or Notion optionally, reads per-product `AGENTS.md` or `CLAUDE.md` and `WORKFLOW.md`, constructs context-rich prompts, spawns OpenCode coding sessions in tmux worktrees or remote worker backends, monitors those sessions, and handles validation, retries, and escalation. Model routing is config-driven via WORKFLOW.md, never hardcoded.

---

## Two-tier architecture — never conflate these

**Tier 1 — Thala (this repo):** Development-agent orchestrator. Reads tasks from trackers, assembles prompts, spawns and monitors OpenCode workers, handles state transitions, escalates to humans via Discord and Telegram.

**Tier 2 — Workers:** OpenCode sessions running Kimi K2.5 (or Claude Sonnet for hard tasks), each in an isolated git worktree inside a named tmux session. Workers see only code and their task prompt. They never see business context beyond what Thala includes in the prompt.

These two tiers must never be conflated. Thala orchestrates. Workers execute.

---

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Run a specific test suite:

```bash
cargo test --test component               # unit-level component tests
cargo test --test integration             # integration tests
cargo test --test system                  # system tests
cargo test --test live -- --ignored       # live tests (requires credentials)
cargo test --test component <test_name>   # single test by name
```

Full pre-PR validation (recommended):

```bash
./dev/ci.sh all
```

The `ci.sh` script runs inside Docker. Available sub-commands: `build-image`, `lint`, `lint-strict`, `lint-delta`, `test`, `test-component`, `test-integration`, `test-system`, `test-live`, `build`, `audit`, `deny`, `security`, `docker-smoke`, `all`, `clean`.

Build profiles: `release` (size-optimized, fat LTO, single codegen unit), `release-fast` (parallel codegen for powerful machines), `ci` (thin LTO, full parallelism), `dist` (same as `release`).

---

## Integrations to implement

Read `src/tools/mod.rs` before touching any tool implementation. Follow existing patterns exactly. Do not introduce new dependencies without checking `Cargo.toml` first. Adding a tool means: implement `Tool` trait → register in `src/tools/mod.rs` factory → add unit test for vtable wiring + error paths.

### Notion tool (`src/tools/notion_tool.rs`)

Thala's task tracker. Auth: `NOTION_API_TOKEN` env var. Use the Notion REST API v1. Use `reqwest` (already a dependency). Return structured JSON. Database IDs come from WORKFLOW.md config or are passed as parameters.

Required operations: `notion_get_task`, `notion_update_task`, `notion_create_task`, `notion_list_tasks`.

**Notion task schema Thala depends on:**

| Field               | Type     | Notes                                           |
|---------------------|----------|-------------------------------------------------|
| Title               | Text     | Clear, actionable task name                     |
| Status              | Select   | Todo / Ready / In Progress / Done / Blocked     |
| Product             | Select   | One entry per active product                    |
| Priority            | Select   | P0 / P1 / P2 / P3                               |
| Model               | Select   | Populated by Thala on dispatch                    |
| PR                  | Number   | GitHub PR number, populated on completion       |
| Context             | Text     | Customer notes, meeting excerpts                |
| Acceptance Criteria | Text     | Mandatory — Thala skips tasks missing this field  |
| Always Human Review | Checkbox | Billing, auth, migrations — never auto-merged   |
| Attempt             | Number   | Retry count, managed by Thala                     |

Tasks without Acceptance Criteria are invisible to Thala — skip them silently.

### GitHub CLI tool (`src/tools/github.rs`)

Wrap `gh` CLI as a tool. Do not use the GitHub REST API directly — `gh` is already authenticated on the host. Execute via `std::process::Command`. Capture stdout/stderr. Return structured results. Handle non-zero exit codes as tool errors, not panics.

Required operations: `gh_pr_create`, `gh_pr_status`, `gh_issue_create`, `gh_run_status`.

### GCP tool (`src/tools/gcp.rs`)

Wrap `gcloud` CLI. Same pattern as GitHub — CLI only, not the GCP SDK.

Required operations: `gcloud_run_deploy`, `gcloud_run_status`, `gcloud_logs_tail`, `gcloud_secret_get`, `gcloud_tasks_enqueue`.

All operations require `project` and `region` parameters, with fallback to `GCP_PROJECT` and `GCP_REGION` env vars.

**`gcloud_secret_get` output must be masked in all logs. Never return raw secret values in tool results that land in the Thala reasoning context.**

---

## Orchestration layer (`src/orchestrator/`)

Thala's core — separate from thala's base agent loop. Keep these files single-purpose:

- `src/orchestrator/state_machine.rs` — task state transitions
- `src/orchestrator/workflow_loader.rs` — WORKFLOW.md parser + file watcher
- `src/orchestrator/prompt_builder.rs` — Tera template rendering + context assembly
- `src/orchestrator/worker_runner.rs` — tmux + OpenCode spawn + stall detection
- `src/orchestrator/dispatch.rs` — preflight validation + dispatch loop
- `src/orchestrator/monitoring.rs` — monitoring loop + signal file polling

### Task state machine (`src/orchestrator/state_machine.rs`)

```
PENDING → DISPATCHING → RUNNING → VALIDATING → DONE
                                      │
                                      └──(fail/retry)──► RUNNING (if attempt < 3)
                                      └──(max retries)──► STUCK
RUNNING ──(stall timeout)──────────────────────────────► STUCK
STUCK ──(human resolves)───────────────────────────────► RESOLVED
```

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,       // In Notion Ready; not yet dispatched
    Dispatching,   // Workspace creating, prompt building
    Running,       // OpenCode session active in tmux
    Validating,    // PR open, CI running
    Done,          // PR merged, Notion updated
    Stuck,         // Max retries or stall timeout — requires human
    Resolved,      // Human resolved; archived
}

pub struct TaskRecord {
    pub id: String,                              // Notion task ID e.g. "MKT-42"
    pub product: String,                         // Product slug
    pub status: TaskStatus,
    pub attempt: u32,
    pub tmux_session: Option<String>,
    pub worktree_path: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity_at: Option<DateTime<Utc>>, // Updated from tmux output, not signal file
    pub pr_number: Option<u32>,
    pub last_tmux_capture: Option<String>,        // Previous capture for change detection
}
```

**Rules:**
- Never mutate `task.status` directly — always call `transition(task, new_status, reason)`
- `transition()` must validate the transition is legal and return `Err` if not
- Invalid transitions are logged as errors and posted to Discord `#thala-alerts`
- All transitions write to `.thala/active-tasks.json` atomically

### WORKFLOW.md (`src/orchestrator/workflow_loader.rs`)

Every product repo has two files Thala reads on each dispatch:

- `AGENTS.md` or `CLAUDE.md` — codebase orientation for the worker agent
- `WORKFLOW.md` — orchestration contract for Thala

These are different files with different audiences. Do not conflate them.

WORKFLOW.md has YAML front matter + a Tera template body. Example:

```yaml
---
tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /workspaces/example-app
  beads_ready_status: open

workspace:
  root: /workspaces/example-app

hooks:
  after_create: "npm install && npm run db:migrate"
  before_run: "git pull --rebase origin main"
  after_run: "npm test"
  before_remove: ""

agent:
  max_concurrent_agents: 3
  stall_timeout_ms: 300000
  max_retries: 3
  model_default: "kimi-k2.5"
  model_hard_tasks: "claude-sonnet"

polling:
  interval_ms: 60000
---
You are an expert developer working on {{ product_name }}.
...
Write DONE to `.thala/signals/{{ issue.identifier }}.signal` when complete
```

**Critical — strict template rendering:** Use Tera with `tera.render()` and treat unknown variables as errors. If template rendering fails for a task, log the error, skip that task's dispatch, and post a warning to `#thala-alerts`. Never silently produce empty variable substitutions — an empty `{{ issue.context }}` in the prompt produces a confused worker.

**Hot reload without restart:** Use the `notify` crate to watch WORKFLOW.md for changes. On change, re-parse and apply to future dispatches. If the new config fails YAML or Tera parse, log the error, post to `#thala-alerts`, and keep the last known good config. Never let a bad WORKFLOW.md crash Thala or affect in-flight sessions.

### Worker runner (`src/orchestrator/worker_runner.rs`)

Spawns OpenCode in tmux. This is NOT thala's own agent loop — Thala is the orchestrator, OpenCode is the worker.

Spawn sequence:
1. Create git worktree: `git worktree add <path> -b task/<id>`
2. Write rendered prompt to `.thala/prompts/<id>.md`
3. Run `after_create` hook in worktree
4. Run `before_run` hook in worktree
5. Spawn tmux session: `tmux new-session -d -s thala-<product>-<id> -c <worktree> opencode --model <model> --no-session -p "$(cat <prompt_path>)"`

**Signal file pattern:** Workers write `DONE` to `.thala/signals/<task-id>.signal` on completion. Thala's monitoring loop polls for this file every 60s. This is how Thala knows a worker finished — not from tmux session death, which can also mean a crash.

**Stall detection — critical detail:** `last_activity_at` must be updated from tmux output changes, NOT from the signal file check. A worker can be alive and producing no output (stalled reasoning loop). A worker can also be dead without writing a signal file. Check both independently:

```rust
// In monitoring loop, every 60s:
let capture = run_cmd("tmux", &["capture-pane", "-t", &session, "-p", "-S", "-100"])?;

if capture != task.last_tmux_capture {
    task.last_activity_at = Some(Utc::now());
    task.last_tmux_capture = Some(capture);
}

let stalled_ms = Utc::now()
    .signed_duration_since(task.last_activity_at.unwrap_or(task.started_at.unwrap()))
    .num_milliseconds();

if stalled_ms > workflow.agent.stall_timeout_ms as i64 {
    transition(&mut task, TaskStatus::Stuck, "Stall timeout")?;
    post_escalation(&task, "Stall timeout — no agent output").await?;
}
```

### Dispatch preflight (`src/orchestrator/dispatch.rs`)

Run before every dispatch tick. Skip dispatch and post to `#thala-alerts` if any check fails. Do not panic — Thala must stay alive even when things are broken:

1. WORKFLOW.md loaded and valid
2. Notion API reachable (lightweight ping)
3. `opencode` binary exists on PATH
4. `gh` binary exists on PATH
5. `gcloud` binary exists on PATH
6. Product git repo is accessible at workspace root
7. Concurrency not already at `max_concurrent_agents` limit

### Monitoring loop (`src/orchestrator/monitoring.rs`)

Runs every 60 seconds independently of the dispatch loop:

1. For each `Running` task: capture tmux output → update `last_activity_at` if changed → check stall timeout → check for `.thala/signals/<id>.signal`
2. For each `Validating` task: check CI via `gh run status` → check PR for `[CRITICAL]` review comments → check `always_human_review` from Notion → if all green AND not human review: post to Discord for merge approval; if CI failed: retry or Stuck; if human review required: post to Discord `#escalations` and Telegram

---

## Escalation

Use thala's built-in Discord and Telegram channels — do not add new channel implementations. Escalation fires on:

- Task stuck (max retries or stall timeout)
- Tool error after 2 retries
- Always-human-review task ready for merge
- `[CRITICAL]` PR review comment
- WORKFLOW.md reload failure
- Preflight failure persisting > 2 ticks

Discord `#thala-alerts`: all events above. Telegram: stall timeouts and `[CRITICAL]` reviews only.

Escalation payload must include: task ID, product, tool or step that failed, error message, and a suggested human action.

---

## thala-core is permanently human-reviewed

Thala can update herself via the same OpenCode + worktree + PR workflow. She gets a Notion task tagged for `thala-core`, runs the worker, and opens a PR.

**Hard rule: Thala never auto-merges changes to herself. Ever.**

Enforced in the monitoring loop: if `task.product == "thala-core"` and the task reaches Validating, always route to Discord escalation regardless of CI status. This path must not be configurable, removable, or overridable by WORKFLOW.md.

---

## Security rules

- Never log or return secrets, tokens, or API keys in tool output or Thala's reasoning context
- All `std::process::Command` calls must not interpolate user-provided values into shell strings — use argument arrays, never `sh -c "... {user_input}"`
- `gcloud_secret_get` results are masked in logs: `[SECRET REDACTED]`
- Notion API token, Discord webhook, Telegram token all come from env vars only

---

## Provider setup

Route through OpenRouter as the primary provider. Model is config-driven via WORKFLOW.md `agent.model_default` and `agent.model_hard_tasks`. Never hardcode a model name or API provider in Rust source.

---

## Definition of done for each integration

Before any integration is considered complete:

- [ ] `cargo build --release` succeeds with no warnings
- [ ] `cargo test` passes
- [ ] Unit test for vtable wiring
- [ ] Unit test for error path (non-zero exit, API error, malformed response)
- [ ] `#[cfg(test)]` guard on anything spawning processes or making network calls
- [ ] No secrets in source, logs, or tool output
- [ ] Escalation fires correctly on repeated failure

---

## Thala base architecture

Core architecture is trait-driven and modular. Extend by implementing traits and registering in factory modules.

Key extension points (trait → factory registration location):

- `src/providers/traits.rs` (`Provider`) → `src/providers/mod.rs` `create_provider_with_url`
- `src/channels/traits.rs` (`Channel`) → `src/channels/mod.rs`
- `src/tools/traits.rs` (`Tool`) → `src/tools/mod.rs`
- `src/memory/traits.rs` (`Memory`) → `src/memory/mod.rs`
- `src/observability/traits.rs` (`Observer`) → `src/observability/mod.rs`
- `src/hooks/traits.rs` (`Hook`) → `src/hooks/mod.rs`
- `src/runtime/traits.rs` (`RuntimeAdapter`) → `src/runtime/mod.rs`
- `src/peripherals/traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)

Tool descriptions are internationalized: add entries to `tool_descriptions/en.toml` (and `zh-CN.toml` for parity) when adding a new tool.

## Workspace

The repository is a Cargo workspace with one member:

- `.` — main `thala` binary + library crate (package name: `thala`, MSRV 1.92)

Optional cargo features (notable non-default ones): `channel-matrix`, `channel-lark` / `channel-feishu`, `memory-postgres`, `observability-otel`, `hardware`, `peripheral-rpi`, `browser-native`, `sandbox-landlock`, `sandbox-bubblewrap`, `plugins-wasm`, `whatsapp-web`, `rag-pdf`, `probe`. Default features include `observability-prometheus`, `channel-nostr`, `skill-creation`.

## Repository Map

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module exports and shared command enums
- `src/config/` — schema + config loading/merging; config resolves via `THALA_WORKSPACE` env → `active_workspace.toml` marker → `~/.thala/config.toml`
- `src/orchestrator/` — **Thala's orchestration layer** (state machine, workflow loader, prompt builder, worker runner, dispatch, monitoring)
- `src/agent/` — thala base agent loop
  - `agent.rs` / `mod.rs` — `Agent` struct + `AgentBuilder`
  - `loop_.rs` — core agentic tool-use loop (streaming, iteration cap, model-switch)
  - `dispatcher.rs` — native vs XML tool dispatch strategies
  - `classifier.rs` — query classification for hint-based model routing
  - `prompt.rs` — system prompt construction
  - `memory_loader.rs` — memory injection into context
- `src/gateway/` — Axum HTTP/WebSocket gateway server (REST API, SSE, pairing)
- `src/security/` — policy, pairing, secret store
- `src/auth/` — OAuth and token auth flows for providers (Anthropic, OpenAI, Gemini)
- `src/memory/` — markdown/sqlite/postgres memory backends + embeddings/vector merge + knowledge graph
- `src/providers/` — model providers (Anthropic, OpenAI, Gemini, Ollama, OpenRouter, GLM, Bedrock, etc.) + `ReliableProvider` fallback wrapper
- `src/channels/` — messaging channels (Telegram, Discord, Slack, WhatsApp, Matrix, IRC, etc.) + session store
- `src/tools/` — tool execution surface (shell, file, memory, browser, MCP, cron, Notion, GitHub CLI, GCP CLI, etc.)
- `src/cron/` — scheduled task engine (cron, fixed-interval, one-shot, delay)
- `src/hooks/` — lifecycle hooks (command logger, webhook audit); built-ins in `hooks/builtin/`
- `src/skills/` — skill system; autonomous skill creation from successful multi-step tasks (feature-gated)
- `src/rag/` — RAG pipeline (chunking, PDF ingestion, vector retrieval)
- `src/nodes/` — multi-node agent coordination
- `src/plugins/` — WASM plugin runtime via extism (feature-gated: `plugins-wasm`)
- `src/peripherals/` — hardware peripherals (STM32, RPi GPIO)
- `src/runtime/` — runtime adapters (currently native)
- `src/observability/` — Prometheus metrics + OpenTelemetry trace/metrics export
- `src/cost/` — token cost tracking per session
- `src/daemon/` — background daemon management (systemd/launchd integration)
- `src/approval/` — human-in-the-loop approval flow for tool calls
- `src/i18n.rs` — tool description i18n loading
- `src/multimodal/` — multimodal content (image, audio) handling
- `src/tunnel/` — tunnel support for exposing the gateway
- `src/migration/` — data migration (e.g., OpenClaw → Thala workspace import)
- `docs/` — topic-based documentation
- `.github/` — CI, templates, automation workflows

## Risk Tiers

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `src/**` behavior changes without boundary/security impact
- **High risk**: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `src/orchestrator/**`, `.github/workflows/**`, access-control boundaries

When uncertain, classify as higher risk.

## Workflow

1. **Read before write** — inspect existing module, factory wiring, and adjacent tests before editing.
2. **One concern per PR** — avoid mixed feature+refactor+infra patches.
3. **Implement minimal patch** — no speculative abstractions, no config keys without a concrete use case.
4. **Validate by risk tier** — docs-only: lightweight checks. Code changes: full relevant checks.
5. **Document impact** — update PR notes for behavior, risk, side effects, and rollback.
6. **Queue hygiene** — stacked PR: declare `Depends on #...`. Replacing old PR: declare `Supersedes #...`.

Branch/commit/PR rules:
- Work from a non-`master` branch. Open a PR to `master`; do not push directly.
- Use conventional commit titles. Prefer small PRs (`size: XS/S/M`).
- Follow `.github/pull_request_template.md` fully.
- Never commit secrets, personal data, or real identity information.

## Anti-Patterns

- Do not add MCP — not used in this setup
- Do not add plan mode or sub-agent frameworks — Thala's orchestration is handled by `src/orchestrator/`
- Do not modify the `Provider` trait or memory backend
- Do not introduce async runtimes beyond what tokio already provides
- Do not create God structs — keep each file single-purpose
- Do not auto-merge `thala-core` PRs under any circumstances
- Do not use `sh -c` with interpolated values anywhere — always use argument arrays
- Do not silently swallow template rendering errors — fail the dispatch for that task
- Do not hardcode model names or API provider names in Rust source
- Do not add heavy dependencies for minor convenience
- Do not silently weaken security policy or access constraints
- Do not add speculative config/feature flags "just in case"
- Do not modify unrelated modules "while here"

## Linked References

- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract, i18n rules, locale parity
