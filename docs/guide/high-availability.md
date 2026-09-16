# High Availability

BatleHub is a **stateless HTTP server** — all durable state lives in PostgreSQL and an object store, not in the process. Running multiple replicas safely requires swapping two single-instance defaults for shared backends: the in-memory cache store and the local filesystem storage.

---

## Architecture overview {#overview}

```
                 ┌───────────────┐
                 │  Load balancer │
                 └──────┬────────┘
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
   ┌────────────┐ ┌────────────┐ ┌────────────┐
   │ BatleHub 1 │ │ BatleHub 2 │ │ BatleHub 3 │
   └─────┬──────┘ └─────┬──────┘ └─────┬──────┘
         │               │               │
         └───────────────┼───────────────┘
                         │
           ┌─────────────┼─────────────┐
           ▼             ▼             ▼
     ┌──────────┐  ┌──────────┐  ┌──────────┐
     │PostgreSQL│  │  Redis   │  │    S3    │
     │(primary) │  │ (cache)  │  │(storage) │
     └──────────┘  └──────────┘  └──────────┘
```

All state is shared externally. No sticky sessions are needed — any replica can serve any request.

### What changes between single-instance and HA

| Component | Single-instance default | Multi-instance requirement |
|-----------|------------------------|---------------------------|
| Metadata cache | `InMemoryCacheStore` (per-process) | PostgreSQL or Redis (`[cache]`) |
| Rate limiting | `InMemoryRateLimitStore` (per-process) | Same `[cache]` backend — automatic |
| IP blocking | `InMemoryIpBlockStore` (per-process) | Same `[cache]` backend — automatic |
| **Global banner** | `InMemoryBannerStore` (per-process) | Same `[cache]` backend — automatic |
| Artifact storage | Filesystem (`/var/cache/batlehub`) | S3-compatible object store |
| Canonical data | PostgreSQL | PostgreSQL — already shared |

The `[cache]` section controls all four in-memory stores with a single setting. Switching it also fixes rate limiting, IP blocking, and the global admin banner without any additional config.

---

## Prerequisites {#prerequisites}

Before scaling beyond one replica:

