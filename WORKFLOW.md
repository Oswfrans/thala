---
product: "thala-core"
github_repo: "oswfrans/thala"

tracker:
  backend: beads
  active_states: ["open"]
  terminal_states: ["Done", "Cancelled"]
  beads_workspace_root: /home/debian/thala
  beads_ready_status: open

execution:
  backend: modal
  workspace_root: /home/debian/thala
  github_token_env: THALA_GITHUB_TOKEN
  callback_base_url: https://thala.makotec.xyz

models:
  worker: "openrouter/moonshotai/kimi-k2.5"
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
    - ".github/workflows/**"
  required_checks:
    - "typecheck"
    - "lint"
    - "ci"

discord:
  bot_token: "Bot ${DISCORD_BOT_TOKEN}"
  public_key: "9a953bdcbe2382bd7be71aaf22e4adaae46d7ce5c4bfeb5fb1947dc0b5a05297"
  alerts_channel_id: "1291764435225600060"

hooks:
  after_create: ""
  before_run: "git pull --rebase --autostash origin master"
  after_run: "cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test"
  before_cleanup: ""
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
