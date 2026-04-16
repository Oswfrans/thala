# Thala Operations Runbook

This runbook is for operators who maintain availability, security posture, and incident response.

Last verified: **February 18, 2026**.

## Scope

Use this document for day-2 operations:

- starting and supervising runtime
- health checks and diagnostics
- safe rollout and rollback
- incident triage and recovery

For first-time installation, start from [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md).

## Runtime Modes

| Mode | Command | When to use |
|---|---|---|
| Foreground runtime | `thala daemon` | local debugging, short-lived sessions |
| Foreground gateway only | `thala gateway` | webhook endpoint testing |
| User service | `thala service install && thala service start` | persistent operator-managed runtime |
| Docker / Podman | `docker compose up -d` | containerized deployment |

## Docker / Podman Runtime

If you installed via `./install.sh --docker`, the container exits after onboarding. To run
Thala as a long-lived container, use the repository `docker-compose.yml` or start a
container manually against the persisted data directory.

### Recommended: docker-compose

```bash
# Start (detached, auto-restarts on reboot)
docker compose up -d

# Stop
docker compose down

# Restart
docker compose up -d
```

Replace `docker` with `podman` if using Podman.

### Manual container lifecycle

```bash
# Start a new container from the bootstrap image
docker run -d --name thala \
  --restart unless-stopped \
  -v "$PWD/.thala-docker/.thala:/thala-data/.thala" \
  -v "$PWD/.thala-docker/workspace:/thala-data/workspace" \
  -e HOME=/thala-data \
  -e THALA_WORKSPACE=/thala-data/workspace \
  -p 42617:42617 \
  thala-bootstrap:local \
  gateway

# Stop (preserves config and workspace)
docker stop thala

# Restart a stopped container
docker start thala

# View logs
docker logs -f thala

# Health check
docker exec thala thala status
```

For Podman, add `--userns keep-id --user "$(id -u):$(id -g)"` and append `:Z` to volume mounts.

### Key detail: do not re-run install.sh to restart

Re-running `install.sh --docker` rebuilds the image and re-runs onboarding. To simply
restart, use `docker start`, `docker compose up -d`, or `podman start`.

For full setup instructions, see [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md#stopping-and-restarting-a-dockerpodman-container).

## Baseline Operator Checklist

1. Validate configuration:

```bash
thala status
```

2. Verify diagnostics:

```bash
thala doctor
thala channel doctor
```

3. Start runtime:

```bash
thala daemon
```

4. For persistent user session service:

```bash
thala service install
thala service start
thala service status
```

## Health and State Signals

| Signal | Command / File | Expected |
|---|---|---|
| Config validity | `thala doctor` | no critical errors |
| Channel connectivity | `thala channel doctor` | configured channels healthy |
| Runtime summary | `thala status` | expected provider/model/channels |
| Daemon heartbeat/state | `~/.thala/daemon_state.json` | file updates periodically |

## Logs and Diagnostics

### macOS / Windows (service wrapper logs)

- `~/.thala/logs/daemon.stdout.log`
- `~/.thala/logs/daemon.stderr.log`

### Linux (systemd user service)

```bash
journalctl --user -u thala.service -f
```

## Incident Triage Flow (Fast Path)

1. Snapshot system state:

```bash
thala status
thala doctor
thala channel doctor
```

2. Check service state:

```bash
thala service status
```

3. If service is unhealthy, restart cleanly:

```bash
thala service stop
thala service start
```

4. If channels still fail, verify allowlists and credentials in `~/.thala/config.toml`.

5. If gateway is involved, verify bind/auth settings (`[gateway]`) and local reachability.

## Safe Change Procedure

Before applying config changes:

1. backup `~/.thala/config.toml`
2. apply one logical change at a time
3. run `thala doctor`
4. restart daemon/service
5. verify with `status` + `channel doctor`

## Rollback Procedure

If a rollout regresses behavior:

1. restore previous `config.toml`
2. restart runtime (`daemon` or `service`)
3. confirm recovery via `doctor` and channel health checks
4. document incident root cause and mitigation

---

## GCP Service Account Bootstrap (Thala)

Thala authenticates to GCP using a dedicated service account (`thala-agent`) rather than an
interactive user credential. This section documents how that account is provisioned and
how to re-bootstrap it after a key rotation or a fresh VPS deployment.

### Service account details

| Field         | Value                                                        |
|---------------|--------------------------------------------------------------|
| SA email      | `thala-agent@example-project.iam.gserviceaccount.com`          |
| Project       | `example-project`                                            |
| IAM roles     | `roles/logging.viewer`, `roles/cloudsql.client`, `roles/secretmanager.secretAccessor`, `roles/artifactregistry.writer` |
| Key storage   | Secret Manager secret `thala-agent-sa-key` (version `latest`) |
| Key on disk   | `/home/thala/.config/gcloud/thala-key.json` (mode 600)          |

The raw key is **never** checked into source control or logged. It lives only in Secret
Manager and in the write-protected local file above.

### One-time provisioning (already done — for reference)

```bash
PROJECT=example-project
SA=thala-agent

# 1. Create service account
gcloud iam service-accounts create "${SA}" \
    --display-name="Thala Orchestrator Agent" \
    --description="Service account for Thala agent framework — non-interactive gcloud auth on VPS" \
    --project="${PROJECT}"

# 2. Bind required roles
for ROLE in \
    roles/logging.viewer \
    roles/cloudsql.client \
    roles/secretmanager.secretAccessor \
    roles/artifactregistry.writer; do
  gcloud projects add-iam-policy-binding "${PROJECT}" \
      --member="serviceAccount:${SA}@${PROJECT}.iam.gserviceaccount.com" \
      --role="${ROLE}"
done

# 3. Generate a JSON key (output is sensitive — pipe directly to a temp file)
TMP_KEY=$(mktemp)
gcloud iam service-accounts keys create "${TMP_KEY}" \
    --iam-account="${SA}@${PROJECT}.iam.gserviceaccount.com"

# 4. Store in Secret Manager (creates or adds a new version)
gcloud secrets create thala-agent-sa-key \
    --replication-policy=automatic \
    --project="${PROJECT}" \
    --labels=managed-by=thala,purpose=gcloud-auth \
    || true   # idempotent — ignore "already exists"

gcloud secrets versions add thala-agent-sa-key \
    --data-file="${TMP_KEY}" \
    --project="${PROJECT}"

# 5. Delete the local temp file immediately
shred -u "${TMP_KEY}"
```

### Bootstrap on the VPS (run after every fresh deployment or key rotation)

Requires that the operator's current gcloud session (or the machine's metadata-server
credentials) has `roles/secretmanager.secretAccessor` on the project.

