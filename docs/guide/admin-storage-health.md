# Storage & Health

## Storage {#storage}

### Filesystem

```toml
[storage]
type = "filesystem"
path = "/var/cache/batlehub"
```

### S3-compatible (AWS S3, MinIO, RustFS)

```toml
[storage]
type   = "s3"
bucket = "batlehub-artifacts"
region = "us-east-1"

# For self-hosted S3 (MinIO, RustFS): set a custom endpoint
# endpoint = "http://rustfs:9900"

# Credentials (omit to use IAM role / instance profile on AWS)
# access_key_id     = "AKIAIOSFODNN7EXAMPLE"
# secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
```

### Multi-backend storage

Different registries can use different backends — for example, filesystem for most registries and dedicated S3 for large GitHub release artifacts:

```toml
[storage]
type = "filesystem"
path = "/var/cache/batlehub"

[[storage.backends]]
name = "github-s3"
type = "s3"
bucket = "batlehub-github"
region = "us-east-1"

[[registries]]
type    = "github"
name    = "github"
storage = "github-s3"
```

### S3 with RustFS (self-hosted)

Start RustFS via the bundled Compose file, then create the bucket:

```sh
task compose:s3:db            # start RustFS + Postgres + Authentik
rc alias set local http://localhost:9900 rustfsadmin rustfsadmin
rc bucket create local/artifacts   # or: task compose:s3:bucket:create
task run:s3                   # run the server with the S3 config
```

---

## Health & Observability {#health}

### Health endpoint

```sh
curl -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/health
```

Returns per-registry status (upstream reachability, cache hit rate) and overall server status.

### Clear registry cache

Forces the next request for any package in the registry to re-fetch from upstream:

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/registries/npm/clear-cache
```

### OpenTelemetry (Jaeger, Tempo)

Enable distributed tracing by adding an `[otel]` block:

```toml
[otel]
endpoint = "http://jaeger:4317"
```

Start the full observability stack locally:

```sh
task compose:otel   # starts Postgres + server + Jaeger
```

Then open `http://localhost:16686` for the Jaeger UI.
