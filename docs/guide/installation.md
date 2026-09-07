# Installation

BatleHub is a single binary backed by PostgreSQL.

**If you have no reason to prefer otherwise, use [Docker Compose](#docker-compose).**
It brings up the server and its database together, needs nothing installed but a
container runtime, and is the shortest path from nothing to a registry you can
point a package manager at. The other three methods below are for when your
environment has already decided for you: a pre-built binary when you do not run
containers, a build from source when you are changing the code, and the
[Helm chart](#helm-chart) when you are deploying to Kubernetes.

---

## Prerequisites

All installation methods require a **PostgreSQL 14+** database. The server creates its schema automatically on first start.

---

## Pre-built releases

Every tagged release publishes ready-to-use artifacts to GitHub:

### Container image (recommended for production)

A multi-arch image (`linux/amd64` + `linux/arm64`) is pushed to the GitHub Container Registry:

```sh
docker pull ghcr.io/batleforc/batlehub:<version>

# Or always pull the latest tagged version (not :latest — pin to a specific version in production)
docker pull ghcr.io/batleforc/batlehub:0.2.0
```

Run it:

```sh
docker run -p 8080:8080 \
  -v /path/to/config.toml:/etc/batlehub/config.toml:ro \
  -v /path/to/cache:/var/cache/batlehub \
  ghcr.io/batleforc/batlehub:<version>
```

### Pre-built binary

A statically linked `batlehub` binary for Linux is attached to each [GitHub Release](https://github.com/batlehub/batlehub/releases). Download it, make it executable, and run:

```sh
curl -L -o batlehub https://github.com/batlehub/batlehub/releases/download/<version>/batlehub
chmod +x batlehub
./batlehub --config config.toml
```

---

## Docker Compose — start here {#docker-compose}

The fastest way to get a running instance for local development or evaluation.

**1. Clone the repository:**

```sh
git clone https://github.com/batlehub/batlehub
cd batlehub
```

**2. Copy and edit the example config:**

```sh
cp config.example.toml config.toml
# Edit config.toml: set database URL, admin token, and at least one registry
```

**3. Start PostgreSQL and the server:**

```sh
podman compose up -d   # or docker compose up -d
```

The server listens on `http://localhost:8080`. The Swagger UI is at `http://localhost:8080/swagger-ui/`.

**4. Verify:**

```sh
curl http://localhost:8080/api/openapi.json
```

### With S3 storage (RustFS)

A separate Compose file adds a RustFS (S3-compatible) storage backend and Authentik OIDC:

```sh
podman compose -f docker-compose.s3.yml up -d postgres rustfs
# Then run the server with the S3 config:
task run:s3
```

---

## Binary from source

**Prerequisites:** Rust 1.87+, Node 24+, PostgreSQL

**1. Build the backend:**

```sh
cargo build --release -p batlehub-server
```

**2. Build the frontend SPA (optional — embeds the UI into the server):**

```sh
cd ui
pnpm install --frozen-lockfile
pnpm run build
cd ..
```

**3. Generate the OpenAPI spec and TypeScript client (required if building the UI):**

```sh
cargo run -p batlehub-server -- --config config.example.toml dump-spec > ui/openapi.json
cd ui && pnpm run generate && pnpm run build && cd ..
```

**4. Create a config file and run:**

```sh
cp config.example.toml config.toml
./target/release/batlehub --config config.toml
```

### Task shortcuts

If you have [Task](https://taskfile.dev) installed:

```sh
task compose:db    # start only PostgreSQL
task run           # cargo run with example config
task ui:dev        # Vite dev server, proxies /api and /proxy to :8080
task dev           # backend + frontend together
task test          # cargo test --workspace
```

---

## Helm chart

Deploy BatleHub on Kubernetes using the bundled Helm chart.

**Prerequisites:** Helm 3+, a running Kubernetes cluster, PostgreSQL accessible from the cluster.

### Quick install

```sh
# Clone the repo (chart is bundled in helm/batlehub/)
git clone https://github.com/batlehub/batlehub
cd batlehub

helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --create-namespace \
  --set database.url="postgresql://batlehub:changeme@postgres-svc:5432/batlehub" \
  --set "auth.tokens[0].value=my-admin-token" \
  --set "auth.tokens[0].role=admin" \
  --set "auth.tokens[0].userId=admin"
```

### Recommended: values file

Create a `my-values.yaml` for a reproducible installation:

```yaml
database:
  url: "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"

auth:
  tokens:
    - value: "my-admin-token"
      role: admin
      userId: admin

registriesRaw: |
  [[registries]]
  type = "npm"
  name = "npm"

  [registries.rbac]
  anonymous = ["releases:read", "source:read"]
  user      = ["releases:read", "source:read"]
  admin     = ["*"]

  [[registries]]
  type = "cargo"
  name = "internal"
  mode = "local"

  [registries.rbac]
  user  = ["source:read"]
  admin = ["*"]

ingress:
  enabled: true
  className: nginx
  host: batlehub.example.com
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com

persistence:
  enabled: true
  size: 50Gi
```

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --create-namespace \
  -f my-values.yaml
```

### Upgrade

```sh
helm upgrade batlehub ./helm/batlehub \
  --namespace batlehub \
  -f my-values.yaml
```

Any change to the values that affects the rendered `config.toml` will automatically trigger a Pod rollout via the `checksum/secret` annotation on the Deployment.

### S3 storage

```yaml
storage:
  type: s3
  s3:
    bucket: batlehub-artifacts
    region: us-east-1
    accessKeyId: "AKIAIOSFODNN7EXAMPLE"
    secretAccessKey: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"

persistence:
  enabled: false   # PVC not needed with S3
```

### Scan worker (RFC 0018) {#helm-worker}

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

### Key values reference

| Key | Default | Description |
|-----|---------|-------------|
| `image.repository` | `ghcr.io/batleforc/batlehub` | Container image |
| `image.tag` | Chart appVersion | Image tag |
| `replicaCount` | `1` | Pod replicas |
| `database.url` | — | PostgreSQL connection string |
| `storage.type` | `filesystem` | `filesystem` or `s3` |
| `auth.tokens` | `[]` | Static token list |
| `auth.oidc` | `[]` | OIDC provider list |
| `registriesRaw` | npm example | Raw TOML `[[registries]]` blocks |
| `ingress.enabled` | `false` | Create an Ingress resource |
| `persistence.enabled` | `true` | Create a PVC for cache |
| `persistence.size` | `10Gi` | PVC capacity |
| `existingSecret` | `""` | Use a pre-existing Secret for config |
| `worker.enabled` | `false` | Run the scan worker in its own Deployment |
| `worker.image.repository` | `ghcr.io/batleforc/batlehub-worker` | Worker image; the `-worker-guarddog` variant adds GuardDog |
| `worker.autoscaling.enabled` | `false` | HPA on the queued-jobs metric |
| `worker.runtimeClassName` | `""` | Sandboxed runtime class around the worker pod |
| `trivy.enabled` | `false` | Deploy the Trivy server sub-chart |
| `networkPolicy.enabled` | `false` | Create a NetworkPolicy for the service |

### Injecting secrets via environment variables {#helm-env-vars}

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

### Using an external secret (GitOps / Sealed Secrets)

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

Then install the chart with `existingSecret`:

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --set existingSecret=batlehub-config
```

---

## First-time setup

Regardless of installation method, once the server is running:

**1. Verify the health endpoint:**

```sh
curl -H "Authorization: Bearer my-admin-token" \
  http://localhost:8080/api/v1/admin/health
```

**2. Open the Web UI and Setup Guide:**

Navigate to `http://localhost:8080` — the Setup Guide page (`/setup`) generates client config snippets for all registered tools.

**3. Point a client at the proxy:**

```sh
# npm
npm install --registry http://localhost:8080/proxy/npm/ some-package

# Go
GOPROXY=http://localhost:8080/proxy/go,direct go get golang.org/x/text@latest

# Cargo — add to .cargo/config.toml
# [source.crates-io]
# replace-with = "batlehub"
# [source.batlehub]
# registry = "sparse+http://localhost:8080/proxy/cargo/registry/"
```
