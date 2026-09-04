# Caching

BatleHub sits between your build tools and upstream registries. This page explains exactly how caching works: from the moment a client sends a request, through the cache lookup, to the response — and how to tune every part of that path.

---

## How the cache works

### Request lifecycle

Every request to `/proxy/{registry}/...` passes through this pipeline:

```
Client
  │
  ▼
AuthMiddleware          ← validates bearer token / OIDC / Kubernetes SA
  │  sets Identity (user_id, role, groups)
  ▼
RateLimitMiddleware     ← checks + increments counters in the cache backend
  │  429 if any bucket exhausted (block mode)
  ▼
RBAC check              ← validates registry permissions for the caller's role
  │  403 if permission denied
  ▼
Rules check             ← release age gate, deny_latest, require_signed_release
  │  403 if a rule fires
  ▼
Metadata cache lookup   ← checked in the cache backend (memory / postgres / redis)
  │
  ├─ HIT:  proceed with cached metadata
  │
  └─ MISS: fetch from upstream, store in cache backend with TTL
              │
              ▼
          Artifact storage lookup   ← checked in blob storage (filesystem / S3)
            │
            ├─ HIT:  stream artifact from storage to client
            │
            └─ MISS: fetch from upstream, store in blob storage, stream to client
```

### Two layers of state

BatleHub separates two kinds of cached state with different lifetimes and backends:

| Layer | What is stored | Where | Lifetime |
|-------|---------------|-------|---------|
| **Metadata cache** | Version lists, release info, package metadata | Cache backend (`[cache]`) | `metadata_ttl_secs` (default 5 min), then re-fetched |
| **Artifact storage** | Tarballs, `.crate` files, VSIX packages, Go module zips | Blob storage (`[storage]`) | Permanent by default; controlled by eviction policy |
| **Rate-limit counters** | Per-user / per-group request counts | Cache backend (`[cache]`) | One fixed window (`window_secs`), then auto-reset |

Metadata is intentionally short-lived: version lists change as packages are published upstream. Artifacts are stored permanently because a `.crate` or tarball at a given version never changes.

### What an artifact response says about itself

Every artifact response names where the bytes are kept and what they are
filed under:

| Header | Meaning |
| --- | --- |
| `X-BatleHub-Storage-Key` | the key in blob storage — `artifact:{registry}/{name}/{version}[/{file}]` |
| `X-BatleHub-Package` | the package name, which may contain slashes (`cli/cli`, `@scope/pkg`) |
| `X-BatleHub-Version` | the version, or the commit on a forge archive |

They disclose nothing new: every part is in the URL that was requested. They
exist because the key is a function of the **route** rather than of the
upstream URL, so a client cannot derive it — the same GitHub asset is filed
under `…/{tag}/filename/{file}` when fetched by name and `…/unknown/{id}` when
fetched by id, and a forge archive is filed under the commit its ref resolved
to, not the branch that was asked for.

`batlehub mise export` reads all three
([RFC 0008](/rfc/0008-mise-in-an-air-gapped-estate) §14.1), which is how an
air-gap bundle names keys the disconnected instance will actually look under.

---

## Cache backend — `[cache]` {#cache-backend}

The `[cache]` section selects the storage engine for **metadata** and **rate-limit counters**. Three backends are available:

```toml
# In-process memory (default — no extra infrastructure needed)
[cache]
type = "memory"

# PostgreSQL — persistent, shared across all server replicas
[cache]
type = "postgres"
# Uses the same database URL as [database]; no extra config needed.

# Redis — persistent, shared, TTL-based eviction, lower latency than Postgres
[cache]
type = "redis"
url  = "redis://localhost:6379"
```

### Choosing a backend

| Backend | Persistence | Multi-instance | Extra infra | Best for |
|---------|:-----------:|:--------------:|:-----------:|---------|
| `memory` | No — resets on restart | No — each instance has its own counters | None | Local dev, single-node |
| `postgres` | Yes | Yes — all instances share one DB | PostgreSQL (already required) | Production, multi-replica |
| `redis` | Yes | Yes — all instances share one cluster | Redis | High-throughput production |

::: tip Single-node deployments
`memory` is the default and requires no extra config. Switch to `postgres` or `redis` when running multiple server replicas or when you need rate-limit counters and metadata cache to survive server restarts.
:::

