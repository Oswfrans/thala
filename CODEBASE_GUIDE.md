# Thala Codebase Guide

## What Is Thala?

Thala is an **autonomous task orchestration engine** for agent-driven software development. It reads coding tasks from a task tracker (Beads), spawns OpenCode worker sessions across multiple execution backends, monitors their progress, validates output via CI and review AI, and handles retries and escalations—all driven by a per-product `WORKFLOW.md` config file.

**One-line pitch**: turn a tracked task into a reviewed, merged PR with no human in the loop (unless configured otherwise).

---

## Directory Map

```
thala/
├── src/
│   ├── main.rs                   # CLI entrypoint (run, validate, onboard commands)
│   ├── lib.rs                    # Module exports; defines ports/adapters structure
│   ├── core/                     # Pure domain types — no I/O, no side effects
│   │   ├── task.rs               # TaskSpec, TaskRecord, TaskStatus (10-state enum)
│   │   ├── run.rs                # TaskRun, RunStatus (5-state), WorkerHandle
│   │   ├── workflow.rs           # WorkflowConfig (parsed from WORKFLOW.md front-matter)
│   │   ├── events.rs             # OrchestratorEvent enum (inter-subsystem messages)
│   │   ├── transitions.rs        # apply_transition() — state machine guard
│   │   ├── ids.rs                # TaskId, RunId type aliases
│   │   ├── interaction.rs        # InteractionRequest/Ticket/Action types
│   │   ├── error.rs              # ThalaError domain errors
│   │   ├── state.rs              # StateError for illegal transitions
│   │   └── validation.rs        # Validator trait stubs
│   ├── ports/                    # Async traits — abstraction boundaries
│   │   ├── execution.rs          # ExecutionBackend trait
│   │   ├── state_store.rs        # StateStore trait (persist tasks/runs)
│   │   ├── task_source.rs        # TaskSource trait (fetch ready tasks)
│   │   ├── task_sink.rs          # TaskSink trait (write back to tracker)
│   │   ├── repo.rs               # RepoProvider trait (git/GitHub ops)
│   │   ├── backend_router.rs     # BackendRouter trait
│   │   ├── interaction.rs        # InteractionLayer trait (Slack/Discord)
│   │   └── validator.rs          # Validator trait (review AI, CI)
│   ├── adapters/                 # Concrete I/O implementations
│   │   ├── beads/
│   │   │   ├── source.rs         # Runs `bd ready --json` → TaskSpec[]
│   │   │   └── sink.rs           # Runs `bd create/update/close` for write-backs
│   │   ├── execution/
│   │   │   ├── local.rs          # tmux + git worktree on Thala host
│   │   │   ├── modal.rs          # Serverless container via `modal run --detach`
│   │   │   ├── cloudflare.rs     # Sandboxed container via HTTP control-plane API
│   │   │   ├── opencode_zen.rs   # Managed OpenCode session (opencode.ai)
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
│   ├── component/                # Unit tests: core, adapters, ports
│   ├── integration/              # Multi-module tests
│   ├── system/                   # Full end-to-end with mock backends
│   └── live/                     # Real backend tests (opt-in, requires creds)
├── docs/                         # Topic docs, quickstart, ops runbooks
├── examples/                     # Example WORKFLOW.md files
├── dev/
│   ├── setup.sh                  # Installs bd, opencode, tmux, etc.
│   ├── ci.sh                     # Lint + test + build + audit
│   └── docker/                   # CI Docker image
├── AGENTS.md                     # Canonical contributor/agent guidance
├── WORKFLOW.md                   # Primary workflow config for thala-core itself
└── Cargo.toml                    # Rust workspace manifest
```

---

## Architecture: Ports & Adapters

The orchestrator (`src/orchestrator/`) only depends on `src/ports/` traits — never on adapter implementations. This means:

- Backends are swappable without changing orchestration logic
- Tests can use mock adapters
- New integrations (e.g., a Linear task source) require only a new adapter

```
┌─────────────────────────────────────┐
│           OrchestratorEngine        │
│  Scheduler │ Dispatcher │ Monitor   │
│  Validator │ HumanLoop │ Reconciler │
└────────────────┬────────────────────┘
                 │ depends only on traits
        ┌────────▼────────┐
        │     PORTS        │
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

---

## Core Domain Types

### TaskSpec vs. TaskRecord

| | `TaskSpec` | `TaskRecord` |
|---|---|---|
| Source of truth | Beads | Thala's SQLite |
| Mutability | Read-only (ingest once) | Mutable (Thala owns it) |
| Contains | id, title, acceptance criteria, labels, metadata | attempt count, status, active_run_id, reroute_hint |

### TaskStatus (10 states)

```
Pending → Dispatching → Running → Validating → Succeeded
                    ↘               ↗
                    Stuck → (human) → retried
                    Failed
                    Resolved  (human override)
                    Cancelled
