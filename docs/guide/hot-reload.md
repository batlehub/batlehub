# Hot reload and dynamic config

BatleHub can reload its configuration at runtime without restarting the
process — but not all of it. Every key falls in one of three tiers:

- **hot** — every reader sees the new value once the reload is applied;
- **partial** — every request sees it, while a component built once at startup
  (warming, release imports, the scan worker) keeps the old value until a
  restart. A rotated upstream credential is the case that matters: installs use
  the new one at once, warming keeps calling with the old one;
- **restart** — nothing reads it again before a restart.

A key no row below names is **restart**: a section added to the configuration
later is restart-only until someone shows a reader for it, because a needless
restart costs seconds and a reload that silently does nothing costs an
incident. The most specific row wins, so
`registries[npm].cache.metadata_ttl_secs` is partial while
`registries[npm].cache.idle_days` is restart.

<!-- reload-keys -->
| Key | Tier | Read by |
| --- | --- | --- |
| `grants` | hot | the instance-tier grants |
| `ip_blocking.trusted_proxies` | hot | the proxy-trust policy, as the deprecated fallback of `server.trusted_proxies` |
| `limits.packages_per_page` | hot | listing pagination |
| `limits.versions_per_page` | hot | listing pagination |
| `registries[*].cache.serve_stale` | hot | the registry's metadata policy |
| `registries[*].grants` | hot | the registry's grant hierarchy |
| `registries[*].hosts` | hot | the host → registry routing table |
| `registries[*].path_routing` | hot | the host → registry routing table |
| `registries[*].rbac` | hot | the registry's grant hierarchy |
| `registries[*].rules` | hot | the registry's rule chain |
| `scanners.*.escalation` | hot | each registry's security policy |
| `search.readmes` | hot | the prose-search switch |
| `server.signed_urls` | hot | the download URL signer |
| `server.trusted_proxies` | hot | the proxy-trust policy, swapped together with the host routing table |
| `subdomain_routing` | hot | the host → registry routing table |
| `air_gap` | partial | the registry clients' air-gap wrapper; warming and release imports keep the startup clients |
| `limits.max_artifact_size_bytes` | partial | the request path; the scan worker keeps the startup limit |
| `proxy` | partial | the registry clients and cargo index; the README fetcher and warming keep the startup proxy |
| `registries[*]` | partial | the registry clients, policies and maps; warming and release imports keep the startup client |
| `registries[*].cache.artifact_ttl_secs` | partial | the registry's download policy; eviction keeps the startup TTL |
| `registries[*].cache.metadata_ttl_secs` | partial | the registry's metadata policy; warming keeps the startup client |
| `registries[*].cache` | restart | the warming and eviction services, built once |
| `registries[*].quota` | restart | the quota service, built once |
| `registries[*].rate_limit` | restart | the rate limiter, built once |
| `registries[*].security.rescan` | restart | the rescan scheduler, started at startup only when a registry already had one |
| `registries[*].storage` | restart | the storage router, built once |
| everything else | restart | read once at startup |
<!-- /reload-keys -->

Adding or removing a whole registry is **partial**: the request path serves it
at once; warming, eviction, rate limits, quota and the metric labels keep the
startup list, so the restart-only keys an added registry carries —
`rate_limit`, `quota`, `storage`, `cache.idle_days` — are listed under
`restart_required` beside it.

On an instance started with `BATLEHUB_DISABLE_HOT_RELOAD=1`, every change is a
restart, whatever the table says.

### What a reload reports

Every diff — the pending reload, `POST /api/v1/admin/config/validate`, the
immediate reload — carries three lists of dotted paths, an array entry named by
its `name` (or `type`): `registries[npm].upstreams`.

| Field | Contains | Compared against |
| --- | --- | --- |
| `applied` | hot keys this candidate changes | the config in force |
| `partially_applied` | partial keys this candidate changes | the config in force |
| `restart_required` | every restart key the running process does not have | the config the process **started** on |

`restart_required` is compared against startup on purpose: a `database.url`
change applied an hour ago is still listed on every later reload, until the
restart that makes it true. The file watcher logs it at `WARN`, and the Config
Reload page shows the three lists.

