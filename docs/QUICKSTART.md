# Quickstart

This gets Thala running locally as a Beads-backed development-task orchestrator.

## 1. Build Thala

```bash
git clone https://github.com/oswfrans/thala.git
cd thala
cargo build --release
./target/release/thala --help
```

The current binary exposes three commands: `onboard`, `validate`, and `run`.

## 2. Install Local Prerequisites

For the local backend, let Thala install/check the host tools and authenticate where needed:

```bash
bash dev/setup.sh
bd --help
gh auth status
opencode --help
tmux -V
```

`dev/setup.sh` installs `bd` and `opencode` when missing. `bd` is the supported tracker. `gh` is used to open PRs and check CI. `opencode` runs the worker inside a tmux session.

## 3. Initialize Your Product Repo

```bash
cd /path/to/your-app
bd init --quiet
```

Create a dispatchable task with acceptance criteria:

```bash
bd create "Add a GET /hello endpoint" \
  --description 'Acceptance Criteria:
- GET /hello returns {"message":"hello"}
- Existing tests still pass'
```

Tasks without acceptance criteria are skipped.

## 4. Generate WORKFLOW.md

From the Thala repo:

```bash
./target/release/thala onboard
```

Choose the local backend for the simplest path. The wizard can install missing `bd`/`opencode`, initialize `.beads/`, write `WORKFLOW.md`, and print the exact next commands.

Validate it with the global `--workflow` flag before the subcommand:

```bash
./target/release/thala --workflow /path/to/your-app/WORKFLOW.md validate
```

## 5. Run Thala

```bash
./target/release/thala --workflow /path/to/your-app/WORKFLOW.md run
```

Thala will poll Beads, create an isolated git worktree for each ready task, run
OpenCode in tmux with the rendered prompt, wait for `.thala/signals/<task>.signal`,
run `after_run`, open a PR, poll CI, and request or perform merge approval based
on the workflow merge policy.

## 6. Share One Discord App Across Repos

Discord allows one interaction endpoint per application. To run multiple Thala
services from that same Discord app, keep each repo's Thala process separate and
put the router in front:

```bash
# service A
THALA_CALLBACK_BIND=127.0.0.1:8788 \
THALA_DISCORD_BIND=127.0.0.1:8789 \
XDG_DATA_HOME=$HOME/.local/share/thala-main \
./target/release/thala --workflow /path/to/main/WORKFLOW.md run

# service B
THALA_CALLBACK_BIND=127.0.0.1:8790 \
THALA_DISCORD_BIND=127.0.0.1:8791 \
XDG_DATA_HOME=$HOME/.local/share/thala-chiropro \
./target/release/thala --workflow /path/to/chiropro/WORKFLOW.md run

# router
THALA_DISCORD_ROUTER_BIND=127.0.0.1:8792 \
THALA_ROUTER_MAIN_URL=http://127.0.0.1:8789/api/discord/interaction \
THALA_ROUTER_CHIROPRO_URL=http://127.0.0.1:8791/api/discord/interaction \
python3 dev/infra/discord_router.py
```

Point Discord's interaction URL at your public
`https://YOUR_DOMAIN/api/discord/interaction` route and proxy that route to the
router. Route messages by adding an explicit hint such as `chiropro:` in the
task description. Button interactions route back to the correct service by task
id prefix, for example `chiropro-...`.

Use [THALA_SETUP.md](THALA_SETUP.md) for backend-specific production notes.
