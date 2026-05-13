# Changelog

All notable changes to Thala are documented in this file.

This project uses semantic versioning while the public API and operator workflows are still stabilizing. Until `1.0.0`, minor and patch releases may include breaking changes when they are called out here.

## Unreleased

- Added an optional Discord interaction router so one Discord application can route signed interactions to multiple Thala services.
- Documented multi-repo Discord routing in the README, quickstart, and Discord + Modal setup guide.
- Updated the onboarding wizard to offer a `thala-discord-router` systemd service template.

## 0.0.1 - Initial Open-Source Preview

- Established Thala as an opinionated open-source agent development framework.
- Added task orchestration around Notion/Beads, WORKFLOW.md prompt rendering, OpenCode worker execution, validation, retries, and human escalation.
- Added local tmux worker execution plus experimental Modal and Cloudflare worker backends.
- Added GitHub Release automation for tagged binary releases.
- Added baseline open-source governance docs.