::: warning Redis feature flag
The Redis backend is compiled only when the `cache-redis` Cargo feature is enabled. The official Docker image includes it. When building from source, add `--features cache-redis` to the `cargo build` command.
:::

### What changes between backends

**Metadata cache:** When a client requests a version list and the result is in the cache, the backend is queried (hash map lookup, DB row read, or Redis `GET`). On a miss, the upstream is contacted and the result is stored with a TTL.

**Rate-limit counters:** Each `increment` call atomically bumps a counter keyed by `rl:{registry}:user:{user_id}` (or `rl:{registry}:group:{group}`) and returns the new count plus the window-reset timestamp. With `memory`, this is a `Mutex<HashMap>` operation. With `postgres`, it is an `INSERT … ON CONFLICT DO UPDATE … RETURNING count` that is serialisable under concurrent load. With `redis`, it is an atomic `INCR` with a conditional `EXPIRE` on first write.

---

## Per-registry cache policy — `[registries.cache]` {#registry-cache-policy}

Each registry has its own `[registries.cache]` block that controls:

- How long metadata is considered fresh
- Whether to serve stale metadata when the upstream is down
- When artifacts are evicted from blob storage
- How to pre-fill the cache before the first client request

```toml
[registries.cache]
metadata_ttl_secs = 300       # re-check version lists every 5 min (default)
serve_stale       = true      # serve cached metadata on upstream 5xx (default)

# Artifact TTL (optional) — re-fetch artifacts older than this many seconds
artifact_ttl_secs = 2592000   # delete/re-fetch artifacts older than 30 days

# Additional eviction strategies — all optional, compose with each other
idle_days         = 14        # delete artifacts not accessed for 14 days
max_size_bytes    = 10737418240  # 10 GiB storage cap — evicts LRU when exceeded
keep_latest_n     = 5         # keep only the 5 most recent versions per package

# Warming
warm_packages    = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n    = 3          # warm the 3 most recent versions of bare-name entries
warm_concurrency = 4          # up to 4 parallel downloads
```

### Metadata TTL and `serve_stale`

Metadata (version lists, release info) is cached for `metadata_ttl_secs` seconds. After expiry:

- If the upstream responds: the fresh data is stored and the TTL is reset.
- If the upstream returns 5xx **and** `serve_stale = true` (the default): the stale cached data is returned so clients continue to work during upstream outages.
- If the upstream returns 5xx **and** `serve_stale = false`: the error is propagated to the client as `502 Bad Gateway`.

Set `metadata_ttl_secs = 0` to always re-check upstream on every request (useful in `local` and `hybrid` modes where you want the index to be always fresh).

### Knowing when you're serving stale data

BatleHub tracks a rolling error-rate/latency average per registry and flags it as **degraded**
once upstream calls are failing often or responding slowly — exactly the situation where
`serve_stale` may be quietly serving old data. Check it via:

- `GET /api/v1/admin/stats` — per-registry `upstream_degraded`, `upstream_error_rate`, and `upstream_latency_ms`
- the `batlehub_upstream_health_degraded{registry}` Prometheus gauge (0/1), for alerting
- a `tracing::warn!` log line on the healthy→degraded transition (and an info log on recovery)

### Artifact TTL

By default, once an artifact is downloaded it is kept until an eviction policy removes it. Set `artifact_ttl_secs` to make artifacts expire by age — the next request after expiry re-fetches from upstream and resets the clock:

```toml
[registries.cache]
artifact_ttl_secs = 86400   # re-fetch artifacts older than 24 hours
```

This is useful for registries where packages may be updated in-place (uncommon for public registries, but possible with some private mirrors).

### Eviction policies

Eviction is checked lazily — an artifact is evicted when it would be served and a policy says it has expired. Strategies compose: the first policy to fire triggers the eviction.

::: tip Artifacts without cache metadata
If an artifact was stored before the `artifact_cache_meta` migration was applied (added in a recent release), it has no `cached_at` timestamp. When `artifact_ttl_secs` is configured, such artifacts are conservatively treated as expired and re-fetched from upstream on next access — the correct behavior is to get a fresh copy rather than serve an artifact of unknown age forever.
:::

