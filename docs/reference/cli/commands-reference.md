# Thala Commands Reference

This reference is derived from the current CLI surface (`thala --help`).

Last verified: **February 21, 2026**.

## Top-Level Commands

| Command | Purpose |
|---|---|
| `onboard` | Initialize workspace/config quickly or interactively |
| `agent` | Run interactive chat or single-message mode |
| `gateway` | Start webhook and WhatsApp HTTP gateway |
| `daemon` | Start supervised runtime (gateway + channels + optional heartbeat/scheduler) |
| `service` | Manage user-level OS service lifecycle |
| `doctor` | Run diagnostics and freshness checks |
| `status` | Print current configuration and system summary |
| `estop` | Engage/resume emergency stop levels and inspect estop state |
| `cron` | Manage scheduled tasks |
| `models` | Refresh provider model catalogs |
| `providers` | List provider IDs, aliases, and active provider |
| `channel` | Manage channels and channel health checks |
| `integrations` | Inspect integration details |
| `skills` | List/install/remove skills |
| `migrate` | Import from external runtimes (currently OpenClaw) |
| `config` | Export machine-readable config schema |
| `completions` | Generate shell completion scripts to stdout |

## Command Groups

### `onboard`

- `thala onboard`
- `thala onboard --channels-only`
- `thala onboard --force`
- `thala onboard --reinit`
- `thala onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `thala onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `thala onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` safety behavior:

- If `config.toml` already exists, onboarding offers two modes:
  - Full onboarding (overwrite `config.toml`)
  - Provider-only update (update provider/model/API key while preserving existing channels, tunnel, memory, hooks, and other settings)
- In non-interactive environments, existing `config.toml` causes a safe refusal unless `--force` is passed.
- Use `thala onboard --channels-only` when you only need to rotate channel tokens/allowlists.
- Use `thala onboard --reinit` to start fresh. This backs up your existing config directory with a timestamp suffix and creates a new configuration from scratch.

### `agent`

- `thala agent`
- `thala agent -m "Hello"`
- `thala agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`

Tip:

- In interactive chat, you can ask for route changes in natural language (for example “conversation uses kimi, coding uses gpt-5.3-codex”); the assistant can persist this via tool `model_routing_config`.

### `gateway` / `daemon`

- `thala gateway [--host <HOST>] [--port <PORT>]`
- `thala daemon [--host <HOST>] [--port <PORT>]`

### `estop`

- `thala estop` (engage `kill-all`)
- `thala estop --level network-kill`
- `thala estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `thala estop --level tool-freeze --tool shell [--tool browser]`
- `thala estop status`
- `thala estop resume`
- `thala estop resume --network`
- `thala estop resume --domain "*.chase.com"`
- `thala estop resume --tool shell`
- `thala estop resume --otp <123456>`

Notes:

- `estop` commands require `[security.estop].enabled = true`.
- When `[security.estop].require_otp_to_resume = true`, `resume` requires OTP validation.
- OTP prompt appears automatically if `--otp` is omitted.

### `service`

- `thala service install`
- `thala service start`
- `thala service stop`
- `thala service restart`
- `thala service status`
- `thala service uninstall`

### `cron`

- `thala cron list`
- `thala cron add <expr> [--tz <IANA_TZ>] <command>`
- `thala cron add-at <rfc3339_timestamp> <command>`
- `thala cron add-every <every_ms> <command>`
- `thala cron once <delay> <command>`
- `thala cron remove <id>`
- `thala cron pause <id>`
- `thala cron resume <id>`

Notes:

- Mutating schedule/cron actions require `cron.enabled = true`.
- Shell command payloads for schedule creation (`create` / `add` / `once`) are validated by security command policy before job persistence.

### `models`

- `thala models refresh`
- `thala models refresh --provider <ID>`
- `thala models refresh --force`

`models refresh` currently supports live catalog refresh for provider IDs: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `llamacpp`, `sglang`, `vllm`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen`, and `nvidia`.

### `doctor`

- `thala doctor`
- `thala doctor models [--provider <ID>] [--use-cache]`
- `thala doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `thala doctor traces --id <TRACE_ID>`

`doctor traces` reads runtime tool/model diagnostics from `observability.runtime_trace_path`.

### `channel`

- `thala channel list`
- `thala channel start`
- `thala channel doctor`
- `thala channel bind-telegram <IDENTITY>`
- `thala channel add <type> <json>`
- `thala channel remove <name>`

Runtime in-chat commands (Telegram/Discord while channel server is running):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`
- `/new`

Channel runtime also watches `config.toml` and hot-applies updates to:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (for the default provider)
- `reliability.*` provider retry settings

`add/remove` currently route you back to managed setup/manual config paths (not full declarative mutators yet).

### `integrations`

- `thala integrations info <name>`

### `skills`

- `thala skills list`
- `thala skills audit <source_or_name>`
- `thala skills install <source>`
- `thala skills remove <name>`

`<source>` accepts git remotes (`https://...`, `http://...`, `ssh://...`, and `git@host:owner/repo.git`) or a local filesystem path.

`skills install` always runs a built-in static security audit before the skill is accepted. The audit blocks:
- symlinks inside the skill package
- script-like files (`.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`)
- high-risk command snippets (for example pipe-to-shell payloads)
- markdown links that escape the skill root, point to remote markdown, or target script files

Use `skills audit` to manually validate a candidate skill directory (or an installed skill by name) before sharing it.

Skill manifests (`SKILL.toml`) support `prompts` and `[[tools]]`; both are injected into the agent system prompt at runtime, so the model can follow skill instructions without manually reading skill files.

### `migrate`

- `thala migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `thala config schema`

`config schema` prints a JSON Schema (draft 2020-12) for the full `config.toml` contract to stdout.

### `completions`

- `thala completions bash`
- `thala completions fish`
- `thala completions zsh`
- `thala completions powershell`
- `thala completions elvish`

`completions` is stdout-only by design so scripts can be sourced directly without log/warning contamination.

## Validation Tip

To verify docs against your current binary quickly:

```bash
thala --help
thala <command> --help
```
