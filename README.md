# Thala — Opinionated Agent Development Framework

[![CI](https://github.com/oswfrans/thala/actions/workflows/ci-run.yml/badge.svg)](https://github.com/oswfrans/thala/actions/workflows/ci-run.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.92+](https://img.shields.io/badge/rust-1.92%2B-orange.svg)](https://www.rust-lang.org)

This is an alpha version that I made because I found it interesting. Use at your own peril and discretion for now. Short pitch: Claude managed agents, but opensource.

Thala is an opinionated open-source agent development framework for turning tracked development tasks into reviewed code changes. It reads tasks from Beads by default or Notion optionally, assembles context-rich prompts, spawns OpenCode workers in isolated tmux/git worktrees or remote containers(such as Modal or Cloudflare), monitors those sessions, and handles validation, retries, and human escalation - all driven by a per-product `WORKFLOW.md` config file.

Model routing is config-driven via `WORKFLOW.md.

## Quick Start

Start with the local CLI agent path:

```bash
cargo build --release
./target/release/thala onboard
./target/release/thala agent -m "Hello, Thala."
./target/release/thala status
./target/release/thala doctor
```

For a scriptable setup:

```bash
export OPENCODE_API_KEY="sk-..."
./target/release/thala onboard \
  --provider opencode \
  --api-key "${OPENCODE_API_KEY:?set OPENCODE_API_KEY first}" \
  --memory sqlite
```

See [QUICKSTART.md](docs/QUICKSTART.md) for a first-run walkthrough. Use
[THALA_SETUP.md](docs/THALA_SETUP.md) for the full ops setup: Beads or Notion, product
`WORKFLOW.md`, worker backends, daemon/service installation, and escalation.

The same `thala onboard` wizard supports both paths. Skip orchestrator setup for
a quick local agent; enable orchestrator setup when you want Beads/Notion task
intake, worker dispatch, PR creation, validation, retries, and escalation.

## Architecture

Two-tier:

- **Tier 1 — Thala (this repo):** Reads tasks from Beads or Notion, assembles context-rich prompts, spawns and monitors OpenCode workers, handles validation, retries, and escalation via Discord/Telegram.
- **Tier 2 — Workers:** OpenCode sessions (Kimi K2.5 / Claude Sonnet) each in an isolated git worktree. See only code and their task prompt — never Thala's orchestration context.

### Worker backends

Workers run in one of three backends, configured per-product in `WORKFLOW.md`:

| Backend | When to use |
|---|---|
| `local` (default) | Development, single-machine deployments. Workers run in tmux git worktrees on the same host. |
| `modal` | Serverless cloud workers. Each task gets a fresh container on Modal; no local tmux or OpenCode needed. |
| `cloudflare` | Cloudflare Containers. Suitable for mature workloads already on Cloudflare's platform. |

Remote backends (`modal`, `cloudflare`) push the task branch to GitHub, spawn a container that clones it, runs OpenCode, pushes changes back, and POSTs a signed completion callback to Thala's gateway.

See [examples/WORKFLOW.md](examples/WORKFLOW.md) for the workflow contract used by this repository.

Core orchestration lives in `src/orchestrator/`. See [AGENTS.md](AGENTS.md) for full architecture docs.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test

# Full CI (runs in Docker)
./dev/ci.sh all
```

## Acknowledgements

Thala interaction was partly inspired by [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw).
Thala was inspired by all the various frameworks I have read about on X. Gastown, Claude managed agents, Glass etc.

## Contributing

PRs are more than welcome. Got feedback DM me on X [@oswinfrans](https://x.com/oswinfrans)

## License

Thala is licensed under the MIT License. See [LICENSE](LICENSE) for details.
