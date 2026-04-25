# AGENTS.md

This file provides guidance to agentic coding tools when working with code in this repository.

Read this entire file before writing any code. The architecture is specific and the order of operations matters.

---

## What this is

**Thala** is an opinionated orchestration kernel for managed coding tasks. It is not a general-purpose agent framework.

Its responsibility is to:

1. Ingest canonical tasks from Beads
2. Build execution context from repository state and workflow config
3. Launch execution runs on a selected backend (local, Modal, Cloudflare)
4. Monitor progress and detect stalls or failures
5. Validate results
6. Involve humans via Slack/Discord when required
7. Finish, retry, reroute, or resolve tasks

Thala separates task-level truth, runtime execution, and human interaction into distinct subsystems.

---

## Two-tier architecture — never conflate these

**Tier 1 — Thala (this repo):** Orchestrator. Reads tasks from Beads, builds prompts, launches and monitors workers, handles state transitions, escalates to humans via Discord/Slack.

**Tier 2 — Workers:** Coding agent sessions running on a configured backend. Each sees only the rendered prompt and the product codebase. Workers never see Thala's orchestration context.

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

Build profiles: `release` (size-optimized, fat LTO, single codegen unit), `release-fast` (parallel codegen), `ci` (thin LTO, full parallelism).

---

## Core Architecture

The codebase is structured into four main layers:

```
core/           → domain model and pure logic (no I/O)
ports/          → trait boundaries for external systems
adapters/       → concrete implementations of ports
orchestrator/   → application logic and execution loops
```

Design principles:
- Keep the core small and explicit
- Separate what the system *is* (core) from how it *connects* (adapters)
- Model task lifecycle and run lifecycle explicitly
- Keep external systems out of core logic
- Prefer clarity over abstraction

---

## Canonical Sources of Truth

**Beads (task-level truth)**

Beads is the canonical system for task existence, metadata (title, description, priority, context), human-authored updates, and high-level task status. Thala reads tasks from Beads and may write updates back via a controlled interface.

**Thala state store (runtime truth)**

Thala maintains its own durable state for: task execution status (`TaskRecord`), execution attempts (`TaskRun`), worker handles and backend details, monitoring timestamps, and orchestration events. This state is not stored in Beads.

---

## Interaction Model

Human interaction is a first-class subsystem, not just escalation.

Thala may: notify humans of progress or issues, request approvals or decisions, request missing context, allow retries or rerouting, or allow manual resolution.

```
Thala → InteractionLayer → Slack/Discord → Human
                                     ↓
                              InteractionResolution
                                     ↓
                                   Thala
```

Core types: `InteractionRequest`, `InteractionAction` (approve, retry, cancel, etc.), `InteractionResolution`.

Slack and Discord are interaction adapters — not sources of truth.

---

## Task Ingestion Model

Tasks always become canonical via Beads.

- Slack/Discord → intake adapter → Beads (TaskSink)
- Beads → TaskSource → Thala

Slack and Discord may create tasks, append context, or update task fields — but they do not create tasks directly inside Thala.

---

## Architecture — Ports & Adapters

The codebase follows a strict ports-and-adapters layout. The orchestrator only imports port traits — never concrete adapter types.

```
┌─────────────────────────────────────┐
│           OrchestratorEngine        │
│  Scheduler │ Dispatcher │ Monitor   │
│  Validator │ HumanLoop │ Reconciler │
└────────────────┬────────────────────┘
                 │ depends only on traits
        ┌────────▼────────┐
        │      PORTS       │
        │ ExecutionBackend │
        │ TaskSource/Sink  │
        │ StateStore       │
        │ RepoProvider     │
        │ InteractionLayer │
        └────────┬────────┘
                 │ implemented by
     ┌───────────▼───────────────┐
     │        ADAPTERS           │
     │ Local/Modal/CF/Zen        │
     │ Beads source/sink         │
     │ SQLite state store        │
     │ GitHub repo provider      │
     │ Slack/Discord interaction │
     └───────────────────────────┘
```

Extension points (trait → registration):