`restart_required` is `["*"]`, with the reason in `restart_reason`, when the
configuration places a `${VAR}` where the diff cannot keep it as text — outside
a one-line string value, in a key, or in the `name` or `type` of any `[[…]]`
entry — `[[registries]]`, `[[auth]]` and the rest (see
[Inline substitution](/guide/configuration#env-inline)). `restart_reason` names
the side that could not be compared: the candidate, the config in force, or the
startup config. Only that comparison is unknown, and unknown is a restart; the
other lists are still filled, and the reload itself goes on as before.

## 9.1 File Watcher

When the config file changes on disk, BatleHub automatically validates the new config (schema check + connectivity probes) and stores a **pending reload**. The admin then confirms or discards it via the UI or API. Pending reloads expire after 10 minutes.

When the process was started with more than one `--config`, **every layer is
watched and every reload re-reads all of them**. Rotating a credential touches
only the credentials file, and that alone stages a pending reload: the
byte-identical-rewrite dedup that exists for `touch` and atomic saves compares
all the layers, not just the first. See
[Layered config files](/guide/configuration#layered-config-files).

A layer that cannot be watched is logged and skipped rather than taking the
watcher down for the others; the reload path re-reads every layer anyway, so a
change to a watched file still picks up whatever the unwatched one now says.

The file watcher is enabled by default. Disable it with:

```sh
BATLEHUB_DISABLE_HOT_RELOAD=1 batlehub --config config.toml
```

Use this when `config.toml` is mounted as a read-only Kubernetes ConfigMap.

## 9.2 API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/admin/config/reload` | Immediate reload: validate + apply atomically |
| `GET` | `/api/v1/admin/config/pending` | Get pending reload diff (404 if none) |
| `POST` | `/api/v1/admin/config/pending/apply` | Apply the pending reload |
| `DELETE` | `/api/v1/admin/config/pending` | Discard the pending reload |
| `GET` | `/api/v1/admin/config/changes` | Paginated audit history (`?page=0&per_page=50`) |
| `GET` | `/api/v1/admin/config/warnings` | Non-fatal problems with the config currently in force |

```sh
# CI/CD: apply a new config atomically
curl -s -X POST \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/config/reload

# Two-step flow: let the file watcher load a pending, then apply from CI
curl -s -X POST \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/config/pending/apply
```

All reloads (applied or rejected) are written to the `config_changes` table with the diff, trigger source, and operator identity.

### Config warnings

Some config states are wrong enough to tell an operator about but not wrong
enough to refuse to start — a registry name that cannot become a DNS label, a
deprecated key being shadowed, a permissive security default left in place. These
are logged at startup and on every reload, **and** served from
`GET /api/v1/admin/config/warnings` so they can be seen without grepping logs:

```json
{
  "warnings": [
    {
      "code": "proxy-trust.unconfigured",
      "path": "server.trusted_proxies",
      "message": "no trusted-proxy list is configured, so Forwarded / X-Forwarded-Host / …"
    }
  ]
}
```

`code` is a stable slug, safe to match on; `path` points at the offending config
location verbatim, so it can be searched for in the TOML.

`POST /api/v1/admin/config/validate` and `POST /api/v1/admin/config/from-content`
return the same shape inline under `warnings`, describing the *candidate* config —
so an admin sees them **before** applying a pending reload rather than after. The
Config Reload admin page renders both.

| Code | Meaning |
|---|---|
| `proxy-trust.unconfigured` | No `trusted_proxies` list anywhere; forwarded host/scheme are believed from any client |
| `proxy-trust.deprecated-key-only` | Proxy trust comes from the deprecated `[ip_blocking].trusted_proxies` |
| `proxy-trust.invalid-deprecated-entry` | An entry of the deprecated `[ip_blocking].trusted_proxies` is not an IP or CIDR range and was dropped |
| `proxy-trust.shadowed-deprecated-key` | Both keys are set; `[server]` wins and the deprecated list is ignored entirely |
| `subdomain.invalid-dns-label` | `[subdomain_routing]` is on but a registry name cannot be a DNS label, so no wildcard host is derived for it |
| `vsx-signing.proxy-mode` | `[registries.vsx_signing]` on a registry in `proxy` mode: nothing is published there, so the key signs nothing; the upstream's signature is relayed regardless |

## 9.3 Global Admin Banner

Administrators can broadcast a message to all website visitors:

```sh
# Set a warning banner
curl -s -X PUT \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"message":"Maintenance window in 30 min","level":"warning"}' \
  http://localhost:8080/api/v1/admin/banner

# Clear it
curl -s -X DELETE \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  http://localhost:8080/api/v1/admin/banner
```

The frontend polls `GET /api/v1/banner` (no auth required) every 30 seconds. The banner backend uses the same infrastructure as the metadata cache:

| `[cache] type` | Banner storage |
|----------------|---------------|
| `"memory"` | In-process — not shared across replicas |
| `"redis"` | Redis — shared across all HA replicas |
| `"postgres"` | `system_kv` table — shared across all HA replicas |

