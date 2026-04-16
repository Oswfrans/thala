# Master Branch Delivery Flows

This document explains what runs when code is proposed to `master` and merged.

Use this with:

- [`docs/contributing/ci-map.md`](../../docs/contributing/ci-map.md)
- [`docs/contributing/pr-workflow.md`](../../docs/contributing/pr-workflow.md)

## Branching Model

Thala uses a single default branch: `master`. All contributor PRs target `master` directly.

## Active Workflows

| File | Trigger | Purpose |
| --- | --- | --- |
| `ci-run.yml` | `pull_request` to `master`, `push` to `master` | Authoritative CI gate (lint, strict delta lint, tests, builds, security, docs gates, 32-bit check) |
| `checks-on-pr.yml` | `workflow_dispatch` | Legacy manual notice only |

## Event Summary

| Event | Workflows triggered |
| --- | --- |
| PR opened/updated against `master` | `ci-run.yml` |
| Push to `master` | `ci-run.yml` |
| Manual dispatch | `checks-on-pr.yml` |

## Step-By-Step

### 1) PR -> `master`

1. Contributor opens or updates a PR against `master`.
2. `ci-run.yml` starts and computes change scope (`docs_only`, `docs_changed`, `rust_changed`).
3. Required CI jobs run conditionally by scope:
   - Rust path changes: lint (`fmt` + `clippy::correctness`), strict delta lint, tests, matrix build, all-features check, security audit, 32-bit check.
   - Docs changes: docs quality and docs links gates.
   - Docs-only PRs skip expensive Rust jobs.
4. Composite job `CI Required Gate` passes only when all scheduled upstream jobs pass.
5. Maintainer merges PR after reviews and branch protection checks pass.

### 2) Push to `master`

1. Commit reaches `master`.
2. `ci-run.yml` executes with the same gating logic.
3. No release workflow is automatically triggered from this repository's current workflow set.

## Build Targets Covered By CI

| Target | `ci-run.yml` |
| --- | :---: |
| `x86_64-unknown-linux-gnu` | ✓ |
| `aarch64-apple-darwin` | ✓ |
| `x86_64-pc-windows-msvc` | ✓ |
| `i686-unknown-linux-gnu` (check only, no default features) | ✓ |

## Quick Troubleshooting

1. `CI Required Gate` failing: inspect failed upstream jobs in `.github/workflows/ci-run.yml`.
2. Strict lint failures on changed Rust lines: inspect `lint-strict-delta` output.
3. Docs PR failures: inspect `docs-quality` and `docs-links` jobs.
