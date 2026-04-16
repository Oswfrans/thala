# Contributing to Thala

Thanks for contributing.

## Before You Open A PR

1. Read the architecture and workflow constraints in [AGENTS.md](AGENTS.md).
2. Keep changes scoped to one concern.
3. Prefer minimal patches over broad refactors.

## Development Checks

Run these before opening a PR:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Or run the full Docker CI flow:

```bash
./dev/ci.sh all
```

## PR Requirements

1. Target `master`.
2. Use a conventional commit title.
3. Fill out [.github/pull_request_template.md](.github/pull_request_template.md).
4. Document risks, validations, and rollback plan.
5. Do not include secrets, personal data, or private tokens.

## Security Reports

Do not open public issues for vulnerabilities. Follow [SECURITY.md](SECURITY.md).

## Additional Contributor Docs

See [docs/contributing/README.md](docs/contributing/README.md) for playbooks, reviewer guidance, CI map, and docs contract details.