```

### RunStatus (5 states)

```
Launching → Active → Completed
                   → Failed
                   → TimedOut
                   → Cancelled
```

State changes go through `apply_transition()` / `apply_run_transition()` — illegal transitions return `StateError`, they never panic.

### TaskRun

One `TaskRun` per dispatch attempt. Retries always create a **new** `TaskRun` (old ones kept as history). Contains: run_id, backend kind, opaque `WorkerHandle`, timestamps, last observation cursor, callback token hash.

---

## Orchestration Subsystems

### 1. Scheduler (`scheduler.rs`)
- Polls `TaskSource::fetch_ready()` every 30s
- Deduplicates against in-flight tasks
- Checks `max_concurrent_runs` headroom
- Emits `DispatchReady` event per eligible task

### 2. Dispatcher (`dispatcher.rs`)
- Consumes `DispatchReady`
- Loads/creates `TaskRecord`, increments attempt
- Routes task to backend (respects `reroute_hint` from human actions)
- Renders WORKFLOW.md body via Tera templates → prompt
- For remote backends: pushes `task/<id>` branch to GitHub first
- Calls `backend.launch(LaunchRequest)` → `WorkerHandle`
- Persists new `TaskRun` to SQLite
- Emits `RunLaunched`

### 3. Monitor (`monitor.rs`)
- Polls active runs every 15–60s
- Calls `backend.observe(handle, prev_cursor)` to get activity snapshot
- If cursor changed: updates `last_activity_at`
- If no change for `stall_timeout_ms` (default 5 min): transitions to `TimedOut`, alerts Discord
- If completion signal detected: emits `RunCompleted`

### 4. Validator (`validator.rs`)
- Consumes `RunCompleted`
- Runs review AI (if enabled in WORKFLOW.md)
- If review passes: creates GitHub PR
- Polls CI checks; on pass + `auto_merge: true` → merges PR
- If review fails: injects feedback into next attempt, checks retry budget, re-dispatches
- Routes PRs needing human approval to `InteractionLayer`

### 5. HumanLoop (`human_loop.rs`)
- Manages `InteractionTicket`s (approvals, escalations)
- Listens on Slack/Discord for responses
- Applies decisions: `merge`, `retry`, `reroute`, `resolve`, `escalate`

### 6. Reconciler (`reconciler.rs`)
- Runs once at startup
- Recovers `Launching`/`Running` tasks left by prior crash
- Re-queries backends; resets valid ones to `Active`, cancels dead ones

### 7. CallbackServer (`callback_server.rs`)
- Axum HTTP server
- Remote workers POST completion here with bearer token
- Validates token hash against `TaskRun.callback_token_hash`
- Emits `RunCompleted` on success

---

## Execution Backends

| Backend | How It Runs | Job Handle | Completion Detection |
|---|---|---|---|
| **Local** | `tmux new-session` + `git worktree` | tmux session name | Signal file `.thala/signals/<id>.signal` |
| **Modal** | `modal run --detach` | App/call ID | HTTP callback to CallbackServer |
| **Cloudflare** | HTTP POST to control-plane Worker | Remote run ID | Polling — control-plane reports status |
| **OpenCode Zen** | REST API call to opencode.ai | Session ID | HTTP callback to CallbackServer |

All backends implement the same `ExecutionBackend` trait. The orchestrator never inspects handles directly.

---

## Stall Detection

The Monitor tracks an opaque `last_observation_cursor` per run:

- **Local**: SHA-256 of `tmux capture-pane` output
- **Remote**: log cursor position or ETag from backend log API

If the cursor hasn't changed for `stall_timeout_ms`, the run is declared stuck — even if the worker is still producing output that isn't changing (e.g., a reasoning loop).

---

## WORKFLOW.md — Config + Prompt Template

Each product has its own `WORKFLOW.md`. The YAML front-matter is the config; the body is a Tera template rendered into the worker prompt at dispatch time.

```yaml
---
product: "my-app"
github_repo: "org/repo"

tracker:
  backend: beads
  beads_workspace_root: /path/to/workspace

execution:
  backend: local           # local | modal | cloudflare | opencode-zen
  workspace_root: /path/to/repo

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
  before_run: ""
  after_run: "npm test"
  before_cleanup: ""

discord:
  bot_token: "Bot ..."
  alerts_channel_id: "..."
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

Unknown Tera variables cause a dispatch-time error (no silent empty substitutions).

---

## Happy-Path Data Flow

