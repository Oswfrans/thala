# Workflow Directory Layout

GitHub Actions only loads workflow entry files from:

- `.github/workflows/*.yml`
- `.github/workflows/*.yaml`

Subdirectories are not valid locations for workflow entry files.

Repository convention:

1. Keep runnable workflow entry files at `.github/workflows/` root.
2. Keep cross-tooling/local CI scripts under `dev/` or `scripts/ci/` when used outside Actions.

Workflow behavior documentation in this directory:

- `.github/workflows/master-branch-flow.md`
- `.github/workflows/release.yml` publishes GitHub Releases for `v*.*.*` tags; see `docs/contributing/release-process.md`.
