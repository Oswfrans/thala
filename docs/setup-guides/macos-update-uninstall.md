# macOS Update and Uninstall Guide

This page documents supported update and uninstall procedures for Thala on macOS (OS X).

Last verified: **February 22, 2026**.

## 1) Check current install method

```bash
which thala
thala --version
```

Typical locations:

- Homebrew: `/opt/homebrew/bin/thala` (Apple Silicon) or `/usr/local/bin/thala` (Intel)
- Cargo/bootstrap/manual: `~/.cargo/bin/thala`

If both exist, your shell `PATH` order decides which one runs.

## 2) Update on macOS

### A) Homebrew install

```bash
brew update
brew upgrade thala
thala --version
```

### B) Clone + bootstrap install

From your local repository checkout:

```bash
git pull --ff-only
./install.sh --prefer-prebuilt
thala --version
```

If you want source-only update:

```bash
git pull --ff-only
cargo install --path . --force --locked
thala --version
```

### C) Manual prebuilt binary install

Re-run your download/install flow with the latest release asset, then verify:

```bash
thala --version
```

## 3) Uninstall on macOS

### A) Stop and remove background service first

This prevents the daemon from continuing to run after binary removal.

```bash
thala service stop || true
thala service uninstall || true
```

Service artifacts removed by `service uninstall`:

- `~/Library/LaunchAgents/com.thala.daemon.plist`

### B) Remove the binary by install method

Homebrew:

```bash
brew uninstall thala
```

Cargo/bootstrap/manual (`~/.cargo/bin/thala`):

```bash
cargo uninstall thala || true
rm -f ~/.cargo/bin/thala
```

### C) Optional: remove local runtime data

Only run this if you want a full cleanup of config, auth profiles, logs, and workspace state.

```bash
rm -rf ~/.thala
```

## 4) Verify uninstall completed

```bash
command -v thala || echo "thala binary not found"
pgrep -fl thala || echo "No running thala process"
```

If `pgrep` still finds a process, stop it manually and re-check:

```bash
pkill -f thala
```

## Related docs

- [One-Click Bootstrap](one-click-bootstrap.md)
- [Commands Reference](../reference/cli/commands-reference.md)
- [Troubleshooting](../ops/troubleshooting.md)