- `src/ports/execution.rs` (`ExecutionBackend`) → `src/adapters/execution/router.rs`
- `src/ports/task_source.rs` (`TaskSource`) → wired in `src/main.rs`
- `src/ports/task_sink.rs` (`TaskSink`) → wired in `src/main.rs`
- `src/ports/state_store.rs` (`StateStore`) → `src/adapters/state/`
- `src/ports/repo.rs` (`RepoProvider`) → `src/adapters/repo/`
- `src/ports/interaction.rs` (`InteractionLayer`) → `src/adapters/interaction/`
- `src/ports/validator.rs` (`Validator`) → `src/adapters/validation/`

Adding a new adapter means: implement the port trait → register in `src/main.rs` → add unit tests for trait wiring and error paths.

---

## Repository Map

```
thala/
├── src/
│   ├── main.rs                   # CLI entrypoint (run, validate, onboard)
│   ├── lib.rs                    # Module exports
│   ├── core/                     # Pure domain types — no I/O, no side effects
│   │   ├── task.rs               # TaskSpec, TaskRecord, TaskStatus (10-state enum)
│   │   ├── run.rs                # TaskRun, RunStatus (5-state), WorkerHandle
│   │   ├── workflow.rs           # WorkflowConfig (parsed from WORKFLOW.md front-matter)
│   │   ├── events.rs             # OrchestratorEvent enum
│   │   ├── transitions.rs        # apply_transition() — state machine guard
│   │   ├── ids.rs                # TaskId, RunId type aliases
│   │   ├── interaction.rs        # InteractionRequest/Ticket/Action types
│   │   ├── error.rs              # ThalaError domain errors
│   │   ├── state.rs              # StateError for illegal transitions
│   │   └── validation.rs        # Validator trait stubs
│   ├── ports/                    # Async traits — abstraction boundaries
│   │   ├── execution.rs          # ExecutionBackend trait
│   │   ├── state_store.rs        # StateStore trait
│   │   ├── task_source.rs        # TaskSource trait
│   │   ├── task_sink.rs          # TaskSink trait
│   │   ├── repo.rs               # RepoProvider trait
│   │   ├── backend_router.rs     # BackendRouter trait
│   │   ├── interaction.rs        # InteractionLayer trait
│   │   └── validator.rs          # Validator trait
│   ├── adapters/                 # Concrete I/O implementations
│   │   ├── beads/
│   │   │   ├── source.rs         # Runs `bd ready --json` → TaskSpec[]
│   │   │   └── sink.rs           # Runs `bd create/update/close` for write-backs
│   │   ├── execution/
│   │   │   ├── local.rs          # tmux + git worktree on Thala host
│   │   │   ├── modal.rs          # Serverless container via `modal run --detach`
│   │   │   ├── cloudflare.rs     # Sandboxed container via HTTP control-plane API
│   │   │   └── router.rs         # DefaultBackendRouter implementation
│   │   ├── state/                # SqliteStateStore — tasks.db
│   │   ├── repo/                 # GitRepoProvider — push branches, create PRs
│   │   ├── validation/           # NoopValidator + ReviewAiValidator stubs
│   │   ├── intake/               # Slack/Discord → Beads integration
│   │   └── interaction/          # Slack/Discord human-approval handling
│   └── orchestrator/
│       ├── engine.rs             # OrchestratorEngine — wires all subsystems
│       ├── scheduler.rs          # Polls TaskSource every 30s, emits DispatchReady
│       ├── dispatcher.rs         # Consumes DispatchReady, launches runs
│       ├── monitor.rs            # Polls active runs, detects stalls/completion
│       ├── validator.rs          # Validates PRs (review AI + CI checks)
│       ├── human_loop.rs         # Manages InteractionTickets, awaits human decisions
│       ├── reconciler.rs         # Crash recovery on startup
│       ├── callback_server.rs    # HTTP webhook receiver for remote completions
│       └── prompt_builder.rs     # Tera template rendering
├── cloudflare/control-plane/     # TypeScript Cloudflare Worker + Durable Object
├── tests/
│   ├── component/                # Unit tests
│   ├── integration/              # Multi-module tests
│   ├── system/                   # Full end-to-end with mock backends
│   └── live/                     # Real backend tests (opt-in, requires creds)
├── docs/
├── examples/                     # Example WORKFLOW.md files
├── dev/
│   ├── setup.sh                  # Installs bd, opencode, tmux, etc.
│   ├── ci.sh                     # Lint + test + build + audit
│   └── docker/                   # CI Docker image
└── Cargo.toml
```

