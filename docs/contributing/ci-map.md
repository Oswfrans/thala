# CI Workflow Map

This document explains what each active GitHub workflow does, when it runs, and whether it blocks merges.

For event-by-event behavior, see [`.github/workflows/master-branch-flow.md`](../../.github/workflows/master-branch-flow.md).

## Merge-Blocking vs Optional

### Merge-Blocking

- `.github/workflows/ci-run.yml` (`CI`)
  - Purpose: authoritative CI gate for code quality and safety.
  - Trigger: `pull_request` to `master`, `push` to `master`.
  - Includes:
    - change-scope detection (`dev/scripts/ci/detect_change_scope.sh`)
    - Rust lint (`cargo fmt --check`, `cargo clippy -D clippy::correctness`)
    - strict delta lint on changed Rust lines (`dev/scripts/ci/rust_strict_delta_gate.sh`)
    - tests (`cargo test --locked`)
    - build matrix (`linux`, `macOS arm64`, `windows`)
    - all-features check (`cargo check --all-features --locked`)
    - security (`cargo audit`, `cargo deny check licenses sources`)
    - 32-bit compatibility check (`i686-unknown-linux-gnu`, `--no-default-features`)
    - docs quality (`dev/scripts/ci/docs_quality_gate.sh`)
    - docs links (`dev/scripts/ci/docs_links_gate.sh`)
  - Merge gate: job `CI Required Gate`.
  - Behavior: docs-only PRs skip expensive Rust jobs; docs jobs run only when docs files changed.

### Optional / Manual

- `.github/workflows/checks-on-pr.yml` (`Quality Gate (Legacy Manual)`)
  - Trigger: `workflow_dispatch`
  - Purpose: compatibility placeholder that points maintainers to `ci-run.yml` as the source of truth.

## Trigger Map

- `CI`: push to `master`, PRs to `master`
- `Quality Gate (Legacy Manual)`: manual dispatch only

## Fast Triage Guide

1. `CI Required Gate` failing: inspect failed jobs in `.github/workflows/ci-run.yml`.
2. Rust lint failures: inspect `lint` and `lint-strict-delta` jobs.
3. Build/test failures: inspect `test`, `build`, and `check-all-features` jobs.
4. Security failures: inspect `security` job and `deny.toml` policy.
5. Docs failures: inspect `docs-quality` and `docs-links` jobs.

## Maintenance Rules

- Keep required checks deterministic (`--locked` where applicable).
- Keep merge-gating logic centralized in `ci-run.yml`.
- Keep local and CI quality policies aligned across:
  - `.github/workflows/ci-run.yml`
  - `dev/ci.sh`
  - `.githooks/pre-push`
  - `dev/scripts/ci/*.sh`
- Keep action usage and allowlist policy aligned with `docs/contributing/actions-source-policy.md`.
