---
product: "thala-core"
github_repo: "oswfrans/thala"

tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /workspaces/thala
  beads_ready_status: open

execution:
  backend: local
  workspace_root: /workspaces/thala
  github_token_env: THALA_GITHUB_TOKEN

models:
  worker: "opencode/kimi-k2.5"
  manager: "anthropic/claude-opus-4-6"
  max_review_cycles: 2

hooks:
  after_create: ""
  before_run: "git pull --rebase --autostash origin master"
  after_run: "cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test"
  before_cleanup: ""

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
    - "billing/**"
    - "infra/**"
    - "**/migrations/**"
    - ".github/workflows/**"
  required_checks:
    - "typecheck"
    - "lint"
    - "ci"

stuck:
  auto_resolve_after_ms: 0
---
You are an expert Rust developer working on Thala.

## Task

**ID:** {{ issue.identifier }}
**Title:** {{ issue.title }}
**Attempt:** {{ run.attempt }}

## Acceptance Criteria

{{ issue.acceptance_criteria }}

{% if issue.context %}
## Context

{{ issue.context }}
{% endif %}

Follow AGENTS.md and run the required validation suite before finishing.
Write DONE to `.thala/signals/{{ issue.identifier }}.signal` when complete.