---

## Orchestration layer (`src/orchestrator/`)

Thala's core — separate from any worker agent loop. Keep these files single-purpose.

### Task state machine (`src/core/transitions.rs`)

```
Pending → Ready → Dispatching → Running → Validating → Succeeded
                                    │
                                    └──(stall)──► Stuck
                                    └──(fail/retry)──► Dispatching (attempt < max)
                                    └──(max retries)──► Failed

Running/Validating ──(human action)──► WaitingForHuman ──► back to flow
Stuck/Failed ──(human override)──► Resolved
```

```rust
pub enum TaskStatus {
    Pending,           // Ingested; not yet assessed for dispatch readiness
    Ready,             // Beads confirms task is ready; waiting for a slot
    Dispatching,       // Context assembling, run being prepared
    Running,           // Active on an execution backend
    WaitingForHuman,   // Awaiting human decision (approval, context, retry)
    Validating,        // PR open, CI running, review AI evaluating
    Succeeded,         // PR merged, written back to Beads
    Failed,            // Max retries exceeded or hard error
    Stuck,             // Stall timeout — requires human
    Resolved,          // Human explicitly closed/archived
}
```

**Rules:**
- Never mutate `task.status` directly — always call `apply_transition(task, new_status)`
- `apply_transition()` validates the transition is legal and returns `StateError` if not
- Invalid transitions are logged as errors and posted to the configured alerts channel
- All state changes are persisted atomically via `StateStore`

### Domain types (`src/core/task.rs`)

```rust
pub struct TaskSpec {
    pub id: TaskId,
    pub title: String,
    pub acceptance_criteria: String,    // Required — tasks without AC are skipped
    pub context: String,
    pub beads_ref: String,
    pub model_override: Option<String>,
    pub always_human_review: bool,
    pub labels: Vec<String>,
}

pub struct TaskRecord {
    pub spec: TaskSpec,
    pub attempt: u32,
    pub status: TaskStatus,
    pub active_run_id: Option<RunId>,
    pub ingested_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub reroute_hint: Option<ExecutionBackendKind>,
}
```

`TaskSpec` is immutable after ingestion — Thala is never the source of truth for task description. Tasks without `acceptance_criteria` are silently skipped by the scheduler.

### Scheduler (`src/orchestrator/scheduler.rs`)

Polls `TaskSource::fetch_ready()` every 30s. Deduplicates against in-flight tasks. Checks `max_concurrent_runs` headroom. Emits `DispatchReady` per eligible task.

### Dispatcher (`src/orchestrator/dispatcher.rs`)

Consumes `DispatchReady`. Loads/creates `TaskRecord`, increments attempt. Routes to backend (respects `reroute_hint`). Renders WORKFLOW.md Tera body → prompt. For remote backends: pushes `task/<id>` branch to GitHub first. Calls `backend.launch(LaunchRequest)` → `WorkerHandle`. Persists new `TaskRun` to SQLite.

**Critical — strict template rendering:** Use Tera with unknown variables as errors. If rendering fails for a task, log the error, skip that task's dispatch, and post a warning to the alerts channel. Never silently produce empty variable substitutions.

### Monitor (`src/orchestrator/monitor.rs`)

Polls active runs every 15–60s. Calls `backend.observe(handle, prev_cursor)`. If cursor changed: updates `last_activity_at`. If no change for `stall_timeout_ms`: transitions to Stuck, alerts Discord/Slack. If completion signal detected: emits `RunCompleted`.

**Stall detection detail:** `last_activity_at` must be updated from backend output changes. A worker can be alive producing no output (stalled reasoning loop). Check stall and completion independently.

Local backend stall detection:
```rust
let capture = run_cmd("tmux", &["capture-pane", "-t", &session, "-p", "-S", "-100"])?;
// cursor = SHA-256 of capture; compare to last_observation_cursor
```