```bash
bash /home/thala/scripts/init-gcloud-auth.sh
```

The script:
1. Pulls the latest key version from Secret Manager into `/home/thala/.config/gcloud/thala-key.json` (mode 600).
2. Runs `gcloud auth activate-service-account` with that key file.
3. Appends `GOOGLE_APPLICATION_CREDENTIALS` and `GCP_PROJECT` exports to `/home/thala/.profile` (idempotent).
4. Smoke-tests that the active account matches the expected SA email and exits non-zero on mismatch.

### Key rotation procedure

```bash
PROJECT=example-project
SA=thala-agent

# 1. Generate a new key
TMP_KEY=$(mktemp)
gcloud iam service-accounts keys create "${TMP_KEY}" \
    --iam-account="${SA}@${PROJECT}.iam.gserviceaccount.com"

# 2. Push new version to Secret Manager
gcloud secrets versions add thala-agent-sa-key \
    --data-file="${TMP_KEY}" \
    --project="${PROJECT}"
shred -u "${TMP_KEY}"

# 3. Re-bootstrap on the VPS
bash /home/thala/scripts/init-gcloud-auth.sh

# 4. Disable the old key version in Secret Manager (not strictly required but good hygiene)
# gcloud secrets versions disable <old-version> --secret=thala-agent-sa-key --project="${PROJECT}"
```

### Tailing Cloud Run logs

```bash
# View last 10 minutes of logs for a service
/home/thala/scripts/thala-logs.sh example-app-api 10m

# View last hour
/home/thala/scripts/thala-logs.sh example-app-api 1h

# Override project via env
GCP_PROJECT=my-other-project /home/thala/scripts/thala-logs.sh example-app-api 30m
```

The script resolves `GCP_PROJECT` and `GCP_REGION` from environment variables, falling
back to `/home/thala/.profile`. It maps the shorthand `<since>` argument (e.g. `10m`, `1h`,
`2d`) to the `--freshness` flag accepted by `gcloud logging read`.

### Verifying non-interactive auth

```bash
# Should show thala-agent as ACTIVE, no interactive prompt
gcloud auth list
# Should exit 0 with log lines (or empty if no logs in window)
gcloud logging read 'resource.type="cloud_run_revision"' \
    --project=example-project --freshness=5m --limit=5
```

---

## Related Docs

- [one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)
- [troubleshooting.md](./troubleshooting.md)
- [config-reference.md](../reference/api/config-reference.md)
- [commands-reference.md](../reference/cli/commands-reference.md)
