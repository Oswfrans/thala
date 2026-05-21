# Goal: Make Pi the Default Worker Runner

Implement Pi as Thala's default worker runner while preserving OpenCode as a supported fallback.

The integration must keep Thala's orchestration boundaries intact: Thala schedules, dispatches, monitors, validates, retries, and escalates. Pi is only the Tier 2 coding worker harness.

Add a worker-runner abstraction below `ExecutionBackend` so local, Modal, Cloudflare, and Kubernetes backends can launch different agent CLIs without hardcoding OpenCode. Pi should become the default runner, with OpenCode selectable for compatibility.

Default Pi execution must include the `autoresearch` and `goal` extensions, and the design must make additional Pi extensions easy to configure, install, and pass into workers.

## Acceptance Criteria

- Add a `WorkerRunner` abstraction that builds the command/env needed to launch a worker from a `LaunchRequest`.
- Support at least two runners:
  - `pi`
  - `opencode`
- Make `pi` the default runner for new workflows and onboarding.
- Keep OpenCode compatibility via explicit config, for example:

  ```yaml
  execution:
    backend: local
    worker_runner: opencode
  ```

- Add Pi runner config with default extensions:

  ```yaml
  worker:
    runner: pi
    extensions:
      - autoresearch
      - goal
  ```

  Use the exact final config shape that best fits the existing `WorkflowConfig`, but do not overload `models.worker`.

- Update local backend to launch Pi through the new runner abstraction instead of directly invoking `opencode`.
- Update remote worker entrypoint/Docker path so Modal and container workers use the configured runner.
- Ensure Pi default launch is non-interactive and receives the rendered Thala prompt.
- Ensure default Pi runs install or enable `autoresearch` and `goal` before execution.
- Add a narrow extension install abstraction that can support more Pi extensions later without modifying backend code.
- Do not allow task/user input to inject shell commands into runner or extension installation.
- Lifecycle hooks from `WORKFLOW.md` remain the only trusted shell snippets.
- Add preflight checks for missing Pi binary and missing/default extensions.
- Update onboarding and docs to show Pi as the default and OpenCode as fallback.
- Add tests for:
  - default workflow config resolves to Pi
  - OpenCode config still resolves correctly
  - Pi command construction includes prompt/model safely
  - default extensions are included
  - additional configured extensions are included
  - missing runner binary produces a clear error
  - extension names are validated/rejected if unsafe
  - remote worker env carries runner and extension config
- Do not introduce MCP, sub-agent frameworks, speculative provider config, or new orchestration concepts.

## Implementation Notes

- Keep `ExecutionBackend` responsible for where the worker runs.
- Put runner-specific command construction in a separate module, likely under `src/adapters/execution/runner.rs` or similar.
- Prefer explicit enums and structs:

  ```rust
  enum WorkerRunnerKind {
      Pi,
      Opencode,
  }

  struct WorkerRunnerConfig {
      kind: WorkerRunnerKind,
      extensions: Vec<String>,
  }
  ```

- `models.worker` should remain the model string. Runner choice should be separate.
- The Pi extension list should default to `["autoresearch", "goal"]`.
- Extension names should be validated as package identifiers, not passed through raw shell.
- For remote workers, pass runner config through env vars or a generated config file, but keep secrets env-only.
- The first implementation can install or enable extensions in the worker image or during startup, but the boundary should allow moving extension installation into Docker build later.

## Non-Goals

- Do not remove OpenCode.
- Do not redesign task lifecycle.
- Do not change Beads integration.
- Do not change validation or merge policy except where worker output contract requires it.
- Do not make Thala depend on Pi extensions for core state transitions yet.
- Do not auto-merge `thala-core`.

## Suggested Commit Title

```text
feat: make pi the default worker runner
```

## Risk

High risk: touches execution backend behavior, worker launch contract, remote worker images and entrypoints, config parsing, and onboarding defaults.

Requires full relevant validation:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Run remote-worker smoke coverage where available.
