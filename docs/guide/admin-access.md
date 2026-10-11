# Access & audit

## Team Namespaces & Package Visibility {#team-namespaces}

Team namespaces let you assign a package name prefix within a registry to an auth-provider group. Only group members — and admins — may publish packages under that prefix. Package visibility independently controls who can download a package.

This feature requires no TOML changes and no server restart — claims and visibility are managed entirely via the admin API.

For the full reference (visibility levels, download-time enforcement, longest-prefix rule, registry support matrix) see the [Access Control guide](/guide/access-control#team-namespaces).

### Managing namespace claims

```sh
# List claims for a registry
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces

# Claim a prefix for a group
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"prefix":"frontend","group_id":"oidc:frontend-team","claimed_by":"admin"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces

# Release a claim (prefix may contain slashes, passed verbatim in the path)
curl -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces/frontend
```

### Managing package visibility

Visibility is package-level — all versions share the same setting. Accepted values: `public` (default), `internal`, `team`.

```sh
# Read current visibility
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility

# Restrict to team members only
curl -X PUT \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"team"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility
```

Package names containing slashes must be percent-encoded in the URL (`/` → `%2F`).

---

## Audit log {#audit-log}

Every access-control decision (allow or deny) is recorded in PostgreSQL, and so
is every administrative action — blocks, ownership changes, deletions,
retention runs.

```sh
# Last 50 decisions across all registries
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/audit-log?per_page=50"

# Filter by registry and outcome
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/audit-log?registry=npm&denied_only=true&per_page=100"
```

### Asking what happened to a package {#audit-actions}

Downloads outnumber everything else by orders of magnitude, so the question
"what was deleted here" is only answerable with `action`. It takes one action or
a comma-separated set, and an unknown name is a `400` listing the alternatives —
never an empty page, which reads as "nothing happened".

```sh
# Every deletion in a registry: by hand and by policy
batlehub admin audit-log --registry acme-npm --action delete,retention_reclaim

# Un-caching is a different question — the bytes come back on the next request
batlehub admin audit-log --registry acme-npm --action cache_evict,cache_clear

# One package's whole history
batlehub admin audit-log --registry acme-npm --package internal-tool

# The same set, exported for an auditor
batlehub admin export-audit-log --action delete,retention_reclaim --format csv
```

`--action` and `--package` are query parameters on the endpoint too
(`?action=delete,retention_reclaim&package_name=internal-tool`), on both the
listing and the export.

| Action | Recorded when |
| --- | --- |
| `delete` | a person deleted a version |
| `retention_reclaim` | a retention policy reclaimed one — [see below](/guide/admin-policies#retention-trail) |
| `retention_run` / `retention_dry_run` | a retention run finished, live or preview |
| `cache_evict` | one proxy-cached artifact was dropped by hand — a copy, not the package |
| `cache_clear` | a whole registry's cache was dropped by hand |
| `cache_evict_run` / `cache_evict_dry_run` | an eviction sweep finished, live or preview — [see below](/guide/admin-policies#cache-eviction-trail) |
| `cache_coherence_run` / `cache_coherence_dry_run` | a sweep collected blobs nothing references — [see below](/guide/admin-policies#cache-coherence) |
| `tombstone_compact` | a registry's aged-out tombstone detail was stripped |
| `audit_purge` | this trail itself was purged to a cutoff |
| `block` / `unblock`, `block_user` / `unblock_user`, `block_ip` / `unblock_ip` | the corresponding admin action; `detail` names the IP for the last two. `block_ip` is also written by `system` for an automatic ban, with the violation count that tripped it |
| `yank` / `unyank`, `deprecate` / `undeprecate`, `unlist` / `relist` | a lifecycle change on one version |
| `add_owner` / `remove_owner`, `set_visibility`, `claim_namespace` / `release_namespace` | ownership and visibility |
| `download` / `view_metadata` | a read, allowed or denied |

The response JSON spells these without the underscores (`retentionreclaim`);
the filter accepts either spelling, so an action pasted out of any response
works.

Example entry:

```json
{
  "id": "01j...",
  "timestamp": "2025-05-22T10:00:00Z",
  "registry": "npm",
  "package": "lodash",
  "version": "4.17.21",
  "user_id": "ci",
  "role": "user",
  "outcome": "allow",
  "rule": null
}
```

---

## Beta/Pre-Release Channel {#beta-channel}

Gate pre-release versions (e.g. `1.0.0-beta.1`) to specific users or groups. Non-members see only stable versions and get 404 on pre-release artifact downloads.

Enable per registry:

```toml
[registries.beta_channel]
enabled = true
```

Manage members at runtime:

```sh
# Add a user
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"principal_type":"user","principal_id":"alice"}' \
  http://localhost:8080/api/v1/admin/registries/my-npm/beta-channel

# List members
curl -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/registries/my-npm/beta-channel

# Remove a member
curl -X DELETE -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/registries/my-npm/beta-channel/user/alice
```

See the [Access Control guide](/guide/access-control#beta-channel) for the full reference, including group membership, per-registry support table, and user-facing behaviour.

---

## IP-Based Blocking {#ip-blocking}

Automatically block IPs that trigger too many violations (rate-limit hits, auth failures) within a time window.

```toml
[ip_blocking]
enabled               = true
violation_threshold   = 10
violation_window_secs = 300      # 5-minute window
ban_duration_secs     = 3600     # 1-hour block
trigger_on_status     = [429, 401]
```

Manage blocks manually:

```sh
# List blocked IPs
curl -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/ip-blocks

# Block an IP
curl -s -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"ip":"1.2.3.4","reason":"bad actor","duration_secs":86400}' \
  http://localhost:8080/api/v1/admin/ip-blocks

# Unblock
curl -s -X DELETE \
  -H "Authorization: Bearer <admin-token>" \
  http://localhost:8080/api/v1/admin/ip-blocks/1.2.3.4
```

Blocked IPs receive `403 Forbidden` with `X-Block-Expires`. The check runs before authentication. Violation counts and blocks are stored in the same backend as the rate-limit store (`memory` / `postgres` / `redis`).

See the [Access Control guide](/guide/access-control#ip-blocking) for the full reference including load-balancer setup and storage backend comparison.