### Validator (`src/orchestrator/validator.rs`)

Consumes `RunCompleted`. Runs review AI if enabled. Creates GitHub PR. Polls CI checks; on pass + `auto_merge: true` → merges PR. On failure: injects feedback into next attempt, checks retry budget. Routes human-required PRs to `InteractionLayer`.

### HumanLoop (`src/orchestrator/human_loop.rs`)

Manages `InteractionTicket`s. Listens on Slack/Discord for responses. Applies decisions: `merge`, `retry`, `reroute`, `resolve`, `escalate`.

### Reconciler (`src/orchestrator/reconciler.rs`)

Runs once at startup. Recovers `Launching`/`Running` tasks from prior crash. Re-queries backends; resets valid ones to `Active`, cancels dead ones.

### CallbackServer (`src/orchestrator/callback_server.rs`)

Axum HTTP server. Remote workers POST completion here with a bearer token. Validates token hash against `TaskRun.callback_token_hash`. Emits `RunCompleted`.

---

## WORKFLOW.md (`src/core/workflow.rs`)

Each product has its own `WORKFLOW.md`. The YAML front-matter is the `WorkflowConfig`; the body is a Tera template rendered into the worker prompt at dispatch.

```yaml
---
product: "my-app"
github_repo: "org/repo"

tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /workspaces/my-app
  beads_ready_status: open

execution:
  backend: local           # local | modal | cloudflare
  workspace_root: /workspaces/my-app
  github_token_env: THALA_GITHUB_TOKEN
  callback_base_url: https://thala.example.com  # required for remote backends

models:
  worker: "opencode/kimi-k2.5"
  manager: "anthropic/claude-opus-4-6"
  max_review_cycles: 2

limits:
  max_concurrent_runs: 3
  stall_timeout_ms: 300000

retry:
  max_attempts: 3
  allow_backend_reroute: false
  reroute_to: null

merge:
  auto_merge: false
  protected_paths:
    - "auth/**"
    - "**/migrations/**"
  required_checks:
    - "typecheck"
    - "lint"

hooks:
  after_create: "npm install"
  before_run: "git pull --rebase --autostash origin main"
  after_run: ""
  before_cleanup: ""

stuck:
  auto_resolve_after_ms: 0

discord:
  bot_token: "Bot ..."
  public_key: ""
  alerts_channel_id: "..."

# slack:
#   bot_token: "xoxb-..."
#   signing_secret: "..."
#   alerts_channel: "#thala-alerts"
---
You are an expert developer on {{ product_name }}.

## Task
**ID:** {{ issue.identifier }}
**Title:** {{ issue.title }}
**Attempt:** {{ run.attempt }}

## Acceptance Criteria
{{ issue.acceptance_criteria }}

Write DONE to `.thala/signals/{{ issue.identifier }}.signal` when complete.
```

**Hot reload:** Use the `notify` crate to watch WORKFLOW.md for changes. On change, re-parse and apply to future dispatches. If the new config fails YAML or Tera parse, log the error, post to alerts, and keep the last known good config. Never crash Thala on a bad WORKFLOW.md.

---

## Execution backends (`src/adapters/execution/`)

| Backend | How It Runs | Completion Detection |
|---|---|---|
| `local` | `tmux new-session` + `git worktree` | Signal file `.thala/signals/<id>.signal` |
| `modal` | `modal run --detach` | HTTP callback to CallbackServer |
| `cloudflare` | HTTP POST to control-plane Worker | Polling — control-plane reports status |

All backends implement `ExecutionBackend`. The orchestrator never inspects handles directly.

Local spawn sequence:
1. `git worktree add <path> -b task/<id>`
2. Write rendered prompt to `.thala/prompts/<id>.md`
3. Run `after_create` hook in worktree
4. Run `before_run` hook in worktree
5. `tmux new-session -d -s thala-<product>-<id> -c <worktree> opencode --model <model> --no-session -p "$(cat <prompt_path>)"`

---

## State persistence

SQLite at `~/.local/share/thala/state.db`:

