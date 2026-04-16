---
tracker:
  backend: beads                           # "beads" | "notion"; Beads is the default
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /workspaces/thala
  beads_ready_status: open
  # Notion-specific (only used when backend=notion):
  # database_id: "notion-db-id-here"

models:
  worker: "kimi-k2.5"                      # OpenCode worker sessions
  manager: "anthropic/claude-opus-4-6"     # Review AI + Discord intake planning
  max_review_cycles: 2

workspace:
  root: /workspaces/thala

hooks:
  after_create: ""
  before_run: "git pull --rebase origin master"
  after_run: "cargo test"
  before_remove: ""

agent:
  max_concurrent_agents: 3
  stall_timeout_ms: 300000
  max_retries: 3
  model_default: "kimi-k2.5"              # Legacy fallback — prefer models.worker above
  model_hard_tasks: "claude-sonnet"

polling:
  interval_ms: 60000

merge_policy:
  auto_merge: true
  protected_paths:
    - "auth/**"
    - "billing/**"
    - "infra/**"
    - "**/migrations/**"
    - ".github/workflows/**"
  required_checks:
    - "typecheck"
    - "lint"
    - "ci"

stuck_cleanup:
  stuck_cleanup_timeout_ms: 86400000       # 24h; 0 = never auto-delete
---

# Thala Workflow Configuration

You are an expert Rust developer working on Thala, an opinionated open-source agent development framework.

## Task Execution Rules

1. Read the full task description from the tracker before starting
2. Follow the AGENTS.md architecture guidelines exactly
3. Use existing patterns in the codebase - read similar implementations first
4. Run the full validation suite before completing: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`
5. Write DONE to `.thala/signals/{{ issue.identifier_slug }}.signal` when complete

## Code Quality Requirements

- All code must pass `cargo fmt` formatting checks
- All code must pass `cargo clippy` with `-D warnings`
- All tests must pass (`cargo test`)
- No new dependencies without explicit approval
- Follow existing naming conventions and module boundaries
- Security-critical code requires human review (see Merge Policy)

## Merge Policy

### Autonomous Merge Criteria

Thala may merge a PR to `master` automatically **only if all** of the following conditions are met:

1. **All CI checks pass:**
   - `cargo fmt --all -- --check` passes (formatting)
   - `cargo clippy --all-targets -- -D warnings` passes (linting)
   - `cargo test` passes (all tests)
   - No merge conflicts with the target branch

2. **No protected paths touched:**
   The changeset must NOT modify any files matching these patterns:
   - `auth/**` - Authentication and authorization systems
   - `billing/**` - Billing and payment processing
   - `infra/**` - Infrastructure and deployment configuration
   - `**/migrations/**` - Database migrations
   - `.github/workflows/**` - CI/CD pipeline definitions

3. **Self-merge exclusion:**
   - Thala **NEVER** auto-merges changes to herself (this repository, `thala-core`)
   - All PRs to the Thala repository require human review and manual merge

### Escalation Triggers

Thala must **escalate to humans via Discord ping** and NOT merge if **any** of the following occur:

1. **CI failure:** Any required check fails (typecheck, lint, or tests)
2. **Merge conflict:** The PR cannot be cleanly rebased on the target branch
3. **Protected path touched:** Any file in the protected paths list is modified
4. **Self-modification:** The PR targets the Thala repository (`thala-core`)

Escalation message includes:
- Task ID and branch name
- Specific failure reason
- List of affected files (if protected path)
- Suggested human action

### Post-Merge Notification

After a successful autonomous merge, Thala posts a summary to the configured Discord operations channel containing:

```
✅ Autonomous Merge Complete
Task: {{ task_id }}
Branch: {{ branch_name }}
Files Changed: {{ count }} ({{ file_list }})
Description: {{ one_line_description }}
Merged by: Thala (autonomous)
Time: {{ timestamp }}
```

### PR Checklist Integration

All PRs must include this checklist in the description:

```markdown
## Protected Paths Checklist

- [ ] I have verified this PR does NOT touch protected paths:
  - [ ] No changes to `auth/**`
  - [ ] No changes to `billing/**`
  - [ ] No changes to `infra/**`
  - [ ] No changes to `**/migrations/**`
  - [ ] No changes to `.github/workflows/**`
- [ ] This is NOT a change to the Thala repository (thala-core)
```

### Protected Paths Reference

| Pattern | Reason for Protection |
|---------|----------------------|
| `auth/**` | Authentication changes affect security posture and user access |
| `billing/**` | Billing changes impact revenue and financial compliance |
| `infra/**` | Infrastructure changes affect deployment and runtime environment |
| `**/migrations/**` | Database migrations are irreversible and can cause data loss |
| `.github/workflows/**` | CI changes affect all future builds and releases |

## Local Development Hooks

To prevent CI failures, install git hooks that run checks before push:

```bash
./dev/scripts/install-hooks.sh
```

This installs a pre-push hook that runs `cargo fmt --check && cargo clippy` to catch issues before they reach CI.

## Signal File Contract

Workers write `DONE` to `.thala/signals/<slugified-task-id>.signal` on completion (e.g., `MKT-42.signal` or `My-Task-123.signal`).
Workers write `FAILED:<reason>` to `.thala/signals/<slugified-task-id>.signal` on failure.

Thala monitors these files every 60 seconds to track task progress.

## Model Routing

- **Worker model** (`models.worker`): `kimi-k2.5` via OpenRouter — all OpenCode execution sessions
- **Manager model** (`models.manager`): `claude-opus-4-6` — Review AI critique + Discord intake planning
- Hard tasks: `claude-sonnet` (complex refactoring, security reviews, multi-file changes)
- Model selection is config-driven via WORKFLOW.md — never hardcoded in source

## Security Reminders

- Never log or return secrets, tokens, or API keys
- All shell commands must use argument arrays, never `sh -c` with interpolation
- `gcloud_secret_get` results are masked: `[SECRET REDACTED]`
- Tracker tokens, Discord webhook, Telegram token all come from env vars only
