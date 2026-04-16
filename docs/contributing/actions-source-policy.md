# Actions Source Policy

This document defines the current GitHub Actions source-control policy for this repository.

## Current Policy

- Repository Actions permissions: enabled
- Allowed actions mode: selected

Selected allowlist (actions used by active workflows):

| Action | Used In | Purpose |
|--------|---------|---------|
| `actions/checkout@v4` | `ci-run.yml` | Repository checkout |
| `actions/setup-node@v4` | `ci-run.yml` | Node setup for docs lint tooling |
| `actions/setup-python@v5` | `ci-run.yml` | Python setup for docs quality scripts |
| `dtolnay/rust-toolchain@stable` | `ci-run.yml` | Install Rust toolchain (1.92.0) |
| `Swatinem/rust-cache@v2` | `ci-run.yml` | Cargo build/dependency caching |

Equivalent allowlist patterns:

- `actions/*`
- `dtolnay/rust-toolchain@*`
- `Swatinem/rust-cache@*`

## Workflows

| Workflow | File | Trigger |
|----------|------|---------|
| CI | `.github/workflows/ci-run.yml` | PRs and pushes to `master` |
| Quality Gate (Legacy Manual) | `.github/workflows/checks-on-pr.yml` | Manual `workflow_dispatch` |

The CI security job also runs `dev/scripts/ci/secret_history_scan.sh` against the
full reachable git history. It uses only local git commands and does not require
an additional third-party action.

## Change Control

Record each policy change with:

- change date/time (UTC)
- actor
- reason
- allowlist delta (added/removed patterns)
- rollback note

Use these commands to export the current effective policy:

```bash
gh api repos/oswfrans/thala/actions/permissions
gh api repos/oswfrans/thala/actions/permissions/selected-actions
```

## Guardrails

- Any PR that adds or changes `uses:` action sources must include an allowlist impact note.
- New third-party actions require explicit maintainer review before allowlisting.
- Expand allowlist only for verified missing actions; avoid broad wildcard exceptions.

## Change Log

- 2026-04-10: Consolidated CI to `ci-run.yml` as single authoritative gate, moved `checks-on-pr.yml` to manual legacy notice, and updated action policy to active workflow set.