| Table | Contents |
|---|---|
| `task_records` | TaskRecord rows (status, attempt, active_run_id, reroute_hint) |
| `task_runs` | TaskRun rows (status, handle, backend, cursors, timestamps, token hash) |
| `interaction_tickets` | Open human approval requests |

---

## Escalation

Use the configured Discord and/or Slack channels. Escalation fires on:

- Task stuck (stall timeout)
- Max retries exceeded
- Always-human-review task ready for merge
- `[CRITICAL]` PR review comment
- WORKFLOW.md reload failure
- Preflight failure persisting > 2 ticks
- Tool or backend error after 2 retries

Escalation payload must include: task ID, product, step that failed, error message, and suggested human action.

---

## thala-core is permanently human-reviewed

Thala can update herself via the same worker + PR workflow. **Hard rule: Thala never auto-merges changes to herself. Ever.**

Enforced in the validator: if `task.product == "thala-core"` and the task reaches Validating, always route to Discord/Slack escalation regardless of CI status. This path must not be configurable, removable, or overridable by WORKFLOW.md.

---

## Security rules

- Never log or return secrets, tokens, or API keys
- All `std::process::Command` calls must use argument arrays — never `sh -c "... {user_input}"`
- Callback tokens: UUID per `TaskRun`; only the SHA-256 hash stored; raw token sent only to the worker
- Discord signature verification: Ed25519 public key validation on all webhook payloads
- `THALA_GITHUB_TOKEN` and all other secrets come from env vars only

---

## Non-Goals

Thala is intentionally not:

- A general-purpose multi-agent framework
- A no-code automation platform
- A plugin marketplace
- A long-term memory system
- A chat-first system

---

## Definition of done for each integration

Before any integration is considered complete:

- [ ] `cargo build --release` succeeds with no warnings
- [ ] `cargo test` passes
- [ ] Unit test for trait vtable wiring
- [ ] Unit test for error path (non-zero exit, API error, malformed response)
- [ ] `#[cfg(test)]` guard on anything spawning processes or making network calls
- [ ] No secrets in source, logs, or tool output
- [ ] Escalation fires correctly on repeated failure

---

## Workspace

Single Cargo workspace with one member:

- `.` — main `thala` binary + library crate (package name: `thala`, MSRV 1.92)

No feature flags are currently defined. Do not add speculative features.

---

## Risk Tiers

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `src/**` behavior changes without boundary/security impact
- **High risk**: `src/core/**`, `src/orchestrator/**`, `src/ports/**`, `src/adapters/execution/**`, `.github/workflows/**`, access-control boundaries

When uncertain, classify as higher risk.

---

## Development Guidelines

When contributing:

- Prefer deleting complexity over adding abstraction
- Keep core logic pure and minimal
- Keep adapters thin and focused
- Do not leak: Slack/Discord payloads into core; Modal/Cloudflare specifics into core; Beads schema into core
- Prefer explicit enums and structs
- Avoid unnecessary generics or macros
- Keep orchestration loops readable and small

If unsure: choose the simpler, narrower design.

---

## Workflow

1. **Read before write** — inspect existing module, trait wiring, and adjacent tests before editing.
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

---

## Anti-Patterns

- Do not add MCP — not used in this setup
- Do not add plan mode or sub-agent frameworks — Thala's orchestration is in `src/orchestrator/`
- Do not let the orchestrator import concrete adapter types — only port traits
- Do not introduce async runtimes beyond what tokio already provides
- Do not create God structs — keep each file single-purpose
- Do not auto-merge `thala-core` PRs under any circumstances
- Do not use `sh -c` with interpolated values — always use argument arrays
- Do not silently swallow template rendering errors — fail the dispatch for that task
- Do not hardcode model names or API provider names in Rust source
- Do not add heavy dependencies for minor convenience
- Do not silently weaken security policy or access constraints
- Do not add speculative config/feature flags "just in case"
- Do not modify unrelated modules "while here"

---

## Mental Model

> Beads defines what should be done.
> Thala executes it.
> Humans intervene when needed.

---

## Linked References

- `@docs/contributing/change-playbooks.md` — adding backends, adapters; security/gateway changes; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->
