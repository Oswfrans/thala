# Thala — Opinionated Agent Development Framework

[![CI](https://github.com/oswfrans/thala/actions/workflows/ci-run.yml/badge.svg)](https://github.com/oswfrans/thala/actions/workflows/ci-run.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.92+](https://img.shields.io/badge/rust-1.92%2B-orange.svg)](https://www.rust-lang.org)

This is an alpha version that I made because I found it interesting. Use at your own peril and discretion for now. Short pitch: Claude managed agents, but opensource.

Thala is an opinionated open-source agent development framework for turning Beads tasks into reviewed code changes. It assembles context-rich prompts, spawns OpenCode workers in isolated tmux/git worktrees or remote containers such as OpenCode Zen, Modal, or Cloudflare, monitors those sessions, and handles validation, retries, and human escalation - all driven by a per-product `WORKFLOW.md` config file.

Model routing is config-driven via `WORKFLOW.md`. Beads is the supported tracker.

## Quick Start

Start with the orchestrator path:

```bash
cargo build --release
./target/release/thala onboard
./target/release/thala --workflow /path/to/product/WORKFLOW.md validate
./target/release/thala --workflow /path/to/product/WORKFLOW.md run
```

See [QUICKSTART.md](docs/QUICKSTART.md) for a first-run walkthrough. Use
[THALA_SETUP.md](docs/THALA_SETUP.md) for the full ops setup: Beads, product
`WORKFLOW.md`, worker backends, and escalation.

## Architecture

Two-tier:

- **Tier 1 — Thala (this repo):** Reads tasks from Beads, assembles context-rich prompts, spawns and monitors OpenCode workers, handles validation, retries, and escalation via Discord and/or Slack. State is persisted in SQLite (`~/.local/share/thala/state.db`).
- **Tier 2 — Workers:** OpenCode sessions each in an isolated git worktree or remote container. See only code and their task prompt — never Thala's orchestration context.

### Worker backends

Workers run in one of four backends, configured per-product in `WORKFLOW.md`:

| Backend | When to use |
|---|---|
| `local` (default) | Development, single-machine deployments. Workers run in tmux git worktrees on the same host. |
| `modal` | Serverless cloud workers. Each task gets a fresh container on Modal; no local tmux or OpenCode needed. |
| `cloudflare` | Cloudflare Containers. Suitable for mature workloads already on Cloudflare's platform. |

Remote backends push the task branch to GitHub, run OpenCode in a managed container, and push changes back. Modal reports completion by signed callback; Cloudflare is polled through its Worker/Durable Object control plane.

See [examples/WORKFLOW.md](examples/WORKFLOW.md) for the workflow contract used by this repository.

Core orchestration lives in `src/orchestrator/`. See [AGENTS.md](AGENTS.md) for full architecture docs.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test

# Full CI (runs in Docker)
./dev/ci.sh all

# Debug logging
./target/release/thala --log thala=debug,info --workflow /path/to/WORKFLOW.md run
```

## Acknowledgements

Thala interaction was partly inspired by [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw).
Thala was inspired by all the various frameworks I have read about on X. Gastown, Claude managed agents, Glass etc.

## Contributing

PRs are more than welcome. Got feedback DM me on X [@oswinfrans](https://x.com/oswinfrans)

## License

Thala is licensed under the MIT License. See [LICENSE](LICENSE) for details.
