# Hot reload and dynamic config

BatleHub can reload its configuration at runtime without restarting the process. The following components are hot-swappable:

- Registry list (add, remove, or update a registry)
- Per-registry RBAC (`anonymous`, `user`, `admin`, group-based access)
- Per-registry policy rules (age gate, deny latest)
- Per-registry versioning, signing, and beta-channel configuration
- Artifact size limit
- Host-based routing (`hosts`, `path_routing`, `[subdomain_routing]`) and the
  `trusted_proxies` policy that governs it — the two swap together, so a reload
  that turns host routing on never runs under the old trust policy

The following components **require a process restart**:
- Server host / port
- Database URL or connection pool size
- Auth providers (`[[auth]]`)
- Storage backends

## 9.1 File Watcher

When the config file changes on disk, BatleHub automatically validates the new config (schema check + connectivity probes) and stores a **pending reload**. The admin then confirms or discards it via the UI or API. Pending reloads expire after 10 minutes.

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

