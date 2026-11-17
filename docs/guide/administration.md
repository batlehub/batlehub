# Administration

This page covers everything an administrator needs to operate BatleHub: configuration, storage, auth providers, registry management, health monitoring, cache cleanup, hot reloading, and the global banner.

For the complete TOML reference see [`docs/guide/configuration.md`](https://github.com/batlehub/batlehub/blob/main/docs/guide/configuration.md).

## Contents

The administration guide is split across four pages:

- **[Configuration](/guide/admin-config)** — the TOML config file and loading order, `${VAR_NAME}` secret injection, named `PROXY_CACHE__*` overrides, registry modes, auth providers (static tokens, OIDC, Kubernetes, user tokens), runtime **hot reload** of the config, and the **global banner**.
- **[Storage & Health](/guide/admin-storage-health)** — filesystem, S3-compatible, and multi-backend **storage**, plus **health & observability**: the health endpoint, cache clearing, and OpenTelemetry tracing.
- **[Policies & packages](/guide/admin-policies)** — **cache policy** (eviction, cache warming, deduplication), **package management** (list, block, bulk-block, invalidate), and per-registry **rules** (release age gate, deny latest, trusted publisher).
- **[Access & audit](/guide/admin-access)** — **team namespaces & package visibility**, the **audit log**, the **beta/pre-release channel**, and **IP-based blocking**.

## Red paths: where to act

A *red path* is the route from what brought you here to the control that
resolves it ([RFC 0046](/rfc/0046-admin-red-paths)). Every row is also one
`Ctrl+K` (`⌘K` on macOS) away: type the word in the last column into the
console's *Go to…* palette.

| What happened | Where | Palette |
| --- | --- | --- |
| "I can't download X" | *Access → Authorization*, the *Simulate* panel (`/admin/access/authorization#simulate`) | `refused` |
| An upstream is degraded | the registry's page, then *Registries → Upstream* (`/admin/registries/upstream`) | `upstream` |
| One registry's config must change | the registry's page → *Edit configuration*, which opens *System → Configuration* on its block | `config` |
| A CVE: which versions carry it? | *Security → Findings* (`/security/findings`) | `cve` |
| A scan failed | *Security → Scanners & worker*, failures tab (`/security/scanners?tab=failures`) | `rescan` |
| Block one version | the package's page, *Admin* tab | `block` |
| Block many versions | *Packages → Block in bulk* (`/admin/packages/bulk`) | `bulk` |
| A leaked token or an abusive address | *Instance security → Blocked accounts & addresses* (`/admin/instance-security/blocks`) | `token` |
| A package looks stale | *System → Health & cache* → *Refresh explorer* (`/admin/system/health`) | `stale` |
| Who did this? | *System → Audit log* (`/admin/system/audit-log`) | `audit` |
| Announce maintenance | *System → Global banner* (`/admin/system/banner`) | `banner` |
| A team needs a namespace | *Access → Team namespaces* (`/admin/access/team-namespaces`) | `namespace` |

Two triggers have no control in the console yet, and are left to their own
RFCs: **revoking every token of one user** (only blocking the account exists,
above) and **what changed in the configuration last week** beyond the change
history shown on *System → Configuration*.

Bookmarks to the pages' former addresses, from before the sections above,
keep working: each redirects to its new page with its query string.

## The console's two spaces

The console has two sidebars ([RFC 0050](/rfc/0050-security-console-completion-and-admin-navigation)):

- **`/admin`** — the instance: registries, packages, access, **Instance
  security** and system. *Instance security* (`/admin/instance-security`,
  `system:read`) says what concerns this server itself over the last 24 hours
  or 7 days: rejected credentials and their addresses, banned addresses, tokens
  used from a new address, static tokens still stored in clear, exemptions that
  end within a week, and the security warnings of the configuration. Addresses
  and user ids appear only with `audit:read`. Its *Blocked accounts &
  addresses* page is the former *Access → Blocks*.
- **`/security`** — the packages: an **overview** that says, per domain, whether
  anything needs attention (holds, flags, scanners & worker, coverage), then
  *Triage* (holds, findings, flags, exposure) and *Follow-up* (trends,
  coverage, scanners & worker). It opens on the first page your verbs read;
  see [Security gates](/guide/security-gates#console).

The admin dashboard carries the same security lines, one per domain your verbs
read. Each sidebar group opens and closes, and the browser remembers which you
opened; the group holding the current page is always open.
