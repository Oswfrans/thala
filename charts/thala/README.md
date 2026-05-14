# Thala Helm Chart

This chart installs the Thala orchestrator and configures the Kubernetes
execution backend so coding workers fan out as Pods.

## Install

Create the required secrets first:

```bash
kubectl create namespace thala
kubectl -n thala create secret generic github-token \
  --from-literal=token=replace-with-github-token
kubectl -n thala create secret generic llm-secrets \
  --from-literal=openrouter=replace-with-openrouter-token
```

Install with your image tags and callback URL:

```bash
helm upgrade --install thala ./charts/thala \
  --namespace thala \
  --set image.repository=ghcr.io/acme/thala \
  --set image.tag=2026-05-14 \
  --set worker.image=ghcr.io/acme/thala-worker:2026-05-14 \
  --set workflow.callbackBaseUrl=https://thala.example.com
```

For real deployments, put your complete `WORKFLOW.md` in a values file:

```yaml
workflow:
  content: |
    ---
    product: "example-app"
    github_repo: "example/example-app"
    execution:
      backend: kubernetes
      workspace_root: /workspace/example-app
      github_token_env: THALA_GITHUB_TOKEN
      callback_base_url: https://thala.example.com
    ---
    You are an expert developer on {{ product_name }}.
```

Then install:

```bash
helm upgrade --install thala ./charts/thala --namespace thala -f values.prod.yaml
```

## Important Values

| Value | Purpose |
|---|---|
| `image.repository`, `image.tag` | Thala orchestrator image |
| `worker.image` | Worker image used for per-task Pods |
| `worker.serviceAccountName` | Service account assigned to worker Pods |
| `worker.automountServiceAccountToken` | Defaults false; enable only if workers need Kubernetes API access |
| `githubToken.existingSecret`, `githubToken.key` | Secret used by Thala and worker Pods for GitHub branch push |
| `secretEnv` | Secret-backed env vars forwarded to worker Pods |
| `workflow.content` | Full WORKFLOW.md rendered into a ConfigMap |
| `persistence.existingClaim` | Existing workspace PVC, if not letting the chart create one |

## Upgrade

```bash
helm upgrade thala ./charts/thala --namespace thala -f values.prod.yaml
```

The Deployment uses `Recreate` because Thala owns a local runtime state store
and should run as a single orchestrator instance unless the state layer is made
multi-writer safe.

## Uninstall

```bash
helm uninstall thala --namespace thala
```

Helm leaves PVCs and manually created secrets behind. Delete them explicitly
when you no longer need the workspace state.
