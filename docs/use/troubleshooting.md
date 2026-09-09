# Troubleshooting

Common failure modes, their symptoms, and how to fix them.

## DB connection pool exhaustion

**Symptom:** Requests return `503 Service Unavailable`; logs show `connection pool timed out` or `PoolTimedOut`.

**Cause:** `max_connections` in `[database]` is too low for the request concurrency, or a slow query is holding connections.

**Fix:**
1. Check current pool usage: `SELECT count(*) FROM pg_stat_activity WHERE datname = 'batlehub';`
2. Identify slow queries: `SELECT query, wait_event, state FROM pg_stat_activity WHERE state != 'idle' ORDER BY duration DESC LIMIT 20;`
3. Increase `[database] max_connections` in config and trigger a reload: `batlehub-cli admin config reload`.
4. If queries are slow, check for missing indexes on `local_packages(registry, name)` and `access_events(created_at)`.

## S3 credential expiry

**Symptom:** `502 Bad Gateway` when downloading artifacts that should be cached; logs show `InvalidAccessKeyId` or `ExpiredTokenException`.

**Cause:** The AWS/MinIO credentials in `[storage]` have expired (e.g. a temporary STS token, or a rotated key).

**Fix:**
1. Rotate the credentials in your secrets manager.
2. Update `[storage] access_key_id` and `secret_access_key` in the config file.
3. Trigger a hot reload: `batlehub-cli admin config reload`.
   Alternatively, restart the server; the new credentials are picked up at startup.

For temporary STS tokens, consider switching to an IAM instance role or IRSA (no static credentials needed).

## Pending publish orphans

**Symptom:** Publishing a package that was previously partially uploaded returns a unique-constraint error; the package appears in the DB with `status = 'pending'`.

**Cause:** A previous publish attempt crashed after creating the `local_packages` row but before committing it.

**Fix (automatic):** The background `spawn_pending_publish_cleanup` task deletes `pending` rows older than 2 hours, running every hour. Wait for the next sweep.

**Fix (immediate):**
```sql
DELETE FROM local_packages WHERE status = 'pending' AND created_at < now() - interval '2 hours';
```
Or delete the specific row:
```sql
DELETE FROM local_packages WHERE registry = 'my-reg' AND name = 'my-pkg' AND version = '1.0.0' AND status = 'pending';
```

## Artifact cache not evicting (eviction loop stalled)

**Symptom:** Disk/S3 usage grows unboundedly; old artifacts are not being removed.

**Check:**
1. Count rows in `artifact_cache_meta`: `SELECT count(*) FROM artifact_cache_meta;`
2. Check if eviction is configured: `[cache] max_cache_bytes` must be set.
3. Check logs for `eviction` entries — if absent, eviction may not be wired.

**Fix:** Ensure `[cache] max_cache_bytes` is set to a nonzero value. Trigger a manual eviction by temporarily lowering the limit and triggering a reload, then setting it back.

## In-memory cache with multiple replicas (rate limits / quota not enforced globally)

**Symptom:** Users exceed their quota or rate limit on one replica but not another; the `batlehub-cli admin quota` command shows different values per instance.

**Cause:** `[cache] type = "memory"` stores rate-limit counters and quota in process memory, so each replica has an independent view.

**Fix:** Switch to a shared cache backend:
```toml
[cache]
type = "postgres"   # or "redis"
```
Then trigger a config reload. A single Postgres cache has very low overhead for rate-limit counters; Redis is preferred for high-throughput deployments.

BatleHub logs a warning on startup when in-memory cache is used:
> `metadata cache: in-memory — rate-limit, quota, and session state are NOT shared between replicas`

## Cache warm-up storm on restart

**Symptom:** All replicas restart simultaneously (e.g. after a deploy) and bombard upstream registries with cache-miss fetches for seconds to minutes.

**Fix:**
1. **Stagger restarts** in your deployment tool (e.g. rolling update with `maxSurge=1`).
2. **Lower warm concurrency**: `[cache] warm_concurrency = 1` (if supported) to slow the refill rate.
3. **Pre-warm** before cutting traffic:
   ```bash
   batlehub-cli admin cache warm my-registry --packages "react,lodash,typescript"
   ```
4. For path-addressed registries (JetBrains, Debian, RPM), pass `--paths` instead of `--packages`.

## Metadata cache returning stale results

**Symptom:** A package was updated upstream but BatleHub still serves the old metadata.

**Cause:** The TTL in `[cache] ttl_secs` has not elapsed.

**Fix:** Clear the metadata cache for the affected registry:
```bash
batlehub-cli admin cache clear <registry>
```
Or wait for the TTL to expire (default: 300 seconds).

**Also check upstream health:** if the registry is erroring or slow, `serve_stale_metadata` may be
serving the last good cache on purpose rather than a stale TTL. `GET /api/v1/admin/stats` reports
`upstream_degraded`, `upstream_error_rate`, and `upstream_latency_ms` per registry, and a
`batlehub_upstream_health_degraded{registry}` gauge (plus a warning log on the transition) is
available on `/metrics` for alerting.

## Upload failing with `413 Payload Too Large`

**Symptom:** `cargo publish`, `pip upload`, or similar clients receive a 413 error.

**Cause:** The uploaded artifact exceeds `[local_registry] max_artifact_size_bytes`.

**Fix:** Increase the limit for the affected registry:
```toml
[[registries]]
name = "my-cargo"
type = "cargo"
max_artifact_size_bytes = 524288000  # 500 MiB
```

Trigger a config reload: `batlehub-cli admin config reload`.

## API key not recognized

**Symptom:** Clients receive `401 Unauthorized` despite passing a token.

**Check:**
1. Confirm the token exists: `batlehub-cli auth token list`.
2. Check if the token is expired: `batlehub-cli auth whoami`.
3. Verify the `Authorization: Bearer <token>` header is being sent (or `X-NuGet-ApiKey` for NuGet clients — BatleHub normalises this internally).
4. If using OIDC, check OIDC provider logs for the token exchange.

## Vulnerability scan (Trivy / grype) blocks startup

**Symptom:** The `watcher::spawn_periodic_vuln_scan` task fails and logs errors on startup in environments where the scanner binary is not installed.

**Fix:** Either install the scanner (`mise install`), or turn the scan off:
```toml
[vulnerability_scan]
enabled = false
```

Removing the `[vulnerability_scan]` section entirely does the same thing —
`enabled` is `false` when the section is absent, so the periodic scan only ever
runs where an operator asked for it.

## Getting more diagnostic information

```bash
# Increase log verbosity
RUST_LOG=batlehub=debug cargo run -p batlehub-server -- --config config.toml

# Check audit log for recent errors
batlehub-cli admin audit-log --denied-only --per-page 50

# Current stats
batlehub-cli admin stats

# Per-registry health
batlehub-cli admin health
```