- **PostgreSQL 14+** — already required; no change needed.
- **S3-compatible object store** — AWS S3 or RustFS ([MinIO is no longer officially supported](/guide/configuration#_3-4-storage)). Filesystem storage is single-node only.
- **Shared cache backend** — either the same PostgreSQL instance (simplest) or a Redis 7+ instance.
- **Load balancer / ingress** — anything that does round-robin HTTP (nginx, Traefik, Kubernetes Ingress). No session affinity required.

---

## Configuration changes {#config}

These are the only config changes needed to go from single-instance to multi-instance. Everything else stays the same.

### Cache backend {#config-cache}

Replace the default in-memory cache with a shared backend. This single change covers metadata cache, rate limiting, and IP blocking.

**Option A — PostgreSQL** (uses your existing database, no extra service):

```toml
[cache]
type = "postgres"
# url defaults to database.url — omit it unless you want a separate connection string
```

**Option B — Redis** (lower latency, recommended for high request volume):

```toml
[cache]
type  = "redis"
url   = "redis://redis:6379"
```

### Artifact storage {#config-storage}

Switch from filesystem to S3. All replicas read and write to the same bucket.

```toml
[storage]
type   = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"

# For self-hosted S3 (RustFS):
# endpoint         = "http://rustfs:9000"
# force_path_style = true

# Credentials (omit on AWS with an IAM role):
# access_key_id     = "AKIAIOSFODNN7EXAMPLE"
# secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
```

### Database connection pool {#config-db}

Each replica opens its own connection pool. Lower `max_connections` per replica when running behind PgBouncer, or leave it at the default (10) when connecting directly.

```toml
[database]
type            = "postgresql"
url             = "postgresql://batlehub:changeme@postgres:5432/batlehub"
max_connections = 5   # recommended per replica when using a connection pooler
```

### CORS {#config-cors}

Since 1.1.0 an unset `cors_allowed_origins` means **same-origin only**. If BatleHub serves the
SPA itself — the default, and what the Helm chart does — you need nothing here: same-origin
requests never consult CORS.

Set it when a browser client is served from a *different* origin than the API:

```toml
[server]
host                 = "0.0.0.0"
port                 = 8080
cors_allowed_origins = ["https://batlehub.example.com"]
```

::: warning Breaking change in 1.1.0
An empty or absent list previously allowed **every** origin. Setting
`cors_allowed_origins = ["*"]` restores that behaviour and raises a `cors.any-origin` config
warning.
:::

### Complete multi-instance config example

```toml
[server]
host                 = "0.0.0.0"
port                 = 8080
static_dir           = "/app/ui/dist"
cors_allowed_origins = ["https://batlehub.example.com"]

[database]
type            = "postgresql"
url             = "postgresql://batlehub:changeme@postgres:5432/batlehub"
max_connections = 5

[cache]
type = "redis"
url  = "redis://redis:6379"

[storage]
type   = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"

[[auth]]
type = "token"

[[auth.tokens]]
value   = "change-me-admin-token"
role    = "admin"
user_id = "admin"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
user      = ["releases:read", "source:read"]
admin     = ["*"]
```

---

## Docker Compose — single-host redundancy {#compose}

Docker Compose can run multiple server replicas on a single host. This protects against process crashes but not host failure. Use it for staging environments or when you want process-level redundancy without a full Kubernetes cluster.

```yaml
# docker-compose.ha.yml
services:
  postgres:
    image: postgres:17-alpine
    environment:
      POSTGRES_DB:       batlehub
      POSTGRES_USER:     batlehub
      POSTGRES_PASSWORD: changeme
    volumes:
      - postgres_data:/var/lib/postgresql/data

  redis:
    image: redis:7-alpine
    command: redis-server --save "" --appendonly no

  batlehub:
    image: ghcr.io/batleforc/batlehub:1.1.0
    deploy:
      replicas: 2
      restart_policy:
        condition: on-failure
    depends_on: [postgres, redis]
    volumes:
      - ./config.toml:/etc/batlehub/config.toml:ro
    # No cache volume needed — storage is S3.

  proxy:
    image: nginx:alpine
    ports:
      - "8080:80"
    volumes:
      - ./nginx.conf:/etc/nginx/nginx.conf:ro
    depends_on: [batlehub]

volumes:
  postgres_data:
```

Minimal `nginx.conf`:

```nginx
events {}
http {
  upstream batlehub {
    server batlehub:8080;   # Docker's internal DNS round-robins across replicas
  }
  server {
    listen 80;
    location / {
      proxy_pass http://batlehub;
      proxy_set_header Host              $host;
      proxy_set_header X-Real-IP         $remote_addr;
      proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
      proxy_set_header X-Forwarded-Proto $scheme;
    }
  }
}
```

::: warning IP blocking behind a proxy
If `[ip_blocking]` is enabled, BatleHub reads the client IP from `X-Real-IP` or `X-Forwarded-For`. Ensure your load balancer sets these headers; otherwise all requests appear to come from the proxy's IP.
:::

---

## Kubernetes / Helm — production HA {#kubernetes}

The bundled Helm chart supports multi-replica deployments out of the box once S3 and a shared cache backend are configured.

### HA values file

```yaml
# ha-values.yaml
replicaCount: 3

image:
  repository: ghcr.io/batleforc/batlehub
  tag: "1.1.0"    # pin to a specific version

database:
  url: "postgresql://batlehub:changeme@postgres-svc:5432/batlehub"

storage:
  type: s3
  s3:
    bucket:          batlehub-artifacts
    region:          us-east-1
    accessKeyId:     "AKIAIOSFODNN7EXAMPLE"
    secretAccessKey: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"

# PVC not needed — all artifacts go to S3.
persistence:
  enabled: false

# Inject the shared cache backend via extraConfig.
extraConfig: |
  [cache]
  type = "redis"
  url  = "redis://redis-svc:6379"

ingress:
  enabled: true
  className: nginx
  host: batlehub.example.com
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
  tls:
    - secretName: batlehub-tls
      hosts:
        - batlehub.example.com

# Spread replicas across nodes.
affinity:
  podAntiAffinity:
    preferredDuringSchedulingIgnoredDuringExecution:
      - weight: 100
        podAffinityTerm:
          topologyKey: kubernetes.io/hostname
          labelSelector:
            matchLabels:
              app.kubernetes.io/name: batlehub

resources:
  requests:
    cpu:    200m
    memory: 256Mi
  limits:
    cpu:    1000m
    memory: 512Mi
```

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub \
  --create-namespace \
  -f ha-values.yaml
```

### Horizontal Pod Autoscaler

Scale replicas automatically based on CPU load:

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: batlehub
  namespace: batlehub
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: batlehub
  minReplicas: 2
  maxReplicas: 10
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 70
```

---

## Rolling updates and zero-downtime deploys {#rolling-updates}

BatleHub applies database migrations automatically on startup. Migrations are designed to be additive — they never drop columns or tables that the previous version still reads. This means a rolling deploy is safe:

1. New pods start, run migrations, and become ready.
2. Old pods continue serving requests while new ones apply migrations.
3. Old pods are terminated once new ones pass their readiness probe.

Configure the Deployment strategy to guarantee zero downtime:

```yaml
# In ha-values.yaml, under the helm chart's extraConfig or via kubectl patch:
strategy:
  type: RollingUpdate
  rollingUpdate:
    maxUnavailable: 0
    maxSurge: 1
```

To apply this to the chart's Deployment directly (the chart does not expose this as a top-level value), patch it after installation:

```sh
kubectl patch deployment batlehub \
  -n batlehub \
  --type=json \
  -p='[{"op":"add","path":"/spec/strategy","value":{"type":"RollingUpdate","rollingUpdate":{"maxUnavailable":0,"maxSurge":1}}}]'
```

---

## Health probes {#health}

The Helm chart configures liveness and readiness probes automatically:

| Probe | Endpoint | Initial delay | Period |
|-------|----------|--------------|--------|
| Readiness | `GET /api/v1/admin/health` | 5 s | 10 s |
| Liveness | `GET /api/v1/admin/health` | 10 s | 30 s |

The health endpoint does **not** require an `Authorization` header. Kubernetes can reach it directly from the kubelet.

Traffic is only routed to a pod once its readiness probe passes — so clients are never sent to a replica that is still applying migrations or warming its cache.

---

## Observability {#observability}

Distributed tracing works across replicas without any extra configuration. Each span carries the same trace ID regardless of which replica handles a request. Point all replicas at the same OpenTelemetry collector:

```toml
[otel]
endpoint     = "http://otel-collector:4317"
service_name = "batlehub"
```

In Helm:

```yaml
otel:
  enabled:  true
  endpoint: "http://otel-collector:4317"
```

Each replica emits its own spans; the collector stitches them into complete traces by trace ID. See the [Administration guide](/guide/admin-storage-health#health) for the Jaeger quickstart.

---

## Known limitations {#limitations}

These are accepted trade-offs documented in `docs/contributing/contributing.md §9`:

- **Quota TOCTOU race** — publish quota enforcement reads the current usage and then increments it in two separate DB operations. Under concurrent publishes across replicas the quota can be exceeded by at most one upload per concurrent writer. Enforcement is eventually consistent, not strict.

- **Cache warm-up duplicates** — each replica runs its own warm-up pass on startup. Multiple replicas starting simultaneously will each independently fetch the same upstream packages. The downloads are idempotent (last writer wins in S3) but generate duplicate upstream traffic.

- **Async quota rollback** — if a publish fails after storage but before the DB commit, the quota counter is decremented asynchronously. A short window exists where the counter is overcounted.

- **In-memory stores if misconfigured** — if `[cache]` is left at the default `type = "memory"`, each replica maintains its own independent rate-limit and IP-block state. Rate limits will be N times more permissive than configured (where N is the replica count), and IP blocks set on one replica will not propagate to others. **Always set a shared cache backend for multi-replica deployments.**
