# Helm chart

Deploy BatleHub on Kubernetes using the bundled Helm chart. Three complete
values files follow — [one replica with neither Redis nor S3](#helm-basic),
[S3 with several replicas](#helm-s3), and a
[production-ready one](#helm-prod) — each a whole `my-values.yaml` rather than
a fragment, and each one a file in the repository under `deploy/helm/`.
[Which setup](#helm-setups) is the one-table comparison, and
[Ready-made values files](#helm-examples) lists the four other examples —
external secrets, every registry kind, and a permissive and a restrictive
configuration.

**Prerequisites:** Helm 3+, a running Kubernetes cluster, PostgreSQL accessible from the cluster.

## Quick install

```sh
# Clone the repo (chart is bundled in helm/batlehub/)
git clone https://github.com/batlehub/batlehub
cd batlehub

helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --create-namespace \
  --set config.database.url="postgresql://batlehub:changeme@postgres-svc:5432/batlehub" \
  --set "config.auth[0].type=token" \
  --set "config.auth[0].tokens[0].value=my-admin-token" \
  --set "config.auth[0].tokens[0].role=admin" \
  --set "config.auth[0].tokens[0].user_id=admin"
```

::: warning
Every key lives under `config`, which is the object serialised verbatim to
`config.toml`. Helm accepts a `--set` for a key the chart does not have without
complaining, so a mistyped path here installs quietly with the defaults — the
placeholder database and the `change-me-admin-token` admin token. Check what you
are about to install with `helm template` before `helm install`.
:::

Past a first look, install from a values file rather than a wall of `--set`.
The three below are complete: each one is a whole `my-values.yaml`, installed
with

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub --create-namespace -f my-values.yaml
```

## Which setup {#helm-setups}

The three differ in one decision each — where artifacts live, and whether more
than one replica may serve them. Everything else follows from that.

| | [Basic](#helm-basic) | [S3](#helm-s3) | [Production](#helm-prod) |
|---|---|---|---|
| Artifacts | PVC (`ReadWriteOnce`) | S3 bucket | S3 bucket |
| Replicas | 1 | 2+ | 2+, anti-affinity, PDB |
| Metadata cache | in-process | PostgreSQL | Redis |
| Extra services | PostgreSQL | PostgreSQL, S3 | PostgreSQL, S3, Redis |
| Secrets | in the values | in the values | own Secret, own lifecycle |
| Identities | one static token | one static token | OIDC + CI tokens |
| Scanning | in the proxy pod | in the proxy pod | own worker Deployment |
| Good for | a team, a lab, a first install | shared cache, rolling updates | an instance other people depend on |

A replica count above 1 is the line that forces the rest: `ReadWriteOnce`
cannot be shared across nodes, and an in-process metadata cache is not shared
at all, so a second replica means S3 and a shared cache backend. Below that
line the basic setup is not a lesser version of the others — it is the whole
product on one pod.

## Setup 1 — one replica, no Redis, no S3 {#helm-basic}

PostgreSQL is the only thing you must bring. Artifacts go to a PVC, metadata
is cached in the process, and no `[cache]` block is written at all — absent
means in-memory.

The file is `deploy/helm/values-minimal.yaml`:

```yaml
# deploy/helm/values-minimal.yaml
replicaCount: 1

config:
  database:
    type: "postgresql"
    url: "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"

  auth:
    - type: "token"
      tokens:
        - value: "my-admin-token"
          role: "admin"
          user_id: "admin"

  registries:
    - type: "npm"
      name: "npm"
      rbac:
        anonymous: ["releases:read", "source:read"]
        user: ["releases:read", "source:read"]
        admin: ["*"]

    - type: "cargo"
      name: "internal"
      mode: "local"
      rbac:
        user: ["source:read"]
        admin: ["*"]

# No [cache] block at all: absent means in-memory, which is correct for one
# replica and wrong for two.

persistence:
  enabled: true
  size: 50Gi

ingress:
  enabled: true
  className: nginx
  host: batlehub.example.com
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com
```

Worth knowing before you scale this one up:

- **`persistence.enabled: false` does not mean "no cache".** With filesystem
  storage the cache becomes an `emptyDir`, capped by
  `persistence.ephemeralSizeLimit` — the container's root filesystem is
  read-only, so it needs a writable volume either way. Every restart starts
  cold.
- **The PodDisruptionBudget is not rendered at one replica**, on purpose: a
  budget over a single pod blocks node drains and buys no availability.
- **Size the PVC for the cache you want, not the one you have.**
  [Capacity planning](/guide/capacity-planning) turns a package count into a
  number of gigabytes.

## Setup 2 — S3 artifacts, several replicas {#helm-s3}

Two changes to the above: artifacts move to a bucket every replica can write
to, and the metadata cache moves into PostgreSQL so the replicas agree. No new
service — the database is already there.

The complete file is
`deploy/helm/values-multi-replica.yaml`;
these are the blocks that differ from Setup 1:

```yaml
replicaCount: 2

config:
  database:
    type: "postgresql"
    url: "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"
    # Each replica opens its own pool. Lower this when a pooler is in front.
    max_connections: 5

  storage:
    type: "s3"
    bucket: "batlehub-artifacts"
    region: "us-east-1"
    # prefix: "prod"          # optional, store under a bucket subfolder
    # endpoint_url: "http://rustfs:9000"   # set for RustFS and the like
    # force_path_style: true               # required by most non-AWS stores

  # Shared metadata cache. Also what rate limiting and IP blocking read, so
  # leaving it in-process means each replica counts on its own.
  cache:
    type: "postgres"
    # url defaults to database.url — set it only for a separate connection

persistence:
  enabled: false   # no PVC: every artifact is in the bucket

# S3 credentials, for a cluster with no IAM role to assume.
envFrom:
  - secretRef:
      name: batlehub-s3-credentials   # AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
```

The storage block carries no access key, because there is no such field — and
an `access_key_id` added there is not rejected, it is ignored, which looks
exactly like a bucket policy problem. S3 credentials come from the standard AWS
SDK chain: the pod's IAM role (IRSA, Workload Identity — nothing to configure
here), or `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` supplied through `env`
or `envFrom` as above.

With `replicaCount` above 1 the chart renders the PodDisruptionBudget
(`minAvailable: 1` by default), and a rolling update no longer takes the
registry offline. Spreading those replicas across nodes is
[Setup 3](#helm-prod).

## Setup 3 — production-ready {#helm-prod}

Setup 2 plus the four things that separate an instance you run from one other
people depend on: secrets with their own lifecycle, real identities,
replicas that survive a node, and a blast radius.

The file is `deploy/helm/values-production.yaml`:

```yaml
# deploy/helm/values-production.yaml
replicaCount: 3

resources:
  requests: { cpu: 500m, memory: 512Mi, ephemeral-storage: 256Mi }
  limits:   { memory: 2Gi, ephemeral-storage: 1Gi }

# Keep the replicas off one node — a PDB protects a drain, not a node failure.
affinity:
  podAntiAffinity:
    preferredDuringSchedulingIgnoredDuringExecution:
      - weight: 100
        podAffinityTerm:
          topologyKey: kubernetes.io/hostname
          labelSelector:
            matchLabels:
              app.kubernetes.io/name: batlehub

podDisruptionBudget:
  enabled: true
  minAvailable: 2

config:
  server:
    roles: ["proxy"]          # scanning moves to the worker Deployment below
    # The ingress controller's pod CIDR. Mandatory once host routing is on, and
    # what makes X-Forwarded-For trustworthy for rate limiting and audit.
    trusted_proxies: ["10.42.0.0/16"]

  storage:
    type: "s3"
    bucket: "batlehub-artifacts"
    region: "us-east-1"

  # Redis rather than postgres: the metadata cache is on the hot path of every
  # request, and it is the one read the database should not have to serve.
  cache:
    type: "redis"
    url: "redis://redis:6379"

  auth:
    # People, through your IdP. The secret is a placeholder — see `env` below.
    - type: "oidc"
      issuer_url: "https://sso.example.com/application/o/batlehub/"
      client_id: "batlehub"
      client_secret: "${OIDC_CLIENT_SECRET}"
      redirect_uri: "https://batlehub.example.com/api/v1/auth/oidc/callback"
      scopes: ["openid", "profile", "email", "groups"]
      user_id_claim: "preferred_username"
      role_claim: "groups"
      role_mappings:
        "platform-admins": "admin"
        "developers": "user"
    # CI, through the cluster it runs in — no long-lived token to rotate.
    - type: "kubernetes"

  registries:
    - type: "npm"
      name: "npm"
      rbac:
        anonymous: []          # no anonymous reads
        user: ["releases:read", "source:read"]
        admin: ["*"]

  limits:
    max_artifact_size_bytes: 524288000   # 500 MiB

  ip_blocking:
    enabled: true
    violation_threshold: 10
    violation_window_secs: 300
    ban_duration_secs: 3600

  otel:
    endpoint: "http://otel-collector:4317"
    service_name: "batlehub"

env:
  - name: OIDC_CLIENT_SECRET
    valueFrom:
      secretKeyRef: { name: batlehub-secrets, key: oidc-client-secret }

envFrom:
  - secretRef:
      name: batlehub-s3-credentials

# The database URL and the upstream tokens, in a Secret this chart does not
# render and does not roll the pods for. See below.
credentials:
  enabled: true
  existingSecret: batlehub-credentials   # from ESO, Sealed Secrets, Vault Agent
  key: credentials.toml

persistence:
  enabled: false

ingress:
  enabled: true
  className: nginx
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
  host: batlehub.example.com
  tls:
    - secretName: batlehub-tls
      hosts: ["batlehub.example.com"]

# Only the worker needs egress to the internet. Name the controller, or the
# ingress rule means "any source" — see the note in values.yaml.
networkPolicy:
  enabled: true
  ingressFrom:
    - namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: ingress-nginx

# Scanners hold an artifact until it has a verdict, on their own pods.
worker:
  enabled: true
  runtimeClassName: gvisor       # empty when the node has neither gVisor nor Kata
  resources:
    requests: { cpu: 500m, memory: 1Gi }
    limits:   { memory: 4Gi }
  autoscaling:
    enabled: true                # needs a metrics adapter, see below
    minReplicas: 1
    maxReplicas: 6

trivy:
  enabled: true
```

The four decisions in that file, and where each one is explained in full:

- **Secrets outside the chart.** `credentials` mounts a second config file,
  merged over the first, from a Secret with its own lifecycle — rotating it
  reloads in place instead of rolling the pods, and the console's config editor
  never reads it. [A separate config file for the
  credentials](#helm-credentials); the `${VAR}` form beside it is
  [Injecting secrets via environment variables](#helm-env-vars).
- **Identities, not a shared token.** OIDC for people, `type = "kubernetes"` for
  in-cluster CI, and `anonymous: []` so a read needs one of them.
  [Access control](/guide/access-control).
- **Availability that survives a node.** Anti-affinity spreads the replicas, the
  PDB protects the drain, and both need the shared cache and S3 of
  [Setup 2](#helm-s3). [High availability](/guide/high-availability) is the
  longer version, including the same file for Docker Compose.
- **A blast radius.** A NetworkPolicy, a size limit, IP blocking, and scanners
  in a sandboxed pod of their own. [Scan worker](#helm-worker) below;
  `worker.autoscaling` needs prometheus-adapter or KEDA to expose
  `batlehub_scan_jobs_queued` before the HPA can read it.

Two things this file does **not** do, because the chart does not: it deploys no
PostgreSQL and no Redis (bring your own, or a sub-chart of your own choosing),
and it creates no bucket.

## Ready-made values files {#helm-examples}

The three setups above are files in the repository, and four more cover the
cases they do not. Each is complete and installs on its own:

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub --create-namespace \
  -f deploy/helm/values-minimal.yaml
```

| File | What it is |
|---|---|
| `values-minimal.yaml` | [Setup 1](#helm-basic) — one replica, a PVC, no Redis and no S3 |
| `values-multi-replica.yaml` | [Setup 2](#helm-s3) — S3, a shared cache, three replicas spread across nodes |
| `values-production.yaml` | [Setup 3](#helm-prod) — the above plus OIDC, external credentials, a NetworkPolicy and the scan worker |
| `values-external-secrets.yaml` | Every credential outside the chart: the `credentials` file from an `existingSecret`, `${VAR}` placeholders from `env`, and the two ExternalSecret resources that fill them |
| `values-all-registries.yaml` | All 27 registry kinds, one `[[registries]]` block each — the shape of a full instance, not a recommendation to run 27 |
| `values-open.yaml` | Open bar: anonymous read **and** publish, no gate anywhere. A lab fixture — read its header before it reaches a network you do not own |
| `values-restricted.yaml` | Its opposite, block for block: no anonymous access at all, no static token, four rules between a request and the upstream |

Read the last two as a pair: they differ in the blocks that matter, which is
easier to see side by side than described.

::: tip
`task helm:examples` renders all seven and loads each rendered `config.toml`
through the server — the check that a file still installs, rather than that it
still reads well. It runs on the same footing as the other gates, so an example
here cannot quietly rot into something that fails in your cluster instead.
:::

## Upgrade

```sh
helm upgrade batlehub ./helm/batlehub \
  --namespace batlehub \
  -f my-values.yaml
```

Any change to the values that affects the rendered `config.toml` triggers a Pod rollout, via the `checksum/config` annotation on both Deployments. The `credentials` layer is deliberately outside that: it has no checksum annotation, so rotating it reloads in place instead. See [A separate config file for the credentials](#helm-credentials).

## Scan worker (RFC 0018) {#helm-worker}

The scanners that hold an artifact until it has a verdict need toolchains the
proxy image does not carry, so the chart can run them in a deployment of their
own. Turn the worker on and take the `worker` role off the proxy:

```yaml
config:
  server:
    roles: ["proxy"]   # the proxy stops scanning

worker:
  enabled: true
  replicaCount: 1
  # gVisor or Kata around the whole pod, on top of bubblewrap around each
  # scanner. Leave empty when the node has neither.
  runtimeClassName: ""
  autoscaling:
    enabled: false     # needs a metrics adapter, see below
```

The worker image (`ghcr.io/batleforc/batlehub-worker`) carries every scanner
but GuardDog. Enabling `[scanners.guarddog]` means pointing
`worker.image.repository` at `ghcr.io/batleforc/batlehub-worker-guarddog`
instead, otherwise the scanner is refused when the config loads.

Each scanner runs under bubblewrap, which needs unprivileged user namespaces on
the node. Where the node forbids them, run the pod under a sandboxed runtime
class and set `runtime = "none"` in `[worker.sandbox]`.

`worker.autoscaling` renders a HorizontalPodAutoscaler on the external metric
`batlehub_scan_jobs_queued`, which a metrics adapter such as prometheus-adapter
or KEDA's Prometheus scaler must expose first. `worker.replicaCount` is ignored
while it is on.

Only the worker needs egress to upstream artifacts, the Trivy server and Rekor.
`trivy.enabled` pulls in the Trivy server sub-chart and the endpoint is then
`http://<release>-trivy:4954`.

Which scanners run, on which registries, and what a scanner error does are all
config, not chart values: see
[`[scanners]` and `[worker]`](/guide/configuration#scanners-and-worker).

## Key values reference

The chart's own `README.md` carries the full table, generated from `values.yaml`
and kept in step with it by a drift gate. This is the shortlist.

| Key | Default | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/batleforc/batlehub` | Container image |
| `image.tag` | Chart appVersion | Image tag |
| `replicaCount` | `1` | Pod replicas |
| `config` | see `values.yaml` | The whole application config, serialised verbatim to `config.toml` |
| `config.database.url` | — | PostgreSQL connection string |
| `config.storage.type` | `filesystem` | `filesystem` or `s3` |
| `config.cache.type` | absent (in-process) | Metadata cache backend: `memory`, `postgres` or `redis`. Shared backends are what several replicas need |
| `config.auth` | one static admin token | `[[auth]]` blocks: `token`, `oidc`, `kubernetes`, `actions-oidc` |
| `config.registries` | npm example | `[[registries]]` blocks |
| `credentials.enabled` | `false` | Mount a second config file, merged over `config`, from its own Secret |
| `credentials.existingSecret` | `""` | Use a Secret managed elsewhere instead of one this chart renders |
| `credentials.config` | `{}` | The contents of that second file, same shape as `config` |
| `ingress.enabled` | `false` | Create an Ingress resource |
| `persistence.enabled` | `true` | Create a PVC for cache |
| `persistence.size` | `10Gi` | PVC capacity |
| `externalManifest[].mount.asConfig` | — | Replace the chart-managed config Secret with one of your own |
| `worker.enabled` | `false` | Run the scan worker in its own Deployment |
| `worker.image.repository` | `ghcr.io/batleforc/batlehub-worker` | Worker image; the `-worker-guarddog` variant adds GuardDog |
| `worker.autoscaling.enabled` | `false` | HPA on the queued-jobs metric |
| `worker.runtimeClassName` | `""` | Sandboxed runtime class around the worker pod |
| `trivy.enabled` | `false` | Deploy the Trivy server sub-chart |
| `networkPolicy.enabled` | `false` | Create a NetworkPolicy for the service |

## Injecting secrets via environment variables {#helm-env-vars}

BatleHub's config file supports `${VAR_NAME}` placeholders that are expanded at startup. The Helm chart lets you inject environment variables into the container so those placeholders resolve at runtime — keeping secrets out of the config Secret entirely.

**1. Write `${...}` placeholders in your values:**

```yaml
# my-values.yaml
config:
  auth:
    - type: "oidc"
      issuer_url: "https://sso.example.com/application/o/batlehub/"
      client_id: "batlehub"
      client_secret: "${OIDC_CLIENT_SECRET}"   # resolved at runtime
      redirect_uri: "https://hub.example.com/api/v1/auth/oidc/callback"

  registries:
    - type: "npm"
      name: "internal-npm"
      upstreams:
        - "https://registry.corp.example.com/npm"
      upstream_auth:
        type: "bearer"
        token: "${INTERNAL_NPM_TOKEN}"   # resolved at runtime
```

**2a. Inject each secret individually (`env` with `secretKeyRef`):**

```yaml
# my-values.yaml (continued)
env:
  - name: OIDC_CLIENT_SECRET
    valueFrom:
      secretKeyRef:
        name: batlehub-secrets
        key: oidc-client-secret
  - name: INTERNAL_NPM_TOKEN
    valueFrom:
      secretKeyRef:
        name: batlehub-secrets
        key: npm-token
```

**2b. Or bulk-import all keys from a Secret (`envFrom`):**

```yaml
# my-values.yaml (continued)
envFrom:
  - secretRef:
      name: batlehub-secrets   # all keys in this Secret become env vars
```

**3. Create the Kubernetes Secret separately:**

```yaml
# batlehub-secrets.yaml — managed by Sealed Secrets / ESO / Vault, not Helm
apiVersion: v1
kind: Secret
metadata:
  name: batlehub-secrets
  namespace: batlehub
type: Opaque
stringData:
  oidc-client-secret: "my-actual-secret"
  npm-token: "npat-xxxxxxxxxxxx"
```

```sh
kubectl apply -f batlehub-secrets.yaml
helm install batlehub ./helm/batlehub --namespace batlehub -f my-values.yaml
```

::: tip
If a placeholder references a variable that is not set in the container, BatleHub exits immediately at startup with a clear error message naming the missing variable — preventing silent misconfiguration.
:::

---

## A separate config file for the credentials {#helm-credentials}

The placeholders above put one secret in one field. When whole *sections* belong
to a different lifecycle — the database URL, the `[[auth]]` block, a registry's
upstream token — the second config file is the better fit: the chart mounts it
from its own Secret, and `--config` merges it over the main one.

```yaml
# my-values.yaml
config:
  # Everything that is not a secret, rendered into the chart-managed Secret.
  registries:
    - type: "npm"
      name: "internal-npm"
      upstreams:
        - "https://registry.corp.example.com/npm"

credentials:
  enabled: true
  config:
    database:
      type: "postgresql"
      url: "postgresql://batlehub:the-real-password@postgres:5432/batlehub"
    auth:
      - type: "token"
        tokens:
          - value: "the-real-admin-token"
            role: "admin"
            user_id: "admin"
    # Merged onto the registry declared above, matched by `name` — the upstream
    # list is not restated.
    registries:
      - name: "internal-npm"
        upstream_auth:
          type: "bearer"
          token: "npat-xxxxxxxxxxxx"
```

Three things follow from this that are worth knowing before you rely on it:

- **Rotating the credentials Secret does not roll the pods.** The kubelet
  updates the mounted file in place and the config file watcher picks the change
  up. Nothing in the chart puts a `checksum/` annotation on this Secret, on
  purpose — a rollout is exactly what hot reload is there to avoid.
- **The console's config editor never sees it.** The editor reads and rewrites
  the first layer only, so a file holding credentials is not served to a browser
  and cannot be overwritten from one. What the editor validates and diffs is
  still the merged config.
- **Arrays merge on `name`, not by position.** That is what lets the block above
  add a token to one registry without restating the other twenty. The rules in
  full are in
  [Layered config files](/guide/configuration#layered-config-files).

To keep the credentials out of Helm entirely, point at a Secret something else
manages and leave `credentials.config` empty:

```yaml
credentials:
  enabled: true
  existingSecret: batlehub-credentials   # from ESO, Sealed Secrets, Vault Agent
  key: credentials.toml
```

The chart then renders no Secret of its own and mounts that one. Its `key` must
hold a TOML document — the same shape as `config`, and only the parts you want
kept separate.

---

## Using an external secret (GitOps / Sealed Secrets)

If you manage secrets externally (Sealed Secrets, External Secrets Operator, Vault), create the Secret yourself:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: batlehub-config
  namespace: batlehub
type: Opaque
stringData:
  config.toml: |
    [server]
    host = "0.0.0.0"
    port = 8080

    [database]
    type = "postgresql"
    url  = "postgresql://..."

    [[auth]]
    type = "token"

    [[auth.tokens]]
    value = "my-token"
    role  = "admin"
    user_id = "admin"

    [[registries]]
    type = "npm"
    name = "npm"

    [registries.rbac]
    anonymous = ["releases:read", "source:read"]
```

Then tell the chart to mount it as the config, with an `externalManifest` entry
that carries no `manifest` of its own — nothing is rendered, the Secret is only
referenced:

```yaml
# my-values.yaml
externalManifest:
  - name: batlehub-config
    kind: Secret
    mount:
      asConfig: true
      items:
        - key: config.toml
          path: config.toml
```

```sh
helm install batlehub ./helm/batlehub --namespace batlehub -f my-values.yaml
```

At most one entry may set `asConfig`. A `credentials` layer still works
alongside it, and is merged over whatever that Secret carries.

---

Every method needs a **PostgreSQL 14+** database, and ends the same way:
[First-time setup](/guide/installation#first-time-setup).
