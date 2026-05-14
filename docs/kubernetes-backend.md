# Kubernetes Backend

The Kubernetes backend runs each Thala worker attempt as one Kubernetes Pod.
Thala remains the orchestrator: it reads Beads, renders the prompt, pushes a
per-run branch, creates a worker Pod, observes Pod status/logs, and validates the
result after the worker calls back.

## Execution Model

`execution.backend: kubernetes` maps directly to Pod lifecycle operations:

- `launch` creates a `restartPolicy: Never` Pod in the configured namespace.
- `observe` reads the Pod phase, worker container state, and recent logs.
- `cancel` deletes the Pod.
- `cleanup` is idempotent Pod deletion.

The worker Pod uses the same remote worker contract as the callback-based
backends. The image must contain `worker-entrypoint.sh`, `opencode`, `git`,
`curl`, and `jq`. The repository includes a baseline worker image at
`dev/docker/Dockerfile.worker`.

## WORKFLOW.md

```yaml
execution:
  backend: kubernetes
  workspace_root: /workspace/product
  github_token_env: THALA_GITHUB_TOKEN
  callback_base_url: https://thala.example.com
```

Per-task routing also supports labels:

```text
backend:kubernetes
backend:k8s
```

Remote backends require `callback_base_url` because workers report completion to
`/api/worker/callback`.

For an end-to-end in-cluster starting point, see
`examples/kubernetes/thala-orchestrator.yaml`. It includes the orchestrator
Deployment, service accounts, RBAC, a sample WORKFLOW.md ConfigMap, and worker
Pod environment wiring.

## Environment

Set these on the Thala process:

```bash
export THALA_K8S_WORKER_IMAGE=ghcr.io/acme/thala-worker:2026-05-14
export THALA_K8S_NAMESPACE=thala-workers
export THALA_K8S_SERVICE_ACCOUNT=thala-worker
export THALA_K8S_IMAGE_PULL_POLICY=IfNotPresent
export THALA_K8S_GITHUB_TOKEN_SECRET=github-token:token
export THALA_K8S_SECRET_ENV=OPENROUTER_API_KEY=llm-secrets:openrouter
```

When Thala runs inside Kubernetes, the backend defaults to:

- `https://kubernetes.default.svc`
- the mounted service account token
- the mounted namespace
- the mounted service account CA certificate

For local development against a cluster, set:

```bash
export THALA_K8S_API_SERVER=https://...
export THALA_K8S_TOKEN=...
export THALA_K8S_CA_CERT=/path/to/ca.crt
```

## RBAC

The Thala orchestrator service account needs only Pod access in the worker
namespace:

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: thala-workers
---
apiVersion: v1
kind: ServiceAccount
metadata:
  name: thala-orchestrator
  namespace: thala-workers
---
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: thala-pod-runner
  namespace: thala-workers
rules:
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["create", "get", "delete"]
  - apiGroups: [""]
    resources: ["pods/log"]
    verbs: ["get"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: thala-pod-runner
  namespace: thala-workers
subjects:
  - kind: ServiceAccount
    name: thala-orchestrator
    namespace: thala-workers
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: thala-pod-runner
```

Use a separate worker service account (`THALA_K8S_SERVICE_ACCOUNT`) when worker
Pods need their own identity or network policy.

## Secrets

Prefer Kubernetes Secrets over passing raw tokens through Thala's environment.

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: github-token
  namespace: thala-workers
type: Opaque
stringData:
  token: replace-with-github-token
---
apiVersion: v1
kind: Secret
metadata:
  name: llm-secrets
  namespace: thala-workers
type: Opaque
stringData:
  openrouter: replace-with-openrouter-token
```

If `THALA_K8S_GITHUB_TOKEN_SECRET` is not set, Thala falls back to the
`github_token_env` value for the worker Pod's `GITHUB_TOKEN`.

## Operational Notes

Pod names are deterministic DNS labels:

```text
thala-<product>-<task-id>-<attempt>
```

Long names are truncated with a hash suffix. The Pod is labeled with product,
task, run, and `app.kubernetes.io/name=thala-worker` for log and policy
selection.

The backend treats image pull and container config failures as terminal worker
failures. A running Pod with unchanged logs is still considered alive; Thala's
existing stall detector decides when lack of output requires human attention.
