# Package Explorer — Cache & API

## Explorer cache {#cache}

Explorer catalog results are served from an **in-memory cache** to avoid scanning all package tables on every page load. This is important for large registries with tens of thousands of packages.

### How it works {#cache-how}

| Property | Value |
| --- | --- |
| TTL | 10 minutes |
| Scope | Per query (registry filter + name search + sort + page) |
| Invalidation | TTL expiry, admin flush, or successful publish |
| Stale-on-failure | Yes — expired entries are kept and served if the database is unreachable |
| Persistence | In-memory only; cleared on server restart |
| Multi-instance | Each instance has its own cache; there is no cluster-wide broadcast — after bulk data changes, call the flush endpoint on **every replica** (see [Multi-instance deployments](#cache-ha)) |

### Stale-while-unavailable {#cache-stale}

If the database becomes unreachable during a request, BatleHub checks whether a stale (expired) cache entry exists for that exact query:

- **Stale entry exists** → The stale results are returned silently. The response includes `"upstream_unavailable": false` because data is available.
- **No cache entry** → An empty result is returned with `"upstream_unavailable": true`. The UI surfaces a warning badge to indicate that results may be incomplete.

This means the Explorer remains usable during database outages as long as the queries being issued have been cached at least once before the outage.

> The upstream search endpoint (`GET /api/v1/explore/upstream`) is **not cached** — it fans out to live upstream registries and always returns real-time results.

### Automatic invalidation {#cache-auto-invalidate}

The cache is invalidated automatically when:

1. **A package is published** to a local or hybrid registry via `cargo publish`, `npm publish`, etc. Only the entries for that specific registry are cleared.
2. **TTL expires** after 10 minutes.

There is no automatic invalidation when a package is first proxied (i.e. downloaded for the first time through BatleHub). Those entries appear in the Explorer at the next TTL refresh, typically within 10 minutes.

### Manual invalidation {#cache-admin}

Admins can flush the cache from the admin panel or via the API.

#### Admin UI

Navigate to **Admin → Explore Cache** (`/admin/explore-cache`). Two actions are available:

- **Invalidate by Registry** — select a registry from the dropdown and click **Invalidate Registry**. Only cache entries that include that registry are cleared.
- **Invalidate All** — flushes the entire cache. All registries are affected.

After invalidation the next request for any flushed query will re-query the database, repopulating the cache transparently.

#### Admin API

```http
POST /api/v1/admin/explore/invalidate
Authorization: Bearer <admin-token>
Content-Type: application/json
```

**Request body:**

| Field | Type | Description |
| --- | --- | --- |
| `registry` | string (optional) | Registry to flush. Omit to flush everything. |

**Flush one registry:**

```sh
curl -X POST https://batlehub.example.com/api/v1/admin/explore/invalidate \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm"}'
# {"ok": true}
```

**Flush all registries:**

```sh
curl -X POST https://batlehub.example.com/api/v1/admin/explore/invalidate \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{}'
# {"ok": true}
```

**Responses:**

| Status | Description |
| --- | --- |
| 200 | `{"ok": true}` — cache flushed |
| 403 | Admin role required |

### Multi-instance deployments {#cache-ha}

The explorer cache is **per-process**. In a multi-replica deployment (Kubernetes, Docker Swarm), each replica has its own independent cache. This means:

- A flush via the API only affects the replica that handled the request.
- Cache TTLs on other replicas tick independently.

After a bulk data operation (database migration, mass publish, registry restructuring) you should call the flush endpoint for **every replica**, or wait up to 10 minutes for TTL expiry to propagate naturally.

See [High Availability](/guide/high-availability) for replica-aware rollout strategies.

---

## Performance notes {#performance}

The catalog queries run two CTEs that union `package_statuses` and `local_packages`, then join access-event counts. The following indexes (migrations 017, 021 and 060) keep these fast:

| Index | Purpose |
| --- | --- |
| `idx_access_events_pkg` on `(registry, package_name, package_version, created_at DESC)` | JOIN condition in the package list, and the per-package LATERAL count through its `(registry, package_name)` prefix |
| `idx_access_events_pkg_allowed_recent` on `(registry, package_name, package_version, outcome, created_at DESC) INCLUDE (user_id)` | `last_accessed_by` correlated subquery |
| `uq_package_status` on `(registry, package_name, package_version, …)` | Explorer GROUP BY aggregation, through its `(registry, package_name)` prefix |

These indexes are created automatically when BatleHub starts and runs migrations. No manual action is required.

For large registries (> 50 000 packages), the 10-minute in-memory cache reduces the load of repeated Explorer requests to near-zero. If you need a shorter TTL to reflect publishes faster, use the admin flush endpoint as part of your CI/CD pipeline (see [Automatic invalidation](#cache-auto-invalidate)).

---

## REST API {#api}

Registries whose RBAC grants the `anonymous` role read access can be queried without credentials; every other registry requires a Bearer token whose identity resolves to at least the `user` role. Either way, only registries the caller can explore are included in responses.

### List packages {#api-list}

```http
GET /api/v1/explore/packages
```

**Query parameters:**

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `registry` | string | — | Filter to a single registry. |
| `name` | string | — | Substring filter on package name (case-insensitive). |
| `sort` | `downloads` \| `name` \| `recent` | `downloads` | Sort order. |
| `page` | integer | `0` | Zero-based page number. |
| `per_page` | integer | `20` | Results per page. |

**Response:**

```json
{
  "items": [
    {
      "registry": "cargo",
      "name": "tokio",
      "version_count": 50,
      "total_downloads": 12500,
      "last_accessed": "2026-05-31T10:00:00Z",
      "source": "proxied",
      "has_blocked": false
    }
  ],
  "total": 150,
  "page": 0,
  "per_page": 20,
  "upstream_unavailable": false
}
```

`source` is one of `"proxied"`, `"local"`, or `"both"`.

`upstream_unavailable` is `true` only when the database was unreachable **and** no cached data was available for this query. Results will be empty. See [Explorer cache](#cache) for details.

### Registry statistics {#api-stats}

```http
GET /api/v1/explore/registries
```

Returns per-registry package counts and total download events for registries that already have cached packages. The web UI calls this alongside `GET /api/v1/registries` (which returns all configured registries) and merges the two lists so that empty registries show a count of `0`.

**Response:**

```json
{
  "registries": [
    { "registry": "cargo", "package_count": 120, "total_downloads": 45000 },
    { "registry": "npm",   "package_count":  30, "total_downloads":  8200 }
  ],
  "upstream_unavailable": false
}
```

### Package detail {#api-detail}

```http
GET /api/v1/explore/packages/{registry}/{name}
```

Returns all known versions of a package, the caller's gate status, and per-version firewall status.

**Response:**

```json
{
  "registry": "cargo",
  "name": "tokio",
  "gate": {
    "beta_member": false
  },
  "versions": [
    {
      "version": "1.38.0",
      "source": "proxied",
      "firewall": { "status": "clear" },
      "download_count": 500,
      "last_accessed": "2026-05-31T10:00:00Z",
      "published_at": null,
      "is_prerelease": false
    },
    {
      "version": "0.9.0",
      "source": "proxied",
      "firewall": {
        "status": "blocked",
        "reason": "CVE-2021-12345",
        "blocked_by": "admin",
        "blocked_at": "2026-01-10T12:00:00Z"
      },
      "download_count": 80,
      "last_accessed": "2026-01-09T09:00:00Z",
      "published_at": null,
      "is_prerelease": false
    }
  ],
  "upstream_unavailable": false
}
```

`firewall.status` is one of `"clear"`, `"blocked"`, or `"yanked"`. Blocked entries include `reason`, `blocked_by`, and `blocked_at`.

### Upstream search {#api-upstream}

```http
GET /api/v1/explore/upstream?name=<query>&registry=<optional>&limit=<n>
```

Queries upstream registry search APIs for packages matching `name`. Only registries the caller can explore are searched.

| Parameter | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | string | (required) | Search query. |
| `registry` | string | — | Limit to a single registry. |
| `limit` | integer | `10` | Maximum results per registry. |

**Response:**

```json
{
  "items": [
    {
      "registry": "npm",
      "name": "lodash",
      "latest_version": "4.17.21",
      "description": "Lodash modular utilities.",
      "already_cached": false
    }
  ]
}
```

`already_cached: true` means the package already appears in the main catalog (the UI suppresses it from the Not Yet Proxied section).
