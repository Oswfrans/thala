# Thala Release Process

This runbook captures the currently active repository release automation state.

Last verified: **April 12, 2026**.

## Current State

Active workflow files are:

- `.github/workflows/ci-run.yml`
- `.github/workflows/release.yml`
- `.github/workflows/checks-on-pr.yml` (manual legacy notice)

Release publishing is handled by `.github/workflows/release.yml`. It runs on pushed tags matching `v*.*.*`, creates a draft GitHub Release, uploads platform archives and SHA256 checksum files, then publishes the release after all uploads succeed.

Published binary targets:

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Docker image publishing is not part of the first release automation pass.

## Maintainer Procedure

1. Use CI (`CI Required Gate`) to validate `master` changes.
2. From a clean checkout at `origin/master`, create the tag:

   ```bash
   dev/scripts/release/cut_release_tag.sh vX.Y.Z --push
   ```

3. Watch the `Release` workflow for the pushed tag.
4. Verify the published GitHub Release includes all platform archives and `.sha256` files.
5. If the release workflow fails after creating a draft release, delete or repair the draft before retrying the same tag.

## Workflow Contract

`.github/workflows/release.yml`:

- Trigger: `push` tags matching `v*.*.*`
- Permissions: `contents: write`
- Guardrails:
  - validates release tags against `vX.Y.Z` or `vX.Y.Z-suffix`
  - creates a draft release first
  - publishes only after all matrix builds upload successfully
  - leaves the draft unpublished if any build or upload fails
- Required secrets/variables:
  - none beyond the repository-provided `GITHUB_TOKEN`
- Rollback:
  - unpublish/delete the GitHub Release
  - delete the tag locally and remotely if the tag itself is wrong:

    ```bash
    git tag -d vX.Y.Z
    git push origin :refs/tags/vX.Y.Z
    ```

## Contract Note

If release workflows change, this document must be updated in the same PR to include:

- workflow filenames
- triggers
- publish guardrails
- required secrets/variables
- rollback steps