```
1. Scheduler ticks
   → bd ready --json → TaskSpec[]
   → headroom check
   → emit DispatchReady

2. Dispatcher
   → create TaskRecord, increment attempt
   → push task/<id> branch (remote backends)
   → render Tera prompt
   → backend.launch() → WorkerHandle
   → persist TaskRun
   → emit RunLaunched

3. Monitor ticks
   → backend.observe() → cursor
   → cursor unchanged 5+ min? → TimedOut → Discord alert
   → signal/callback received? → emit RunCompleted

4. Validator
   → run review AI (optional)
   → create GitHub PR
   → poll CI checks
   → pass + auto_merge? → merge
   → pass + human required? → InteractionRequest → Discord/Slack
   → fail? → inject feedback, retry (up to max_attempts)

5. HumanLoop (if needed)
   → Discord/Slack response → merge/retry/reroute/resolve
```

---

## Retry & Reroute Flow

When a run fails review:
1. Validator injects feedback text into the next run's prompt context
2. Dispatcher creates a **new** `TaskRun` (never mutates old one)
3. Attempt counter increments; capped at `max_attempts`
4. If a human posted "retry on cloudflare", `reroute_hint` is set on `TaskRecord` and honored on next dispatch

---

## Security Rules

- **No shell interpolation**: all subprocess calls use argument arrays — `["bd", "close", task_id]`, never `sh -c "bd close {task_id}"`
- **Token masking**: secrets are `[SECRET REDACTED]` in all logs
- **Per-run callback tokens**: UUID generated per `TaskRun`, only the SHA-256 hash stored; raw token sent only to the worker
- **Discord signature verification**: Ed25519 public key validation on all webhook payloads
- **thala-core auto-escalation**: if the product is `thala-core`, Thala always routes to Discord for human approval — she never auto-merges changes to herself

---

## State Persistence

Everything lives in SQLite (`tasks.db`):

| Table | Contents |
|---|---|
| `task_records` | TaskRecord rows (status, attempt, active_run_id, reroute_hint) |
| `task_runs` | TaskRun rows (status, handle, backend, cursors, timestamps, token hash) |
| `interaction_tickets` | Open human approval requests |

On startup, the Reconciler reads all non-terminal records and reconciles with live backends before the orchestrator begins normal operation.

---

## Running Thala

```bash
# Build
cargo build --release

# Validate a WORKFLOW.md (dry run, no side effects)
./target/release/thala --workflow /path/to/WORKFLOW.md validate

# Run the orchestrator (runs until Ctrl-C)
./target/release/thala --workflow /path/to/WORKFLOW.md run

# Interactive onboarding wizard
./target/release/thala onboard

# Debug logging
./target/release/thala --log thala=debug,info --workflow ... run
```

---

## Tests

```bash
cargo test --test component      # Core state machine, adapters, port wiring
cargo test --test integration    # Multi-module interaction
cargo test --test system         # Full orchestrator with mock backends
cargo test --test live -- --ignored   # Live tests — requires real Beads/GitHub creds

./dev/ci.sh all                  # Full CI: fmt + clippy + tests + audit
```

---

## Environment Variables

| Variable | Purpose |
|---|---|
| `THALA_GITHUB_TOKEN` | GitHub PAT for branch push and PR creation |
| `THALA_CF_BASE_URL` | Cloudflare control-plane Worker URL |
| `THALA_CF_TOKEN` | Bearer token for control-plane auth |
| `OPENCODE_API_KEY` | OpenCode Zen API key |
| `DISCORD_ALERTS_WEBHOOK` | Discord webhook URL for escalation alerts |

---

## Tech Stack

| Layer | Crate/Tool |
|---|---|
| Async runtime | `tokio` (multi-threaded) |
| HTTP client | `reqwest` (rustls) |
| HTTP server | `axum` + `tower` |
| Serialization | `serde` (JSON, YAML, TOML) |
| Template engine | `tera` (Jinja2-style) |
| State storage | `rusqlite` (bundled SQLite) |
| CLI parsing | `clap` |
| Logging | `tracing` |
| Crypto | `sha2`, `hmac`, `ed25519-dalek`, `base64` |
| Cloudflare backend | TypeScript, `@cloudflare/sandbox`, Durable Objects |

---

## Key Invariants to Know

1. `TaskSpec` is immutable once ingested — Thala is never the source of truth for task description.
2. Retries always create a new `TaskRun`; old runs are history.
3. State changes go through `apply_transition()` — no direct field mutation.
4. The orchestrator never imports adapter types directly — only port traits.
5. Tera template variables are strict — unknown vars error at dispatch, not silently empty.
6. WORKFLOW.md is hot-reloaded; a bad parse keeps the last good config and alerts Discord.
