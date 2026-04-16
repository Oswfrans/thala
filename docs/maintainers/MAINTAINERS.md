# Maintainers

Thala is maintained by the repository owner and trusted contributors.

## Current Maintainers

- Repository owner: [@oswfrans](https://github.com/oswfrans) — security reviews, releases, governance
- See [CODEOWNERS](.github/CODEOWNERS) for path-specific review assignments.

## Responsibilities

- Review changes to security-sensitive areas, including `src/security/`, `src/runtime/`, `src/gateway/`, `src/tools/`, `src/orchestrator/`, and `.github/workflows/`.
- Keep release automation, security policy, and contributor documentation accurate.
- Triage issues and mark experimental features clearly.
- Ensure published releases include source, licenses, notices, binary artifacts, and checksums.

## Release Authority

Only maintainers should push release tags matching `v*.*.*`. Release tags should be created from a clean checkout at `origin/master` using:

```bash
dev/scripts/release/cut_release_tag.sh vX.Y.Z --push
```

## Security Contact

Report vulnerabilities through GitHub private vulnerability reporting once the public repository is available. Do not open public issues for suspected vulnerabilities.