| Policy | Config key | Effect |
|--------|-----------|--------|
| **Age** | `artifact_ttl_secs` | Remove artifacts older than N seconds |
| **Idle** | `idle_days` | Remove artifacts not accessed for N days |
| **Size cap** | `max_size_bytes` | When storage exceeds the cap, the least-recently-used artifacts are removed until usage is back under the cap |
| **Version count** | `keep_latest_n` | Keep only the N most recently cached versions per package; older versions are removed when a new one is stored |

Omitting all four fields disables eviction — artifacts are kept indefinitely.

To run a sweep on demand, preview one before turning a new size cap loose, or
read what a sweep took out of the audit trail, see
[Running it, and previewing it](/guide/admin-policies#cache-eviction-run).

No eviction strategy considers a blob whose meta row is missing — every strategy
reads that table. Collecting those is
[a separate sweep](/guide/admin-policies#cache-coherence).

---

## Cache warming {#cache-warming}

Cache warming pre-fetches packages so they are available with zero latency before any client requests them. Configure it in `[registries.cache]`:

```toml
[registries.cache]
warm_packages    = ["lodash", "react@18.2.0", "serde"]
warm_latest_n    = 3      # warm the 3 most recent versions of bare-name entries
warm_concurrency = 4      # up to 4 parallel downloads during warming
```

- **Bare name** (`"lodash"`): warms the `warm_latest_n` most recent versions.
- **Pinned version** (`"react@18.2.0"`): warms exactly one version.

Warming runs at startup in the background — the HTTP server is immediately available while warming proceeds. You can also trigger warming on-demand via the admin API:

```sh
# Warm using the registry's configured warm_latest_n
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash"}'

# Warm a specific version
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash@4.17.21"}'
```

::: tip Registry support
Version enumeration (needed to warm bare package names) applies only to the package-based registry types — those addressed by a name plus a version — and excludes the path-addressed ones (Deb, RPM, Pacman, JetBrains IDEs, Generic). It is implemented for every package-based type, so bare names work across all of them; pass a pinned `name@version` entry when you want exactly one version. The name half is whatever coordinate the registry addresses packages by:

| Registry | Bare name | Pinned version |
| --- | --- | --- |
| npm | `lodash` | `lodash@4.17.21` |
| Maven | `com.google.guava:guava` | `com.google.guava:guava@33.0.0-jre` |
| Terraform | `providers/hashicorp/aws` | `providers/hashicorp/aws@5.0.0` |
| RubyGems | `rails` | `rails@7.1.0` |
| Composer | `monolog/monolog` | `monolog/monolog@3.5.0` |

Scoped npm names keep their leading `@` as part of the name (`@scope/pkg`, `@scope/pkg@1.2.3`). For **GitHub**, bare names enumerate releases via the Releases API. For **VS Code Marketplace**, they enumerate all extension versions via the Gallery API. For **Conda**, the version list is synthesised by scanning `repodata.json` across the standard platforms. For **JetBrains Marketplace**, an entry is the plugin `xmlId` and bare names enumerate the **Stable** channel only. The path-addressed types (Deb, RPM, Pacman, JetBrains IDEs, Generic) have no version model at all and pre-warm `warm_paths` instead of `warm_packages`. The two toolchain types (**Node distributions**, **SDKMAN**) are one archive *per platform*, so an entry such as `node@v22.11.0` or `java@21.0.5-tem` warms one file per entry of `warm_platforms` (`linux-x64`, `darwin-arm64`, … for Node; `linuxx64`, `darwinarm64`, … for SDKMAN), defaulting to the platform this server runs on — guessing every platform would fetch 1.6 GB of JDK for a one-line `.sdkmanrc` (RFC 0010 §6.9).
:::

---

## Content-addressable deduplication {#deduplication}

Artifact bytes are stored at a content-addressed key (`blob/{sha256}`) in blob storage. Each logical artifact path (e.g. `artifact:npm/lodash/4.17.21`) holds a reference to the blob rather than the bytes themselves. A reference-count table tracks how many logical keys point to each blob.

This means:
- The same package published to two registries stores only one copy.
- A yanked and re-released version at the same content is stored once.
- Deduplication is transparent — no configuration required.

The deduplication tables (`artifact_dedup_index`, `artifact_dedup_refs`) are created by the database migration and maintained automatically.

---

## Rate limiting and the cache backend {#rate-limiting}

Rate-limit counters are stored in the same backend as the metadata cache. This means:

- With `[cache] type = "memory"`: counters are per-process. Restarting the server or running multiple replicas gives each process its own independent counters.
- With `[cache] type = "postgres"` or `type = "redis"`: counters are shared across all server replicas and survive restarts. A user who hits the limit on one replica cannot bypass it by routing their next request to a different replica.

Configure rate limiting per registry:

```toml
[registries.rate_limit]
requests_per_window = 200    # per authenticated user, or per client IP for anonymous
window_secs         = 60
enforcement         = "block"   # "block" returns 429; "warn" lets request through

# Shared pool for all CI bot group members combined:
[[registries.rate_limit.groups]]
name                = "oidc:ci-bots"
requests_per_window = 5000
window_secs         = 60
```

### Response headers

| Header | Condition | Value |
|--------|-----------|-------|
| `X-RateLimit-Limit` | Every proxied response (when rate limiting is configured) | The most-restrictive limit that applied to this request |
| `Retry-After` | 429 response (block mode) | Seconds until the current window resets |
| `X-RateLimit-Reset` | 429 response (block mode) | Unix timestamp when the current window resets |
| `X-RateLimit-Warning: rate-limit-exceeded` | Over-limit response (warn mode) | Present when the request was allowed despite exceeding the limit |

### Fixed-window semantics

BatleHub uses a **fixed window** counter (not a sliding window or token bucket). Each window is aligned to the Unix epoch:

```
window_start = floor(now_unix / window_secs) * window_secs
```

For example, with `window_secs = 60`, windows run from `:00` to `:59` of each minute, then reset. The `X-RateLimit-Reset` header gives the exact Unix timestamp of the next window boundary.

### Fail-open behaviour

If the cache backend is unavailable when a rate-limit counter needs to be incremented, the request is **allowed** rather than rejected. This prevents the cache backend from becoming a single point of failure for the entire proxy.

When a bucket cannot be incremented due to a store error, BatleHub emits a `WARN` log entry (`rate-limit store unavailable … failing open`) so the outage is visible in your observability tooling even though individual requests are not blocked. Monitor for these warnings and check the health endpoint if you suspect rate limiting is not being enforced.

---

## Worked examples

### Single-node with memory cache (default)

No extra config needed — works out of the box:

```toml
[database]
type = "postgresql"
url  = "postgresql://batlehub:changeme@localhost:5432/batlehub"

# [cache] defaults to type = "memory"

[[registries]]
type = "npm"
name = "npm"

[registries.cache]
metadata_ttl_secs = 300
keep_latest_n     = 10
```

### Multi-replica with PostgreSQL cache

All replicas share the same database, so metadata cache and rate-limit counters are consistent:

```toml
[database]
type = "postgresql"
url  = "postgresql://batlehub:changeme@db:5432/batlehub"

[cache]
type = "postgres"

[[registries]]
type = "npm"
name = "npm"

[registries.cache]
metadata_ttl_secs = 60
serve_stale       = true

[registries.rate_limit]
requests_per_window = 1000
window_secs         = 60
enforcement         = "block"
```

### High-throughput with Redis cache

Redis provides lower per-operation latency than PostgreSQL for hot paths:

```toml
[cache]
type = "redis"
url  = "redis://redis:6379"

[[registries]]
type = "npm"
name = "npm"

[registries.cache]
metadata_ttl_secs = 120

[registries.rate_limit]
requests_per_window = 5000
window_secs         = 60
enforcement         = "block"

[[registries.rate_limit.groups]]
name                = "oidc:ci-bots"
requests_per_window = 50000
window_secs         = 60
```

### Aggressive eviction (space-constrained)

```toml
[registries.cache]
metadata_ttl_secs = 600
artifact_ttl_secs = 604800   # 7 days
idle_days         = 3
max_size_bytes    = 5368709120  # 5 GiB
keep_latest_n     = 3
warm_packages     = ["lodash", "react", "axios"]
warm_latest_n     = 1
```
