---
reference: true
---

# RFC 0042 — Scan visibility and admin views

| Field       | Value                                                        |
| ----------- | ------------------------------------------------------------ |
| Status      | Accepted                                                         |
| Short       | Scan visibility and admin views                               |
| Settles     | How a reader sees whether a version was scanned, and where an administrator follows scans, findings, rescans and one registry's configuration and health |
| Author      | Max Batleforc <maxleriche.60@gmail.com>                       |
| Co-author   | Claude Opus 5.5 <noreply@anthropic.com>                       |
| Created     | 2026-11-10                                                    |
| Supersedes  | —                                                             |
| Touches     | `crates/config` (`schema/mod.rs` `StatsConfig`: `dashboard_url`, `registry_dashboard_url`; `Cargo.toml`: `url`, `form_urlencoded`), `deploy/grafana/batlehub-production.json` (a `registry` variable), `config.example*.toml`, `crates/core` (`entities/security.rs`, `services/scan_status.rs` new, `ports/security.rs`, `services/scan_worker.rs`, decision 34), `crates/config/src/reload_keys.rs` and `docs/guide/hot-reload.md` (decision 32), `crates/adapters/migrations` + `migrations.rs` (decision 33), `perf/profile_routes.txt`, `tests/heavy/{quarantine.sh,config.db-calls.toml,db_calls.budget.json}`, `crates/adapters` (`db/security.rs`, `registry/http_client.rs` for `redact_text_urls`, the in-memory `ScanQueue`/`VerdictRepository`), `crates/web` (`handlers/security.rs`, `handlers/front_office/explore/{list,detail}.rs`, `handlers/back_office/packages/list.rs`, `handlers/back_office/registries.rs` new, `services/reload/{mod,applier}.rs`, `lib.rs`, `tests/`), `ui` (`pages/PackageDetailPage.vue`, `pages/PackageCatalog.vue`, `pages/AdminPackages.vue`, `pages/AdminScans.vue` new, `pages/AdminFindings.vue` new, `pages/AdminRegistry.vue` new, `pages/AdminRegistries.vue` new, `pages/AdminDashboard.vue`, `pages/AdminHealth.vue`, `pages/AdminConfigReload.vue` (the `?registry=` jump), `composables/usePoll.ts` new, `components/admin/StatsTrendChart.vue` new (extracted), `components/package/`, `config/adminSections.ts`, `router/index.ts`, `locales/{en,fr}.json`, `openapi.json`, `client/`), docs (`guide/security-gates.md`, `guide/configuration.md`, `guide/administration` pages, French twins) |
| Risk        | medium — §7 (a findings list across registries and a configuration view are two new places a secret or an embargoed advisory can leak from) |

---

## 1. Summary

The console tells nobody whether a version has been scanned. A reader sees
a verdict only by scrolling to the `VerdictPanel` of a 1 954-line package
page. An administrator has no list of findings, no view of the scan queue,
no way to know when a version will be rescanned and no page about one
registry. Everything this RFC shows already exists on the server
(`artifact_verdicts`, `scan_jobs`, `registry_scan_state`, the `upstream_*`
fields of `/api/v1/admin/stats`). What is missing is a way to read it.

This RFC does five things:

- it adds one computed `scan_status` field to the responses that already
  describe a version or a package;
- it splits the package page into tabs;
- it adds four admin pages, Scans, Findings, Registries and one Registry
  page per registry, backed by three
  read endpoints;
- it shows the two next-rescan dates;
- it puts the existing gate exemption one click away from a finding.

It adds no table, no column, no verb and no write endpoint. It does add two
optional link templates under `[stats]`, which point the admin pages at the
operator's own monitoring tool (Grafana, ClickStack or any other). BatleHub
never queries that tool.

### Before / after

```text
# today — package page, top to bottom (PackageDetailPage.vue, 1 954 lines)
trail · header · one version (vulns, SBOM) · README · moving refs ·
VerdictPanel · versions index · Administration
  → "was 2.4.1 scanned?" means scrolling to VerdictPanel
  → the catalog and the admin package list say nothing about scanning

# with this RFC
header: lodash  2.4.1 ▾  [● scanned · allowed]       (sticky)
tabs:   Overview | Versions | Security | Administration     ?tab=security
catalog row: lodash  12 versions  [● scanned]
/admin/security/scans     queue · failures · verdicts by state
/admin/security/findings  by finding · by reference (CVE → packages)
/admin/observability/registries/npm-public  config · health · suggestions
```

```mermaid
flowchart LR
    subgraph T["today"]
        R1["reader"] --> PD1["package page"] --> V1["GET /verdicts/…"]
        A1["admin"] -.->|"no page"| Q1["scan_jobs"]
        A1 -.->|"no page"| F1["artifact_findings"]
        A1 -.->|"raw TOML, secrets included"| C1["GET /admin/config/content"]
    end
    subgraph W["with this RFC"]
        R2["reader"] --> L2["lists + package page<br/>scan_status"]
        A2["admin"] --> S2["GET /admin/scans"]
        A2 --> F2["GET /admin/findings"]
        A2 --> G2["GET /admin/registries/#123;name#125;<br/>allow-listed DTO"]
    end
```

The registry page reads an allow-listed DTO, not the raw file, so it
**cannot** show more than the editor already does, and it shows less. The
raw `content` endpoint itself is unchanged and still serves secrets to
`config:read` (decision 9). Every new arrow is a read.

---

## 2. Motivation

1. **"Was this version scanned?" has no answer outside one panel.**
   `PackageCatalog.vue` and `AdminPackages.vue` render no verdict or scan
   indicator. `VerdictPanel.vue` (around lines 191–198) shows
   `last_scanned_at` or "never", and only after five other sections. A
   developer evaluating a dependency cannot tell "not scanned" from
   "scanned and clean" without opening the page and scrolling.
2. **The package page is too long to find anything in.**
   `PackageDetailPage.vue` stacks eight sections with no tabs. The page
   itself makes five API calls (`:402, :457, :536, :943, :966`) before its
   child panels add their own, and it carries an admin block of five
   components. The security content (the
   vulnerabilities at :1433, the SBOM at :1492 and the `VerdictPanel` at
   :1579) is split across two non-adjacent places.
3. **An administrator cannot see findings across packages.** There is no
   endpoint that lists findings. They exist only inside one verdict
   (`GET /api/v1/verdicts/{r}/{n}/{v}`, `handlers/security.rs:337`). "Which
   packages carry GHSA-xxxx?" has no answer in the console.
4. **The scan queue is invisible.** `scan_jobs` (migration 051) records
   leases, attempts and `last_error`. The only window onto it is the gauges
   `batlehub_scan_jobs_queued` and `batlehub_scan_jobs_leased`, which give
   counts and never name a coordinate. `GET /api/v1/admin/verdicts`
   (`handlers/security.rs:720`) exists, and no `.vue` file calls it. A
   version stuck in `SCANNER_ERROR` after `max_attempts` (3,
   `scan_worker.rs:56`) is found when a developer complains.
5. **Nobody can tell when a version will be rescanned.** `RescanScheduler`
   (`services/rescan.rs:51`) picks up a verdict when
   `last_scanned_at < now - interval` (`adapters/src/db/security.rs:249`).
   `[registries.security.rescan] interval_secs` is absent by default, which
   means never rescan, and nothing shows the operator which of the two cases
   they are in.
6. **The registry health that is computed is never shown.** `RegistryStatsDto`
   (`back_office/stats.rs:91`) carries `upstream_degraded`,
   `upstream_error_rate` and `upstream_latency_ms`, and no `.vue` file reads
   any of them. Degradation surfaces as a `tracing::warn!`
   (`services/metrics.rs:152–165`) and a gauge. The Dashboard does show a
   different test, "Errors — see health" from `recent_errors`
   (`AdminDashboard.vue:68–70`). So an operator sees "this registry had
   errors", but never "its upstream is over the error-rate or latency
   threshold".
7. **There is no page about one registry.** To learn how `npm-public` is
   configured, an admin reads the whole raw TOML in `AdminConfigReload.vue`.
   That editor serves the file as written, `upstream_auth` credentials
   included, to every holder of `config:read`. Neither its health nor its
   scan freshness is in the same place.

---

## 3. Goals / non-goals

**Goals**

- Every list or page that names a version or a package says, at a glance,
  whether it was scanned, is being scanned, is waiting, or failed, to
  exactly the callers who may already learn that a version is held.
- The package page is navigable: its security content sits in one tab, and
  a tab can be linked.
- An administrator can list findings across the registries they may read,
  by finding or by reference, and reach the exemption for a version from
  there.
- An administrator can follow the scan queue: what is waiting, what is
  leased and by whom, what failed and why. They can relaunch a scan.
- An administrator can see when a version will next be rescanned, on each
  of the two clocks, including "never".
- An administrator has one page per registry with its effective
  configuration (no secret values), its upstream health, its scan freshness,
  a closed list of computed suggestions and a link to the editor.
- That page shows the registry's activity over time (hit rate, requests,
  listing reads, storage) from the history BatleHub already keeps.
- When the operator configures their monitoring tool's URL, the dashboard
  and each registry page link straight into it, filtered on that registry.

**Non-goals**

- **A new bypass that serves a version before it is scanned.** Quarantine
  exists to prevent exactly that. The bypass is the existing gate exemption
  (`EXEMPTIBLE_GATES`, `entities/policy.rs:479`), which already demands a
  `reason` and an `exempt_until`.
- **Triage state per finding** (accepted, false positive, suppressed). It
  would be new persisted data and a second override path beside the
  per-version exemption.
- **A four-eyes approval workflow for configuration changes.** It is a new
  persisted state and an approval model. The "suggest a change" of this RFC
  is a computed hint and a link to the existing editor.
- **Fixing approver = author in the config staging flow.** `from-content`
  stages a change "for approval" (`back_office/config.rs:46`) and `apply`
  needs the same `config:write`, so the author can approve their own change.
  It is a real gap and is out of scope here; recorded as decision 14.
- **Showing which layer a key came from.** `load_layered`
  (`crates/config/src/lib.rs:169`) merges TOML documents and keeps no
  provenance. The page says how many layers were loaded.
- **Configurable degradation thresholds.** `DEGRADED_ERROR_RATE_PERMILLE`
  and `DEGRADED_LATENCY_MS` stay constants (`services/metrics.rs:8–9`). The
  page shows them as they are.
- **Querying Prometheus (or any monitoring backend) from BatleHub.**
  Prometheus knows nothing of grants, so a delegate scoped to one registry
  would read them all. The page links out instead, and the monitoring tool
  enforces its own access (§12).
- **Keeping upstream error and latency history in BatleHub.**
  `stats_history` has no such column, and the metrics
  `batlehub_upstream_errors_total` and
  `batlehub_upstream_request_duration_seconds` already carry it per
  registry for anyone who scrapes `/metrics`.
- **Choosing the monitoring tool.** The link is a template the operator
  writes. `deploy/grafana/batlehub-production.json` is an example, not a
  dependency.
- **Push updates (SSE, WebSocket).** A bounded poll covers the only
  transient states.
- **CLI or TUI work.** The new field reaches the CLI through the responses
  it already decodes. Nothing in `cli/` changes.
- **A new verb.** Every surface maps to a verb that exists (§7.2).

---

## 4. User-facing design

### 4.1 Configuration

Two optional keys are added under the existing `[stats]` section
(`StatsConfig`, `crates/config/src/schema/mod.rs`). Both are absent by
default:

```toml
[stats]
# Global monitoring link, shown on Admin → Dashboard. No placeholder.
dashboard_url = "https://grafana.example.com/d/batlehub-production/batlehub"
# Per-registry link, shown on each registry page. `{registry}` is the only
# placeholder, and it is replaced percent-encoded.
registry_dashboard_url = "https://grafana.example.com/d/batlehub-production/batlehub?var-registry={registry}&from=now-24h&to=now"
# The same key for ClickStack, Kibana or anything else that takes a filter in its URL:
# registry_dashboard_url = "https://clickstack.example.com/dashboards/batlehub?where=registry%3A%22{registry}%22"
```

- When a key is **absent**, its button is absent. There is no default URL,
  because BatleHub cannot guess the operator's tool. An **empty string** is
  rejected (§4.3), so it cannot be mistaken for "absent".
- These keys are not security-relevant defaults: they only show a link. The
  link is still validated (§4.3), because a config value rendered as an
  `href` is a script vector when it is `javascript:`.

The pages also read existing keys:

```toml
[[registries]]
name = "npm-public"
[registries.security.rescan]
interval_secs = 604800   # absent (the default) = never rescanned on a clock

[vulnerability_scan]
enabled = true
interval_secs = 86400    # the SBOM/OSV re-check, default 86400
```

- `rescan.interval_secs` **absent** is shown as "never (no interval
  configured)". It is never shown as an empty date.
- `vulnerability_scan.enabled = false` is shown as "SBOM re-check
  disabled". It is never shown as a date.

### 4.2 Behaviour rules

**`scan_status`** is derived on the server by a single function, in this
order (first match wins):

| # | Condition | `scan_status.state` |
| --- | --- | --- |
| 1 | the registry has no `[registries.security]` | field absent |
| 2 | an open job (`completed_at IS NULL`) with `leased_by IS NOT NULL` and `leased_until > now` | `scanning` |
| 3 | an open job otherwise: never leased, lease expired, or **deferred** (`leased_by IS NULL`, `leased_until` in the future, which is how `defer` marks it, `db/security.rs:585`) | `queued` |
| 4 | no verdict row | `not_scanned` |
| 5 | the **latest closed** job for the coordinate was closed as exhausted, or the verdict carries `SCANNER_ERROR` | `failed` ("closed as exhausted" is the `exhausted:` prefix the worker writes, decision 34) |
| 6 | the verdict carries `SCAN_PENDING` | `queued` |
| 7 | otherwise | `scanned`, plus `verdict: allowed\|warned\|quarantined\|denied` |

- Rule 2 needs `leased_by`. A deferred job has `leased_until` in the future
  for up to an hour (`scan_worker.rs:114`) but no worker. The existing
  `batlehub_scan_jobs_deferred` gauge draws the same line
  (`db/security.rs:626`).
- Rule 2 comes before rule 5, so a rescan of a failed version shows
  `scanning`, not `failed`.
- Rule 5 looks at the job, not only the verdict. An exhausted `rescan` or
  `backfill` job writes no `SCANNER_ERROR` (`scan_worker.rs:850`), so
  without it a failed rescan would keep reading `scanned · allowed`. Rule 5
  reads only the latest closed job per coordinate. Closed jobs are never
  deleted, so an old failure must not outlive a later success.
- Rule 6 covers a verdict written before its job (the `first_seen` race), so
  that version never reads as `scanned`.
- **Uninteresting case:** a registry without `[registries.security]` has no
  scanning at all. The field is absent and no badge is drawn. It is not
  `not_scanned`, because that would tell every reader of an unscanned
  registry that something is missing.

**Who sees what** follows `hold_visibility` (`handlers/security.rs:238`), the
function the verdict GET already uses:

| `hold_visibility` | `scan_status` returned |
| --- | --- |
| `Hidden` (no `quarantine:read`) | absent, as if the registry had no security section |
| `Reason` (`quarantine:read`) | `state` and `verdict` |
| `Findings` (`findings:read`) | the above plus `last_scanned_at`, `attempts` and `last_error` (redacted, then truncated to 200 chars, §7.3) |

The next-rescan dates and the queue detail (`leased_by`, `trigger`) are
returned only to holders of `system:read`. That verb lives at the
**instance tier only**: it guards endpoints that name no registry, and a
registry-tier block cannot grant it (`schema/mod.rs:108–112`). It is
checked with `authorize_control(hot, None, …)` (`authz/chain.rs:453`).

**Computing visibility is not per row, and ignores shadows.** For a list
page, the tier is resolved **once per (registry, identity)**:
`crate::entities::resolve` over the instance and registry nodes, the same
shadow-free resolution `installable_registries` makes
(`authz/chain.rs:589`, "a shadow is not a grant"). For a registry with
namespace- or package-tier nodes, it falls back per coordinate to
`resolution_path_for_coordinate` (`chain.rs:424`) and the same `resolve`,
again without a shadow. It never falls back to `hold_visibility`, whose
`authorize_grants` answers `Ok` under an active shadow (`chain.rs:271`). So
under a dry run, a list can be **stricter** than the per-version verdict
GET, never wider. That is the fail-closed direction, the same reasoning
§5.2 gives for findings. When
every registry on the page resolves to `Hidden`, which is the case for an
anonymous caller on a default config, the status batch is **not run at
all**. The anonymous explore list therefore keeps its warm cost of 0
statements (`tests/heavy/db_calls.budget.json`, `api-explore-list`).

**Lists** (`/api/v1/explore/packages`, `/api/v1/admin/packages`) carry the
status of the **most recently cached version** of each row. It is computed
in one batched call per page, never per row, and the batch spans every
registry on the page (`registry = ANY($1)`). The Versions tab carries it
per version.

**Lists show the last recorded verdict.** The verdict GET re-evaluates
through `VerdictService::current()` against the current policy and clock
(`verdict.rs:418`). A list reads the stored row. The two can differ for the
few minutes after a release-age window lifts or a policy changes, so the
list badge's tooltip says "as last recorded". `scan_status_for` must
**never** call `current()`: it writes (it enqueues a scan and upserts the
verdict, `verdict.rs:448–460`), and a page view must not trigger a scan. A
test pins this.

**The verdict GET keeps its `404` when there is no verdict row**
(`handlers/security.rs:399`). The CLI's `wait` reads that `404` as "no
verdict yet" (`cli/src/cli/security.rs:328`). The Security tab therefore
polls the explore detail for the selected version, which always answers,
and not the verdict GET.

**Next rescan**, per version:

- `verdict_rescan_at = last_scanned_at + rescan.interval_secs`. It is `null`
  when no interval is configured, and "due" when it is in the past (the
  scheduler ticks every 60 s, `RESCAN_TICK`, `rescan.rs:37`).
- `sbom_recheck_at = registry_scan_state.last_scan_at +
  vulnerability_scan.interval_secs`. This one is per registry: the SBOM pass
  covers a whole registry. It is `null` when disabled.

**Polling:** the Security tab and the Scans page poll every 5 s, only while
one displayed status is `queued` or `scanning`. The poll stops in three
cases:

- on `document.visibilityState === "hidden"`;
- after 10 minutes, when the page shows a "Refresh" button, so a stuck job
  cannot keep a tab polling forever;
- after three failed responses in a row. Failures back off ×2, up to 60 s,
  and a `Retry-After` header is honoured, so open tabs do not hammer a
  database that is already slow.

**Tabs:**

- `?tab=overview|versions|security|admin`. An unknown or forbidden value
  falls back to `overview` without an error.
- **Without `tab`**, a URL that carries the versions index's own `q` or
  `page` (`PackageDetailPage.vue:269–296`) opens **Versions**, so existing
  links into the index keep landing on it.
- The Administration tab is not rendered without the verbs its components
  need.
- The URL changes with `router.replace`, so tabs do not stack in the
  history.

**Suggestions** (Registry page) are a closed list, computed on the server:

| Code | Condition | Message |
| --- | --- | --- |
| `degraded_without_stale` | `upstream_degraded` and `cache.serve_stale = false` | "The upstream is degraded and stale metadata is not served: clients get 502s. Consider `serve_stale = true`." |
| `security_without_rescan` | `[registries.security]` present, `rescan.interval_secs` absent | "Versions are scanned once and never rescanned. New advisories reach them only through the SBOM re-check." |
| `scan_failures` | at least one coordinate of this registry whose `scan_status` is `failed` (rule 5) | "N versions failed to scan; see Scans → Failures." |

A suggestion is text and a link. It never writes anything.

**Monitoring links:**

- The server builds the final URL, so the UI never substitutes anything
  itself:
  - `RegistryView.monitoring_url` is `registry_dashboard_url` with
    `{registry}` replaced by the percent-encoded registry name;
  - `StatsResponse.monitoring_url` is `dashboard_url`, as is.
- Each field is returned only to holders of `system:read` (instance tier).
  The URL can name an internal monitoring host, which a `stats:read`-only
  delegate has no reason to learn.
- The button opens a new tab with `rel="noopener noreferrer"`, so the
  monitoring tool learns neither the console's URL nor gets a handle on its
  window.
- No `[stats]` key reloads today: `RELOAD_KEYS`
  (`crates/config/src/reload_keys.rs`) has no `stats.*` row, so they fall
  under "restart". These two become hot: one `RELOAD_KEYS` row each (decision 32).

**Upstream health is per replica.** `upstream_degraded`, the error rate
and the latency come from `ProxyMetrics`, an in-process `AtomicBool` and
counters (`services/metrics.rs:26,77`) that reset on restart. With several
replicas behind a load balancer, two refreshes can show two different
answers. The status strip therefore labels them "this replica, since
<start time>". The estate-wide view is the monitoring link's job.

**"Degraded" has one meaning.** The Dashboard's per-registry State column
already marks a registry "Errors — see health" from `recent_errors`
(`AdminDashboard.vue:68–70`), which is a different test. This RFC does not
add a second "degraded" chip next to that column. The Dashboard's registry
cell links to the registry page, and the registry page names its test:
"upstream degraded (error rate or latency over threshold)".

**Activity history** reuses `GET /api/v1/admin/stats/history?window=1|7|30`
(`back_office/stats_history.rs:108`). It is gated by `stats:read` and
already filtered per registry. The page keeps only its registry's
`per_registry` entry. The hit rate is computed in the UI from `hits` and
`misses` per hour; an hour with no row is drawn as a gap, not as zero.

### 4.3 Validation

Rejected on input (`400`, before any query):

| Condition | Rationale |
| --- | --- |
| `per_page` > 200 on `/admin/findings` or `/admin/scans` | bounds one admin's query cost |
| `severity` not in the `Severity` enum, `state` not in `VerdictState` | a typo must not read as "no results" |
| `reference` longer than 128 chars | advisory ids are short; the filter is a prefix match |
| unknown `{name}` on `/admin/registries/{name}` | `404` "no registry named `{name}`". A caller without `system:read` gets `403` before the lookup, so the `404` never answers an unauthorised caller and enumerates nothing |

Rejected at config load (`AppConfig::validate()`; on reload the previous
config stays):

| Condition | Rationale |
| --- | --- |
| `dashboard_url` or `registry_dashboard_url` not parseable by `url::Url`, or a scheme other than `https`/`http` | a `javascript:` or `data:` value rendered as an `href` runs in the console's origin |
| an empty string | it is neither absent nor a link; fail at load rather than draw a dead button |
| `dashboard_url` containing `{` | it has no placeholder, so a brace is a copy of the per-registry template by mistake |
| `registry_dashboard_url` with a placeholder other than `{registry}`, or without `{registry}` | an unknown placeholder would ship literally; a template without it links every registry to the same unfiltered view |
| the URL after substitution of a test name (`a/b c`) fails to parse | catches a placeholder in the host part, where percent-encoding is not valid |
| the URL carries userinfo (`user:pass@`) | a credential in a link shown to every `system:read` holder |

Each message names the field path and the rule, as the existing
`validate_*` errors do. For example: "`stats.registry_dashboard_url`: the
template must contain `{registry}`", or "`stats.dashboard_url`: scheme
`javascript` is not allowed (http or https only)". The check is a new
`validate_stats_urls()` called from `validate()`
(`schema/mod.rs:1899`).

Warnings, and where the operator sees them:

| Condition | Behaviour | Surfaced in |
| --- | --- | --- |
| a job past `max_attempts` | listed under Failures | Scans page badge count; Registry page suggestion `scan_failures` |
| `rescan.interval_secs` absent with security on | — | Registry page suggestion `security_without_rescan` |
| upstream degraded | — | Registry page status strip, red; the existing gauge |
| `dashboard_url` or `registry_dashboard_url` uses `http` | accepted | startup log `WARN` and the config editor's warnings panel (`GET /admin/config/warnings`), pushed from `warnings()` (`schema/mod.rs:701`), not from `validate()` |

### 4.4 API surface

| Method | Path | Auth | Who may call | Limit |
| --- | --- | --- | --- | --- |
| GET | `/api/v1/explore/packages` (changed: `scan_status` per row) | as today | as today; field filtered by `hold_visibility` | as today |
| GET | `/api/v1/explore/packages/{r}/{n}` (changed: per version) | as today | as today; same filter | as today |
| GET | `/api/v1/admin/packages` (changed: per row) | bearer | as today; same filter | as today |
| GET | `/api/v1/verdicts/{r}/{n}/{v}` (changed: `scan_status`, and for `system:read` `next_rescan`; still `404` with no verdict row) | as today | as today | as today |
| GET | `/api/v1/admin/scans?registry&state=queued\|scanning\|failed&page&per_page` | bearer | `system:read` (instance tier); `403` otherwise | 200/page |
| GET | `/api/v1/admin/findings?registry&severity&scanner&reference&verdict&group=finding\|reference&page&per_page` | bearer | `findings:read`, per registry, through `authorize_control` (§7.2); `403` when held on none | 200/page |
| GET | `/api/v1/admin/registries/{name}` | bearer | `system:read` (instance tier); the `config` block also needs `config:read`, and the upstream URLs `upstreams:read` | — |
| GET | `/api/v1/admin/stats` (changed: `monitoring_url`) | bearer | as today (`stats:read`); the new field only with `system:read` | as today |
| GET | `/api/v1/admin/stats/history` (unchanged, reused) | bearer | `stats:read`, per registry | as today |

- The actions reuse existing endpoints and nothing else:
  - Rescan one version: `POST /api/v1/verdicts/{r}/{n}/{v}/rescan`
    (`gates:exempt`).
  - Rescan a registry: `POST /api/v1/admin/verdicts/rescan` (`system:write`).
  - Exempt a version: `PUT /api/v1/admin/registries/{r}/policy/version/{p}/{v}/rules/{gate}`
    (`gates:exempt`).
- Each new `200` declares its `body = T` (`openapi_contract.rs`):
  `ScanQueuePage`, `FindingsPage`, `RegistryView`. Then `task dump-spec` and
  `task ui:generate`.
- `registry` is **optional** on `/admin/scans` and `/admin/findings`.
  - On `/admin/scans`, the verb is instance-wide, so `registry` is only a
    filter.
  - On `/admin/findings`, the server restricts the result to the
    registries where `authorize_control(hot, Some(r), …, FindingsRead)`
    answers `Ok`. A caller who holds the verb on none gets `403`, as
    `/admin/stats` does for `stats:read` (`stats.rs:21–35`). An explicit
    `registry` the caller may not read gets `200` with an empty page; it is
    not a `403`, because a `403` would confirm the registry exists.
- `/admin/scans?state=failed` reads rule 5, from the latest closed job per
  coordinate. `queued` and `scanning` read open jobs only
  (`completed_at IS NULL`), on the partial indexes of migration 051. The
  closed history is never scanned without a coordinate.

### 4.5 Mocks

ASCII wireframes. The `batlehub-design` skill is not installed in this
workspace, so there are no HTML mocks.

- Badges are the existing `components/ui/badge/Badge.vue` variants. The
  verdict colours reuse `VerdictPanel.vue`'s `stateVariant` mapping
  (`:81–91`): denied → `destructive`, quarantined → `copper`, warned →
  `outline`, allowed → `secondary`. The header and the panel therefore
  never disagree. The three scan states without a verdict use `outline`;
  the design system has no fifth hue for them.
- Icons are lucide icons, as in `VerdictPanel`, marked `aria-hidden`. The
  glyphs below stand for them in the wireframes.
- Each badge carries a text label, so meaning never depends on colour
  (§11).
- Wording reuses the existing `verdictPanel.*` keys wherever the meaning is
  the same, and new keys are listed in §6.4. In particular:
  - "Queue a rescan" stays as it is;
  - "Findings are withheld: reading them needs findings:read." stays;
  - "Held until {when}." stays.

**Status badge — every state**

```text
[○ not scanned]            outline
[◌ queued]                 outline     tooltip: "Waiting for a scan worker"
[◐ scanning]               outline     tooltip: "Being scanned — attempt 1 of 3"      (findings:read)
[✕ scan failed]            destructive tooltip: "3 attempts; last error: tar: unexpected EOF"  (findings:read)
[● scanned · allowed]      secondary   in lists: "as last recorded"
[● scanned · warned]       outline
[● scanned · quarantined]  copper      + "Held until 2026-11-12 09:00."
[● scanned · denied]       destructive
(no badge)                 registry without [security], or caller without quarantine:read
—                          status unavailable (scan_status_unavailable on the page), tooltip:
                           "Scan status could not be read; the list is otherwise current."

French, the longest: [● analysé · en quarantaine]. The list cell wraps under
the State badge rather than widening the column (catalog mock below).
```

This decides that the badge text, not its colour, carries the state, and
that an unscanned registry shows no badge rather than "not scanned".

**Package page — nominal, Security tab, admin**

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ npm-public / lodash        version [2.4.1 ▾]   [● scanned · warned]      │  ← sticky
├──────────────────────────────────────────────────────────────────────────┤
│  Overview   Versions   [Security]   Administration                       │
├──────────────────────────────────────────────────────────────────────────┤
│ Verdict      warned — CVE_GATE (1 high)          last scanned 2 h ago    │
│ Next rescan  verdict: 2026-11-17 14:02 (every 7 d)                       │
│              SBOM re-check: 2026-11-11 03:00 (registry-wide, daily)      │
│              [Queue a rescan]  [Exempt this version…]                    │
│                                                                          │
│ Findings (1)                                                             │
│  high  GHSA-35jh-r3h4-6jhm  osv   Command injection in template          │
│                                                                          │
│ Vulnerabilities  ……(the existing block, moved)                           │
│ SBOM             ……(the existing block, moved)                           │
└──────────────────────────────────────────────────────────────────────────┘
```

This decides that verdict, findings, vulnerabilities and SBOM share one tab.
Each part has its own verb:

- "Next rescan" renders only with `system:read`;
- [Queue a rescan] and [Exempt this version…] both render only with `gates:exempt` on the coordinate. That is
  the verb their endpoints check, and `VerdictPanel.vue:33` already uses it.

Overview keeps the README, plus a one-line verdict summary linking to
`?tab=security`. The SBOM and vulnerability blocks keep loading on mount, as
today, so `PackageDetailPage.test.ts` changes its selectors, not its
timing.

**Package page — Security tab, `user` role (quarantine:read, no findings:read)**

```text
│ Verdict      warned — CVE_GATE                                           │
│ Findings     Findings are withheld: reading them needs findings:read.    │
```

This decides that the reason codes stay visible, as they are today
(`findings_withheld`). The findings, dates and buttons are absent, not
greyed out.

**Package page — Security tab, scanning (poll running)**

```text
│ Verdict      [◐ scanning] — attempt 1 of 3, queued since 14:01           │
│              Updates automatically.                                      │
```

"of 3" is `[worker] max_attempts` as configured, 3 by default
(`scan_worker.rs:56`).

**Package page — Security tab, poll stopped**

```text
│ Verdict      [◌ queued] — waiting since 14:02                            │
│              Stopped updating after 10 minutes. [Refresh]                │
```

After three failed polls in a row, the same line reads "Stopped updating:
the server did not answer. [Refresh]".

**Package page — Security tab, registry without security**

```text
│ This registry does not scan packages. Versions are served as upstream    │
│ publishes them.                                                          │
```

**Package page — Security tab, error**

```text
│ ⚠ Could not load the verdict: 503 Service Unavailable. [Retry]           │
```

The real status line comes from the response. The other tabs stay usable.

**Catalog — nominal and empty** (the real columns, `PackageCatalog.vue:1077`)

```text
 State                     Package     Version   Size     Last fetched
 [Cached]                  lodash      2.4.1     312 KB   2 h ago
 [● scanned · allowed]
 [Cached]                  left-pad    1.3.0     4 KB     1 d ago
 [◌ queued]
 [Upstream]                internal    1.0.0     —        —
                                                           (no scan badge)

 Nothing matches that search                                        (empty)
 No package in this view matches. Upstream results appear here too when …
```

This decides that the scan badge sits **under** the existing State badge in
the same cell, with no new column, so the 390 px layout is unchanged. The
empty states keep the existing `catalog.emptyFilteredTitle` and
`catalog.emptyFilteredBody`.

**Admin → Security & Access → Scans — Queue tab**

```text
 Scans   [Queue (14)]  Failures (2)  Verdicts by state
 Registry [all ▾]   State [queued+scanning ▾]                  ↻ every 5 s
 Coordinate                     State        Trigger     Since     Worker    Attempts
 npm-public/react@19.1.0        ◐ scanning   first_seen  14:02:11  worker-2  1/3
 pypi/requests@2.33.0           ◌ queued     rescan      14:01:40  —         0/3
 …                                                        page 1/1
 Queue empty — every known version has a verdict.                   (empty)
```

**Admin → Security & Access → Scans — Failures tab**

```text
 Coordinate                  Attempts  Last error                          Since
 npm-public/huge@1.0.0       3/3       —  (lease expired; no error recorded)  yesterday  [Rescan]
 cargo/weird@0.1.0           3/3       tar: unexpected EOF at 1048576      2 d ago    [Rescan]
```

This decides that `last_error` is shown redacted (§7.3) and truncated at
200 chars, because it is the operator's only lead. A lease that simply
expired writes no `last_error` (`db/security.rs:563`), so that row shows
"— (lease expired; no error recorded)". [Rescan] needs `gates:exempt` on the
coordinate and is absent otherwise.

**Admin → Security & Access → Scans — error**

```text
 ⚠ Could not load the scan queue: 503 Service Unavailable. [Retry]
```

**Admin → Security & Access → Scans — Verdicts by state:** a table of
`GET /admin/verdicts?registry&state`. It needs one registry selected,
because that endpoint requires one. Empty state: "No version of npm-public
is quarantined."

**Admin → Security & Access → Findings — by reference**

```text
 Findings   by finding  [by reference]
 Registry [all ▾]  Severity [≥ high ▾]  Scanner [all ▾]  Reference [GHSA-…   ]
 Reference             Severity   Scanner  Versions affected
 GHSA-35jh-r3h4-6jhm   high       osv      3  ▸ npm-public/lodash@2.4.1 [warned] [Exempt…]
                                              ▸ npm-public/lodash@2.4.0 [warned]
                                              ▸ npm-mirror/lodash@2.4.1 [denied]
 MAL-2026-1234         critical   osv      1  ▸ npm-public/evil@0.0.1 [denied]
 No findings match these filters.                                   (empty)
```

This decides that the grouping is by advisory id across registries, that
each row names the verdict it produced, and that [Exempt…] opens the
existing exemption dialog. That dialog has the required reason and expiry,
taken from `AdminAuthorization.vue`.

**Admin → Security & Access → Findings — refusal (role without findings:read anywhere)**

```text
 Findings
 You cannot read findings on any registry. Findings name advisories that
 may be embargoed; they need the findings:read permission.
```

The server answers `403` (§4.4); the UI shows this text instead of a bare
error. Today this state is reachable only by an `admin` whose
`findings:read` was withdrawn, because every `/admin` route requires
`role === "admin"` (`router/index.ts:271`, `useAuth.ts:197`). Whether
registry-scoped delegates get these pages: they do not (decision 31).

**Admin → Observability → Registries → npm-public — layout: status strip + tabs**

The page has three parts:

- a **status strip**, always visible, in which each cell is a link to its tab;
- **four tabs** with `?tab=overview|activity|scan|config`, the same
  mechanism as the package page;
- the ⚠ markers of a suggestion, placed **beside the configuration key** it
  concerns.

Overview tab, nominal, with a monitoring link configured:

```text
 ← Registries   npm-public   npm · proxy          [Edit configuration]  [Monitoring ↗]
 ┌─────────────────┬───────────────┬────────────────┬──────────────────┬──────────┐
 │ upstream        │ cache         │ scan           │ queue            │ advice   │
 │ ● degraded      │ 92 % hit      │ rescan never   │ 14 · 2 failed    │ ⚠ 2      │
 │ this replica,   │               │                │                  │          │
 │ since 09:12     │               │                │                  │          │
 └─────────────────┴───────────────┴────────────────┴──────────────────┴──────────┘
  [Overview]   Activity   Scan   Configuration                    ?tab=overview

 Suggestions
  ⚠ The upstream is degraded and stale metadata is not served: clients
    get 502s. Consider serve_stale = true.          → Configuration · cache
  ⚠ Versions are scanned once and never rescanned. New advisories reach
    them only through the SBOM re-check.            → Configuration · security
 Activity      92 % hit over 24 h (−3 pts vs previous 24 h)  → Activity
 Scan          strict profile · 14 queued · 2 failed          → Scan
 Configuration npm · proxy · 1 upstream · 2 layers loaded      → Configuration
```

Activity tab:

```text
  Overview   [Activity]   Scan   Configuration          window [24 h | 7 d | 30 d]
 Live — this replica (pod batlehub-7d9f…), since 09:12
   upstream errors  31 %   threshold 25 %   ● degraded
   upstream latency 6 200 ms  threshold 5 s
 History (stats_history, per hour)
   hit rate  100 ┤▇▇▇▇▇▇▇▇▇▆▆▇▇▇▇▇▇▃▂▂▃▇▇▇
               0 ┼────────────────────────
   requests      ▁▂▃▅▇▇▆▅▅▆▇▇▆▅▃▂▁▁▁▂▃▅▆▇   hits ▇  misses ░
   listings      ▁▁▂▂▃▃▂▂▂▃▃▂▂▂▁▁▁▁▁▁▂▂▃▃
   storage       3.0 → 3.1 GiB
 Error and latency history are not kept by BatleHub.  [Monitoring ↗]
```

Scan tab:

```text
  Overview   Activity   [Scan]   Configuration
 security        on · profile strict
 verdict rescan  never — no rescan.interval_secs                         ⚠
 SBOM re-check   last 2026-11-10 03:00 · 4 812 artifacts · 7 findings
                 next 2026-11-11 03:00
 queue           14 queued · 1 scanning · 2 failed
   npm-public/react@19.1.0   ◐ scanning   worker-2   14:02
   npm-public/huge@1.0.0     ✕ failed     3/3  scanner osv: timed out
   …                                                     → Scans (filtered)
```

Configuration tab:

```text
  Overview   Activity   Scan   [Configuration]        effective · 2 layers loaded
 type            npm
 mode            proxy
 upstreams       https://registry.npmjs.org          ← redacted(); upstreams:read
 upstream auth   configured (bearer)                 ← never the value
 cache           metadata_ttl_secs 300
                 serve_stale       false                                 ⚠
                 artifact_ttl_secs —
 rules           deny_latest · release_age_gate (7 d)
 security        profile strict
                 rescan.interval_secs —                                  ⚠
 grants          3 subjects        namespaces  0
                                                    [Edit configuration]
```

This decides the following:

- The strip carries the five signals someone opens the page for, so a
  problem is visible from any tab.
- History shows only what `stats_history` keeps (hits, misses, listing
  reads, storage). Error and latency stay live values, and the Monitoring
  link takes over for their history.
- A suggestion is shown twice: in the Overview list, and as a ⚠ beside the
  key in Configuration.
- Credentials read as "configured (kind)".
- [Edit configuration] is a plain in-app link, without the ↗ that marks
  the external Monitoring link. It goes to `AdminConfigReload` with
  `?registry=npm-public`. That page, in its editable state, puts the caret
  on the first line of the matching `[[registries]]` block (`name =
  "npm-public"`) and scrolls the 18-row textarea to it.
  - While a reload is staged, the textarea is folded
    (`AdminConfigReload.vue:49–53`). The page then opens the staged diff,
    and the registry parameter is ignored with the notice "A reload is
    staged; review or discard it first."
  - On a read-only mount (`ConfigReadOnlyView`), the button reads "View
    configuration".
  - Without `config:write`, it is not rendered.
- Upstream URLs pass through `registry::http_client::redacted` (an
  upstream URL can carry a token, `http_client.rs:231`). They are shown only
  with `upstreams:read`, the verb RFC 0038 created because an upstream can
  name an internal host. Without it the row reads "1 upstream (hidden:
  upstreams:read)". The same applies to `index_url`, `search_url`,
  `broker_url` and the other URL fields that `project()` copies.

**Registry page — other states**

- **No `[stats] registry_dashboard_url`:** the [Monitoring ↗] buttons are
  absent. The Activity footnote reads "Error and latency history are not
  kept by BatleHub; set `[stats] registry_dashboard_url` to link your
  monitoring."
- **No `[registries.security]`:** the strip's scan and queue cells read
  "off". The Scan tab reads "This registry does not scan packages."
- **`[stats] history_enabled = false`:** the Activity history block reads
  "History is disabled (`[stats] history_enabled`)". Live values stay.
- **History empty** (new registry, no traffic): "No traffic recorded in
  this window."
- **Without `config:read`:** the Configuration tab is not rendered, and the
  strip and the other tabs stay.
- **Without `stats:read`:** the Activity tab is not rendered, and the
  strip's cache cell reads "—".
- **Unknown registry:** the API answers `404`. The SPA has no not-found
  page today (no catch-all route in `router/index.ts`), so this RFC adds
  one inside `AdminRegistry.vue`:

  ```text
   ← Registries
   No registry named `foo`.
   It is not in the loaded configuration. [Back to Registries]
  ```

- **Without `system:read`:** the API answers `403`. An admin always holds
  that verb, so behind the admin route guard this is unreachable today. It
  renders the house `403` state: "You do not have system:read."
- **Error loading history:** "⚠ Could not load activity: 503 Service
  Unavailable. [Retry]". The strip and the other tabs stay.

**Admin → Dashboard — monitoring link**

```text
 Dashboard                                              [Monitoring ↗]
 …(existing tiles unchanged)…
 Registry        State                      Hit rate   …
 npm-public →    Errors — see health        92 %          (existing table; name links to the registry page)
```

This decides two things:

- The global link uses its own key, `[stats] dashboard_url`. It is not the
  per-registry template with an "all" value, because each tool spells "all"
  differently (`All` and `$__all` in Grafana, something else in
  ClickStack).
- There is no new chip row. The existing per-registry table
  (`AdminDashboard.vue:290–301`) keeps its own State column and its own
  test (`recent_errors`). Only its registry name becomes a link to the
  registry page, so the Dashboard never shows two different "degraded"
  verdicts side by side.

---

## 5. Architecture

### 5.1 One function decides the status

```mermaid
flowchart TD
    A["coordinate"] --> S{"registry has #91;security#93;?"}
    S -->|no| X["absent"]
    S -->|yes| J{"open job?"}
    J -->|"leased_by set and leased_until > now"| SC["scanning"]
    J -->|"yes: unleased, expired or deferred"| Q["queued"]
    J -->|no| V{"verdict row?"}
    V -->|no| N["not_scanned"]
    V -->|"latest closed job exhausted, or SCANNER_ERROR"| F["failed"]
    V -->|"SCAN_PENDING"| Q
    V -->|other| D["scanned + verdict state"]
    X & SC & Q & N & F & D --> H{"hold_visibility"}
    H -->|Hidden| X2["field absent"]
    H -->|Reason| R["state + verdict"]
    H -->|Findings| R2["+ dates, attempts, last_error"]
```

The diagram shows that the visibility filter is applied **after**
derivation and to every outcome. So no branch can return a status to a
caller who may not learn the version is held.

- `crates/core/src/services/scan_status.rs` (new) holds
  `pub fn derive(security_on: bool, open: Option<&ScanJob>, last_closed: Option<&ScanJob>, verdict: Option<&Verdict>, now: DateTime<Utc>) -> Option<ScanStatus>`.
  It is pure and has no I/O, as `core` requires.
- The reads happen in a fixed order: **jobs first, then verdicts**. The
  worker writes the verdict before it closes the job
  (`scan_worker.rs:186–192`), so a job read as closed always comes with
  its fresh verdict. The other order can show a stale `failed` for one
  poll.
- `ScanStatus` and `ScanState` go in `entities/security.rs`, next to
  `VerdictState`.
- `last_error` also carries the prefix `defer` writes to anchor
  `deferred_since` (`db/security.rs:365`); `ScanJob` already parses it into
  its `deferred_since` field (`entities/security.rs:736`). The detail shows
  that field, never the raw prefix.
- `ScanQueue` (`ports/security.rs:71`) gains two methods:
  - `jobs_for(&self, coords: &[PackageId]) -> Vec<(Option<ScanJob>, Option<ScanJob>)>`,
    the batch for lists. For each coordinate it returns the open job and
    the latest closed one, across registries, in one statement;
  - `list(&self, registries: Option<&[String]>, state: JobState, limit: u64, offset: u64) -> Vec<ScanJob>`,
    for the Scans page. It follows the `limit` convention of the existing
    ports (`ports/security.rs:30`), not a new `Page` type.

  `ScanJob` gains `leased_by` and `last_error`. Both are already columns of
  `scan_jobs`; only the entity omits them today.
- `VerdictRepository` (`ports/security.rs:11`) gains:
  - `get_many(coords: &[PackageId])`, the batch, across registries;
  - `list_findings(&FindingsFilter, limit, offset)`, which reads
    `artifact_findings` joined on `artifact_verdicts`. `FindingsFilter`
    (new, in `entities/security.rs`) holds `registries`, `severity`,
    `scanner`, `reference_prefix` and `verdict`.

  Both have Postgres and in-memory implementations, as every port does.

**Invariant:** `scan_status` is reachable only through `derive` followed by
the visibility tier, both called from one helper in `handlers/security.rs`
(`scan_status_for`). Every handler calls that helper. The web DTO
`ScanStatusDto` has **private fields** (it keeps its `Serialize` derive),
and its only `pub fn` is `ScanStatusDto::filtered(status, visibility)`.
Nothing outside its module can build one. The compiler enforces **how**
the DTO is built, and it has no `Deserialize`, so it cannot arrive from
outside either. It does not enforce **which** tier a handler passes:
`HoldVisibility` is a plain enum (`handlers/security.rs:55`). That half
rests on `scan_status_for` being the only caller of `filtered`, and on
T1/T5, which test the tier each route actually produces.

### 5.2 Admin reads, filtered per registry

```mermaid
sequenceDiagram
    participant UI as "Admin UI"
    participant H as "GET /admin/findings"
    participant Z as "authz"
    participant DB as "VerdictRepository"
    UI->>H: "?severity=high (no registry)"
    H->>Z: "authorize_control(r, findings:read) for each configured r"
    Z-->>H: "#91;npm-public#93;"
    H->>DB: "list_findings(registries=#91;npm-public#93;, …)"
    DB-->>H: "page"
    H-->>UI: "200 page, or 403 if the list was empty"
```

The sequence shows that the registry list is computed **before** the query
and passed into it. The repository never returns a row the caller may not
read: the registry list is a ceiling bound into the query. Each returned
row is then checked once more against `resolution_path_for_coordinate`
resolved **without shadow**, so a namespace that seals (§4.3 of RFC 0015)
cuts the list exactly as it cuts the per-version GET. `authorize_control`
only sees the registry node (`chain.rs:480`), and a row the per-version
read would refuse must not reach the list. A page can therefore hold fewer
than `per_page` rows; the response carries `next_offset` so the UI keeps
paging. An empty registry list short-circuits to `403` without a query.

The list is built with `authorize_control` (`authz/chain.rs:453`) over the
**configured** registries only. It is not built with `authorize_grants`,
which fails open twice: it answers `Ok` for a registry with no grants
hierarchy (`chain.rs:225`), and it answers `Ok` under an active shadow
(`chain.rs:271`). The second is acceptable for one coordinate per request.
It is not acceptable for a whole registry's findings in one page. The cost
of this choice is that a package-tier `findings:read` grant does not open
the cross-registry list. Such a holder still reads findings one version at
a time, on the verdict GET. The list fails closed.

### 5.3 The registry view is built from an allow-list

```mermaid
graph LR
    HC["AppConfig.registries#91;name#93;<br/>(RegistryConfig, at load and in build_pending)"] --> RV["registries::project()"]
    ST["ProxyMetrics per registry"] --> RV
    SS["registry_scan_state"] --> RV
    SQ["ScanQueue counts"] --> RV
    RV --> DTO["RegistryView DTO<br/>fields copied by name"]
    HC -. "upstream_auth, keys, tokens" .-x DTO
```

The graph shows that `RegistryView` is filled field by field from
`RegistryConfig`. It is not a `serde` projection of it.

`HotConfig.registries` holds clients, not configs (`hot_config.rs:367`).
`crates/core` cannot name `RegistryConfig` either, because `batlehub-config`
depends on `core`. So `project()` lives in `crates/web`, which already
reads config types. It runs where the config is in hand: at startup
(beside `set_warnings`, `services/reload/mod.rs:544`), and in
`build_pending` (`services/reload/applier.rs:229–239`), which already
computes `new_config.warnings()` there. `PendingReload` gains
`new_registry_views`, and `apply` swaps it in next to the warnings
(`applier.rs:370`). The result is kept in a `RegistryViews` handle shaped
like `ConfigWarnings`, an `Arc<RwLock<…>>` replaced whole, so a request
reads a snapshot and never sees half a reload.

That snapshot holds **only the config part**: the allow-listed fields and
the two `[stats]` URLs. `ProxyMetrics`, `registry_scan_state` and the queue
counts change constantly. `get_registry_view` joins them with the snapshot
on each request; they are never frozen at reload time. A field added to
`RegistryConfig` tomorrow, secret or not, does not appear until someone
writes it into `project()`. Secrets reach the DTO only as the enum
`AuthPresence { None, Configured { kind } }`.

---

## 6. Detailed design

### 6.1 `crates/core`

- `entities/security.rs`:
  - `ScanState { NotScanned, Queued, Scanning, Failed, Scanned }`, with
    `serde(rename_all = "snake_case")`;
  - `ScanStatus { state, verdict: Option<VerdictState>, detail: Option<ScanDetail> }`;
  - `ScanDetail { last_scanned_at, attempts, max_attempts, queued_since, deferred_since, last_error }`.
    `queued_since` is the open job's `created_at`, and `deferred_since` the
    field `ScanJob` already parses. No lease-start time exists (a heartbeat
    moves `leased_until`), so `scanning` shows the attempt, not a start
    time. `leased_by` is not in it: it belongs to the `system:read` tier, on
    the Scans page only;
  - `ScanJob` gains `leased_by: Option<String>` and
    `last_error: Option<String>`;
  - `pub const EXHAUSTED_PREFIX: &str = "exhausted: "`, shared by the worker
    and `derive` (decision 34). `DEFERRED_SINCE` stays private to adapters.
- `ports/security.rs`: `ScanQueue` gains
  `complete_exhausted(&self, job_id, last_error: &str)`, with Postgres and
  in-memory implementations. `complete(job_id, worker)` takes no error
  text, so `close_exhausted` (`scan_worker.rs:843`) calls the new method in
  place of its `complete(job.id, None)` (`:898`), passing
  `EXHAUSTED_PREFIX` + the job's last error.
- `services/scan_status.rs`: `derive` (§5.1) and
  `next_rescan(verdict, policy, scan_state, sbom_interval: Option<Duration>) -> NextRescan { verdict_at: Option<…>, sbom_at: Option<…> }`.
  It takes the SBOM interval as a plain `Duration` because core cannot
  import `VulnerabilityScanConfig`. The verdict clock is already in core as
  `SecurityPolicy::rescan_interval` (`entities/security.rs:644`).
- `services/scan_status.rs` also holds
  `suggestions(degraded: bool, serve_stale: bool, security_on: bool, rescan_interval: Option<Duration>, failed: u64) -> Vec<Suggestion>`,
  the closed list of §4.2. Its inputs are plain values, so it stays in
  core.

### 6.2 `crates/adapters`

- `db/security.rs` gets the following queries, all parameterised `sqlx`
  queries with no string-built SQL:
  - `jobs_for`:
    `WITH c(r, n, v) AS (SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[]))`.
    The open job is served by `scan_jobs_open_coordinate_idx`. The latest
    closed job is a `DISTINCT ON (registry, package_name, version) … ORDER BY completed_at DESC`,
    on the index of decision 33.
  - `list` for jobs: open rows only for `queued`/`scanning`, and rule 5 for
    `failed`.
  - `get_many` for verdicts, on `artifact_verdicts`' primary key.
  - `list_findings`: `registry = ANY($1)`, severity, scanner and
    `reference LIKE $n || '%'` (escaped), with `LIMIT/OFFSET`. `severity` is
    `TEXT` (`050_artifact_findings.sql:14`), so the order is a `CASE` rank
    (critical → low), not the alphabetical order of the column. The query's
    index is decision 33's.
- The in-memory implementations mirror them for the web tests.

### 6.3 `crates/web`

- `handlers/security.rs`:
  - `scan_status_for(svc, identity, coords)` is the single entry point
    (§5.1);
  - `list_scans` and `list_findings` are the new admin handlers;
  - `get_verdict` adds `scan_status`, and `next_rescan` when the caller
    holds `system:read`.
- `front_office/explore/list.rs`: `ExploreEntryDto` gains
  `scan_status: Option<ScanStatusDto>`, filled after the page is built, in
  one batched call.
- `front_office/explore/detail.rs`: the same field, per version.
- `back_office/packages/list.rs`: the same field, per row.
- `back_office/registries.rs` (new): `get_registry_view` (snapshot + live join), the `RegistryView`
  allow-list DTO, `AuthPresence`, `project()` and the `RegistryViews` handle
  (§5.3).
- `scan_status_for` resolves the visibility tier once per (registry,
  identity) for a page (§4.2). It skips the batch when every registry is
  `Hidden`. It wraps the batch in `tokio::time::timeout(500 ms)`, outside
  the list's own `try_join!` (`explore/list.rs:346`). A timeout or an
  error drops the field and sets the page-level
  `scan_status_unavailable: true`, the same pattern as the existing
  `upstream_unavailable` (`list.rs:362`).
- `last_error` passes through a new
  `redact_text_urls(&str) -> String`, in `crates/adapters/src/registry/http_client.rs`
  beside `redacted` (`:235`), **before** truncation to 200 chars.
  Truncating first could cut a token mid-URL, and the remainder would no
  longer parse as one.
- `handlers/security.rs` `list_scans` filters rows per registry: a
  registry where the caller lacks `quarantine:read`
  (`authorize_control(hot, Some(r), …)`) contributes no row, and
  `last_error` needs `findings:read` there too. `system:read` alone must not
  name held versions or carry a finding's text (decision 15's reasoning,
  T16).
- `services/reload/{mod,applier}.rs`: `PendingReload.new_registry_views`,
  built in `build_pending`, swapped in `apply` (§5.3).
- `lib.rs`: routes and `utoipa` tags.

### 6.4 `ui`

- `PackageDetailPage.vue` is split into `components/package/tabs/{Overview,Versions,Security,Admin}Tab.vue`.
  The page keeps loading, the header and tab routing. `ReadmePanel`,
  `MovingRefsPanel` and the admin components move unchanged. `VerdictPanel`
  moves with three additions:
  - "Next rescan", a new key `verdictPanel.nextRescan`;
  - the [Exempt this version…] button, a new key `verdictPanel.exempt`;
  - the scan detail line, new keys `verdictPanel.scanning`,
    `verdictPanel.pollStopped` and `verdictPanel.pollFailed`.

  Its existing strings are kept. Tabs reuse `components/ui/tabs/`, and the
  admin pages sit inside the existing `SectionTabs.vue` strip.
- `components/package/ScanBadge.vue`: one component, used by the header, the
  catalog, `AdminPackages`, the Versions tab and the Scans page.
- New pages:
  - `pages/AdminScans.vue`, under the `adminSections.ts` group labelled
    "Security & Access";
  - `pages/AdminFindings.vue`, in the same group;
  - `pages/AdminRegistry.vue` and `pages/AdminRegistries.vue`, the list
    (reusing `/admin/stats` `per_registry`), both under Observability, with
    routes `/admin/observability/registries` and
    `/admin/observability/registries/:name`.

  The registry names in `AdminDashboard`'s table and `AdminHealth`'s table
  link to the registry page. A page with a tab strip inside the section's
  `SectionTabs` strip follows the same focus order as the package page:
  Tab moves from the section strip to the page strip, and arrow keys move
  within a strip.
- `AdminRegistry.vue` has a status strip always visible and four tabs synced
  to `?tab=`, and [Monitoring ↗] when `monitoring_url` is present. The
  history sparklines reuse the trend chart drawn inside `AdminDashboard.vue`
  from `adminStatsHistory` (tested by `AdminDashboardTrend.test.ts`). It is
  extracted into `components/admin/StatsTrendChart.vue`, so both pages use
  one component rather than a fork. `AdminDashboard.vue` gains [Monitoring ↗] and links
  the registry names of its existing table.
- `composables/usePoll.ts`: the bounded poll of §4.2, with its error
  backoff. It is a composable because two pages use it. It lands in
  phase 1 with the Security tab.
- `locales/en.json` and `fr.json`: every new string in both, gated by
  `catalogues.test.ts`.

### 6.5 Docs

- `docs/guide/security-gates.md` gains a section "Reading scan status",
  with the table of §4.2 and the visibility rule.
- The administration guide pages for Scans, Findings and Registry go in
  `docs/guide/`, because the reader runs the server.
- French twins for all of them.

### 6.6 `crates/config` and `deploy/`

- `StatsConfig` gains `dashboard_url: Option<String>` and
  `registry_dashboard_url: Option<String>`, both with
  `#[serde(default)]`. A new `validate_stats_urls()`, called from
  `validate()`, applies §4.3, parsing with `url::Url`.
  That crate is already in `Cargo.lock` (2.5.8, via `reqwest`); it becomes a
  direct dependency of `batlehub-config` (§8).
- `pub fn monitoring_url_for(template: &str, registry: &str) -> String`
  substitutes with `form_urlencoded`'s byte serializer. That is already a
  workspace dependency, so `/`, space, `&`, `#` and `"` in a registry name
  cannot escape the query value.
- An `http` URL adds a `ConfigWarning` from `warnings()`
  (`schema/mod.rs:701`), shown in the editor's warnings panel.
- `deploy/grafana/batlehub-production.json` gains a `registry` query
  variable (`label_values(batlehub_requests_total, registry)`, multi, with
  `All`, `allValue: ".*"` so "All" stays a match-all regex rather than the
  list of registries seen at load). Its `by (registry)` panels gain
  `{registry=~"$registry"}`, so the example URL of §4.1 filters with the
  dashboard the repository ships. The `uid` stays, and `version` is
  bumped. The upgrade notes say that re-importing overwrites local edits to
  that dashboard.
- `config.example.toml` shows both keys commented out.

### 6.7 Schema, worker and reload keys

- **Migration** (decision 33): a new numbered migration in
  `crates/adapters/migrations/` with its `mig!` entry in
  `embedded_migrator()`, holding the two `CREATE INDEX IF NOT EXISTS`.
- **Worker** (decision 34): `close_exhausted` writes the prefix through
  `complete_exhausted` (§6.1).
- **Reload keys** (decision 32): `crates/config/src/reload_keys.rs` gains
  `stats.dashboard_url` and `stats.registry_dashboard_url` as Hot.
  `docs/guide/hot-reload.md` is regenerated, as its gate
  `docs_table_matches_the_hot_reload_page` requires.

### 6.P Proof against a real client

The client is a real browser: Chrome in the CDE sidecar on port 9222,
driven over CDP by the rendered gate's harness (`task ui:dev:local`, API on
`localhost:8080`). The API is checked with `curl` against the same server.
The server runs `tests/heavy/config.quarantine.toml`. The suite is
`tests/heavy/quarantine.sh`, which already drives a real `npm` into a
quarantined registry through numbered steps (1–9 in its header). It gains a
step 10, `console`, that runs the observations below after the install
steps, so the queue holds real jobs that a real client caused. Each
observation names the phase (§16) that makes it pass, so every phase has
its own proof.

1. *(phase 1)* `npm install left-pad@1.3.0` through the quarantined registry, then
   `curl` `/api/v1/explore/packages?registry=npm-q` as the `user` token
   during the scan. The row carries `scan_status.state` `queued` or
   `scanning`, with no `detail`. After the worker completes, the same call
   returns `scanned` with `verdict: "allowed"`. The state moved without a
   reload in the browser. The CDP `Network` log shows
   `GET /api/v1/explore/packages/npm-q/left-pad` repeated at about 5 s,
   then stopping, and no `GET /verdicts/…` answered `404` in between.
2. *(phase 1)* **Refusal — anonymous.** The same list call without a token shows the
   row (the registry is public) with no `scan_status` key at all. `jq 'has("scan_status")'`
   prints `false`, the same as for a registry without `[security]`. The
   sqlx statement log for that request shows no `scan_jobs` or
   `artifact_verdicts` statement: the batch was skipped.
3. *(phase 3)* **Refusal — findings across registries.** A token holding `findings:read`
   on `npm-q` only calls `/api/v1/admin/findings` with no `registry`. Every
   returned row has `registry == "npm-q"`. The sqlx statement log for that
   request shows `registry = ANY($1)` bound to `{npm-q}`, so the other
   registry's rows were never read. The same token with `?registry=npm-b`
   gets `200` and `"items": []`.
4. *(phases 2, 3, 5)* **Refusal — user role.** The `user` token gets `403` on
   `/api/v1/admin/findings`, `/admin/scans` and `/admin/registries/npm-q`.
   In the browser, `/admin/security/findings` redirects (`requiresAdmin`),
   and the package page Security tab shows "Findings are withheld: reading
   them needs findings:read." *(phase 1)*
5. *(phase 5)* **Secret never leaves.** The config sets
   `upstream_auth.token = "SENTINEL-0042"` and an upstream URL
   `https://u:SENTINEL-0042@up.test/?token=SENTINEL-0042`. `GET /api/v1/admin/registries/npm-q`
   as admin returns `"upstream_auth": {"configured": true, "kind": "bearer"}`.
   `grep -c SENTINEL-0042` on the body and on the server log for the run
   prints `0`.
6. *(phase 2; the suggestion, phase 5)* **A failure is visible.** A
   version whose tarball the fake upstream truncates exhausts
   `max_attempts`. It appears under Scans → Failures with the worker's
   `last_error` text. A second case covers a `rescan` that exhausts on a
   version already `allowed`: it reads `failed`, not `scanned · allowed`
   (rule 5). From phase 5, the registry page shows the `scan_failures`
   suggestion. [Rescan] produces a `POST …/rescan` `202`,
   and the row moves to Queue.
7. *(phase 4)* **Next rescan.** With `rescan.interval_secs = 3600`, the verdict GET as
   admin returns `next_rescan.verdict_at == last_scanned_at + 3600 s`.
   Without the key it returns `null`, and the Security tab reads "never (no
   interval configured)".

8. *(phase 5)* **Monitoring link.** With
   `registry_dashboard_url = "https://grafana.test/d/x?var-registry={registry}"`,
   the registry page of `npm-q` renders an `<a href>` ending in
   `?var-registry=npm-q`, with `target="_blank"` and
   `rel="noopener noreferrer"` in the DOM read over CDP. Encoding of
   `/`, space, `&`, `#` and `"` is proven by the unit test (§14), since a
   registry name the config accepts cannot carry them. Clicking it opens a
   tab whose request carries no `Referer` (CDP `Network.requestWillBeSent`).
9. *(phase 5)* **Refusal — unsafe link.** A reload with
   `registry_dashboard_url = "javascript:alert(1)//{registry}"` is refused
   by `validate()`: `POST /admin/config/reload` answers `400`, quoting
   "`stats.registry_dashboard_url`: scheme `javascript` is not allowed". The
   running config is unchanged, and `/admin/registries/npm-q` still returns
   the old `monitoring_url`. A server started with that value exits non-zero
   with the same message.

**Deliberately untouched**, so reviewers do not go looking:

- `crates/core/src/services/rescan.rs`: the scheduler's logic does not
  change. The page computes the same expression the scheduler's query uses.
- `back_office/config.rs`: the editor, its staging and `content` keep their
  behaviour, including returning the raw file to `config:read` (decision 9).
- `crates/core/src/services/verdict.rs` `apply_override`: exemptions keep
  their semantics. Only a new entry point to the dialog is added.
- `cli/`: no change (non-goal).

---

## 7. Security

**Rating:** medium. Three new read endpoints aggregate data that used to be
reachable one coordinate at a time: findings across registries, the queue
across registries, and a registry's configuration. An aggregation mistake
discloses more than any single read did. There is no write surface.

### 7.1 Threat model

Assets:

- embargoed advisory ids and SOC case ids carried by findings (the reason
  `findings:read` is admin-only by default, `authz/translate.rs:280`);
- upstream credentials and internal hostnames in `RegistryConfig`;
- the existence of held versions, which anonymous callers must not learn
  (RFC 0018 §4.1).

Actors:

- an anonymous caller;
- a `user`;
- a registry-scoped delegate, who holds `findings:read` or
  `upstreams:read` on one registry. `system:read` cannot be registry-scoped
  (`schema/mod.rs:108`), so `/admin/scans` and `/admin/registries` answer
  this actor `403`. Of the new endpoints, a delegate reaches only
  `/admin/findings`, and only through the API, not the console pages
  (decision 31);
- an admin.

```mermaid
flowchart LR
    AN["anonymous / user"] -- "B1: public lists" --> L["explore + verdict GET"]
    DL["registry-scoped delegate"] -- "B2: /admin/findings only" --> AR["/admin/findings"]
    AD["admin (system:read)"] -- "B3" --> AR2["/admin/scans · /admin/registries"]
    AR2 --> P
    L --> F["scan_status_for<br/>(hold_visibility)"]
    AR --> P["authorize_control per configured registry<br/>before the query (fails closed)"]
    F & P --> DB["verdicts · findings · scan_jobs"]
    AR --> RV["RegistryViews<br/>(allow-list, built from AppConfig)"] --> HC["RegistryConfig (secrets)"]
```

| # | Threat (STRIDE) | Where | Mitigation | Residual risk |
| --- | --- | --- | --- | --- |
| T1 | Info disclosure: an anonymous caller learns a version is held via `scan_status: quarantined` | B1 | `scan_status_for` applies `hold_visibility`; `Hidden` → field absent | none known |
| T2 | Info disclosure: "absent" vs `not_scanned` lets anonymous callers tell scanned registries apart | B1 | `Hidden` returns the same absence as a registry without `[security]` | the registry's existence is already public on a public mirror; accepted, author |
| T3 | Info disclosure: a delegate with `findings:read` on A reads B's findings through the cross-registry list | B2 | registry list built with `authorize_control` over configured registries, bound into `registry = ANY($1)` | none known |
| T3b | Info disclosure: the list opens a registry the caller holds nothing on, because it has no grants hierarchy or sits under an active shadow (`authorize_grants` answers `Ok` for both, `chain.rs:225,271`) | B2 | `authorize_control`, not `authorize_grants`; only configured registries are iterated | a package-tier `findings:read` holder does not get the list; accepted, author (they keep the per-version GET) |
| T4 | Info disclosure: `upstream_auth`, a signing key (`ApkSigningConfig.private_key_pem`, the `signing`/`repo_signing`/`vsx_signing`/`nix_signing` blocks) in the registry view | B2 | `project()` allow-list; `AuthPresence` enum | a future field added to `project()` carelessly — caught by the sentinel test, seeded from every secret-bearing type |
| T4b | Info disclosure: an upstream URL carrying userinfo or `?token=`, or naming an internal host | B2 | every URL field through `http_client::redacted`; shown only with `upstreams:read` (RFC 0038) | none known |
| T5 | Info disclosure: `last_error` carries a URL with credentials or an internal path | B1 (`Findings` tier) / B2 | `last_error` returned only at `findings:read`/`system:read`; passed through `redact_text_urls` **before** truncation | a worker error naming an internal path is shown to admins; accepted, author |
| T6 | Info disclosure: `/admin/registries/{name}` enumerates registry names | B2 | `403` for a caller without `system:read` is decided before the lookup, so the `404` only ever answers a caller who may list the registries anyway | none known |
| T7 | Denial of service: an admin query scans `artifact_findings` unbounded | B2 | `per_page ≤ 200`; prefix-only `reference`; indexed columns | a delegate can still issue many pages; same as every admin list |
| T8 | Denial of service: many tabs polling | B1 | 5 s, only during transient states, stopped when hidden, after 10 min and after 3 failures; ×2 backoff to 60 s on errors; `Retry-After` honoured | none known |
| T8b | Denial of service: the status batch costs grant lookups and DB statements per anonymous list request | B1 | visibility resolved once per (registry, identity); the batch is skipped when every registry is `Hidden`; 500 ms timeout | none known |
| T9 | Elevation: the [Rescan]/[Exempt] buttons shown to a caller who lacks the verb | UI | the buttons call existing endpoints that check `gates:exempt`/`system:write` server-side; the UI hiding is cosmetic | none |
| T10 | Tampering: `reference` filter injected into SQL | B2 | bound parameter; `%`/`_` escaped for `LIKE` | none known |
| T11 | Elevation / XSS: a `javascript:` or `data:` monitoring URL rendered as `href` | config → UI | `validate()` accepts only `http`/`https`; the UI also refuses a non-http(s) `monitoring_url` before binding it | an operator with `config:write` can already change everything; this guards against a mistake or a hostile layer file |
| T12 | Tampering: a registry name breaks out of the URL query (`&`, `#`, `"`) | substitution | percent-encoded by `form_urlencoded`; the post-substitution parse of §4.3 | none known |
| T13 | Info disclosure: the monitoring host revealed to a `stats:read`-only delegate, or the console URL leaked to the monitoring tool | UI → monitoring | `monitoring_url` only at `system:read`; `rel="noopener noreferrer"` | the monitoring tool sees the registry name the operator chose to put in the URL; accepted, author |
| T14 | Info disclosure: a token placed in a monitoring URL's query string is shown to every `system:read` holder | config → UI | userinfo rejected by `validate_stats_urls`; `docs/guide/configuration.md` says not to put credentials there | a query-string token cannot be recognised; accepted, author; `system:read` is an admin verb |
| T16 | Info disclosure: `/admin/scans` names held versions and carries `last_error` to a `system:read` holder lacking `quarantine:read`/`findings:read` on that registry | B3 | rows filtered per registry by `quarantine:read`; `last_error` by `findings:read` | none known |
| T15 | Elevation: `scan_status_for` calls `VerdictService::current()`, so a page view enqueues scans and writes verdicts | B1 | `scan_status_for` reads stored rows only; a test asserts no `enqueue`/`upsert` during a list request | none known |

Each row has its negative test in §14.

### 7.2 Authentication and authorisation

| Surface | Verb | Enforced in |
| --- | --- | --- |
| `scan_status` coarse | `quarantine:read` (default `user`, `admin`) | `hold_visibility` via `scan_status_for` |
| `scan_status.detail` | `findings:read` (default `admin`) | same |
| `next_rescan` on verdict GET | `system:read` (instance tier) | `authorize_control(hot, None, …)` |
| `/admin/scans` | `system:read` (instance tier) | `require_verb(…, None, …)`; `403` otherwise |
| `/admin/findings` | `findings:read`, per configured registry | `authorize_control(hot, Some(r), …)` per registry, bound into the query; `403` when held on none |
| `/admin/registries/{n}` | `system:read` (instance tier); `config` block `config:read`; URL fields `upstreams:read` on `n` | `require_verb`, then per block |
| `monitoring_url` (both responses) | `system:read` (instance tier) | `authorize_control(hot, None, …)` |

No new verb (decision 10). The findings list uses `findings:read`, not the
`system:read` the draft proposed, because that verb already decides who may
see a finding on one version. A cross-registry list gated by anything weaker
would turn `system:read` into a findings grant (decision 15). An absent
grant means an absent field, an absent block, a `403`, or, for an
explicit `registry` the caller may not read, an empty page (decision 35).
It never means a partial row. Authorisation is checked **before** any query, never after.

The `user` role cannot reach the admin pages in the console: every `/admin`
route checks `role === "admin"` (`router/index.ts:147,271`). The server's
checks above are what hold. The route guard is navigation, not
enforcement.

### 7.3 Input handling and injection

The new inputs are the query parameters of §4.4:

- `registry` is matched against `HotConfig.registries` keys and never reaches
  SQL unvalidated;
- `severity`, `state` and `verdict` are parsed into enums;
- `reference` is at most 128 chars, bound as a parameter, with `LIKE`
  metacharacters escaped;
- `page` and `per_page` are integers capped at 200.

No path or archive is parsed. `{name}` on `/admin/registries/{name}` is a
map lookup. Two strings are parsed as URLs, both trusted input from the
operator's config: the monitoring templates (§4.3) and the URL fields
copied into `RegistryView` (redacted, T4b). One free-text field is
scanned for URLs: `last_error`, through `redact_text_urls` (T5).

### 7.4 Secrets and cryptography

No new secret and no cryptography. Credentials appear as `AuthPresence`,
never as a value, a length or a hash prefix. The registry view shows less
than the raw `content` endpoint, but that endpoint still serves the whole
file, secrets included, to every `config:read` holder, and [Edit
configuration] links to it. This RFC does not close that (decision 9). It
is the remaining half of motivation 7 and a residual risk the author
accepts.

### 7.5 Abuse and resource exhaustion

- The list batch adds two indexed statements per page, `jobs_for` and
  `get_many`, each over ≤ 200 coordinates and spanning every registry on
  the page. It is skipped entirely when every registry on the page is
  `Hidden` for the caller.
- The admin lists are paginated at ≤ 200.
- The poll is bounded (T8).
- On a default config, an anonymous caller makes the server run only the
  existing list queries. The batch runs only where the operator granted
  `quarantine:read` to anonymous callers, or where the registry has no
  `[security]`, in which case there is nothing to batch.

### 7.6 Existing defences that still hold

- `hold_visibility` and the `findings_withheld` behaviour of the verdict GET.
- `require_verb` on every back-office handler.
- `gates:exempt` on per-version rescan and on exemptions; `system:write` on
  bulk rescan.
- The exemption's mandatory `reason` and `exempt_until`
  (`policy.rs:542–551`).

---

## 8. Supply chain

No new package, image, action or tool enters `Cargo.lock` or
`pnpm-lock.yaml`; only `batlehub-config`'s own dependency list in the lock
grows by two entries already present. One existing crate becomes a direct dependency of one more
member: `url` 2.5.8 (MIT/Apache-2.0, already in the tree through `reqwest`)
in `batlehub-config`, for §4.3's parse. `form_urlencoded` is already a
workspace dependency (`Cargo.toml:173`) and becomes a direct dependency of
`batlehub-config` too. The backend also reuses `sqlx`, `chrono` and
`utoipa`. The Grafana dashboard JSON is an example file, not a runtime
dependency. The UI reuses Vue, the router and the house components. Tabs
reuse `ui/src/components/ui/tabs/`, which exists and is tested. The repo's
audit, `task security` (this repo's name for `task audit`), is unchanged.

---

## 9. Data and privacy

No new table or column. Two things are written that were not before: two
**indexes** (decision 33), and the `exhausted:` prefix on `last_error`
(decision 34). That prefix is the worker's own error text, already stored
today without it. Everything shown is read from tables that exist
(`artifact_verdicts`, `artifact_findings`, `scan_jobs`,
`registry_scan_state`, `stats_history`) or from the loaded config. The only
personal datum newly *shown* is `leased_by`, the worker id: a hostname or
pod name, not a person. It is visible at `system:read` only, on the admin
endpoints, and **not** in `ScanDetail`, the `findings:read` tier. The badge
tooltip says "Being scanned — attempt 1 of 3" and does not name the worker. No metric label or log line is added
that carries a coordinate or an identity.

---

## 10. Operability

### 10.1 Observability

- **Metrics:** none added. The existing `batlehub_scan_jobs_queued`,
  `batlehub_scan_jobs_leased`, `batlehub_scan_jobs_expired_total` and
  `batlehub_upstream_health_degraded` are what the pages show in detail; the
  numbers must agree. A test asserts the queue page's count equals the gauge
  for the same in-memory queue.
- **Logs and audit trail:**
  - The three new handlers are reads and write no audit event, as
    `/admin/verdicts` writes none today.
  - The actions they link to (rescan, exempt) already write theirs.
  - The access log records the request as for every route.
- **Alerts:** none added. The pages are where an operator looks after an
  existing alert. The Helm chart's deferred-jobs alert (RFC 0041) is
  unchanged.

### 10.2 Failure modes

| Failure | Detected by | Behaviour | Recovery |
| --- | --- | --- | --- |
| Monitoring tool down or URL wrong | the user's click | nothing in BatleHub depends on it; the link opens a tab that fails | the operator fixes the key; a reload applies it (decision 32) |
| `stats_history` disabled or empty | `history_enabled = false`, or no rows | the Activity tab says so (§4.5); live values stay | — |
| The status batch fails or exceeds 500 ms while the list's own queries succeed | the `timeout` or an error, in `scan_status_for` | **closed for disclosure:** `scan_status` omitted from every row, `scan_status_unavailable: true` on the page, badges show "—" with their tooltip; the list is served | automatic on next request |
| Postgres down entirely | the list's own `try_join!` (`explore/list.rs:346`) | the list fails as it does today; this RFC adds no new failure | as today |
| A slow database while tabs poll | poll errors | backoff ×2 to 60 s, stop after 3 failures, `Retry-After` honoured | [Refresh] |
| Verdict and job read at different instants (race) | — | prevented by reading jobs before verdicts (§5.1); the worker writes the verdict before closing the job | none needed; no state is written |
| A job deferred on an advisory database outage | `leased_by IS NULL`, `leased_until` future | reads `queued` (rule 3), not `scanning`, for up to an hour | the existing deferral |
| An exhausted job no worker sweeps (workers scaled to 0, or the registry outside every worker's `registries`) | the job stays open with `attempts >= max_attempts` | reads `queued`; the Scans queue row shows "attempts 3/3, waiting for a worker sweep" | a worker sweep; the Scans page makes the stall visible |
| Several replicas | — | `upstream_*` live values differ per pod; the strip labels them "this replica, since …" | the monitoring link for the estate-wide view |
| Worker dead with leases held | `leased_until` passes | the job reads `queued` again (rule 3), not `scanning` | the existing expiry |
| `HotConfig` reloaded mid-request | — | the view is built from one snapshot (`Arc` clone before `await`) | — |
| `registry_scan_state` row missing (SBOM pass never ran) | `None` | `sbom_recheck_at` shows "not yet run" | the next pass |

The direction is closed for disclosure: when in doubt, a field is absent.
It is open for availability: a list still renders without its badges.
Withholding a badge never blocks a download, so failing open for
availability here grants nothing.

### 10.3 Performance and capacity

- **Anonymous explore list:** 0 statements warm today
  (`db_calls.budget.json`, `api-explore-list`). With this RFC it is still 0
  on a default config, because the batch is skipped when every registry is
  `Hidden`.
- **Authenticated list pages:** today `api-packages-admin` is cold 2 / warm
  2 and `api-explore-detail` cold 6 / warm 6 (`db_calls.budget.json`);
  `api-explore-list` has no `user` arm yet, so phase 1 adds one. Each gains
  +2 statements (`jobs_for`, `get_many`), whatever the number of
  registries on the page, since both span registries.
- `tests/heavy/config.db-calls.toml` has no `[registries.security]` today,
  so it would measure nothing. Phase 1 adds a security-enabled registry
  there, measures the routes above as anonymous **and** as a `user`, and
  sets the budgets from those numbers.
- The page's first paint is unchanged in call count: the SBOM and
  vulnerability blocks keep loading on mount (§4.5). The tabs reduce
  scrolling, not requests.
- **Storage:** `scan_jobs` already grows without bound; closed rows are
  never deleted. This RFC does not add to that, but its reads must not walk
  the history. `queued`/`scanning` read open rows on the partial indexes,
  and `failed` and `jobs_for` use the index of decision 33.

---

## 11. Other concerns

- **Accessibility:**
  - The tabs use `role="tablist"`/`tab`/`tabpanel` with arrow-key
    navigation, `aria-selected` and focus moved to the panel heading.
  - Each badge has visible text, so status never rests on colour alone, and
    `aria-label` repeats the tooltip detail. Its lucide icon is
    `aria-hidden`.
  - On the admin pages, the page's own tab strip sits inside the section's
    `SectionTabs` strip. Tab moves between the two strips, and arrow keys
    move within one (§6.4).
  - The poll announces a state change once through an `aria-live="polite"`
    region, not every 5 s.
  - Contrast follows the existing tokens, and `task docs:design:rendered`
    checks it at four viewports.
- **Internationalisation:** every new string goes in `en.json` and
  `fr.json`; `catalogues.test.ts` fails on a missing key. Dates use the
  existing locale formatter. Existing `verdictPanel.*` and `catalog.*` keys
  are reused where the meaning is unchanged (§4.5). The French badge text
  is the longest ("analysé · en quarantaine"), and it wraps in its cell
  rather than widening the column.
- **Compliance:** this supports the RFC 0036 CRA baseline (showing
  vulnerability handling state) without changing a control.
- **Licensing:** unchanged.
- **Documentation:** §6.5 pages, written in the same change as each phase.
  `docs/guide/configuration.md` documents the two `[stats]` keys with a
  Grafana and a ClickStack example, and says not to put credentials in
  them.

---

## 12. Alternatives considered

| Alternative | Why rejected |
| --- | --- |
| A `status` column on `scan_jobs` | It duplicates what `leased_until` and `completed_at` already say, and a column that can disagree with them would need its own consistency logic. |
| Gate the findings list with `system:read` (the draft's answer) | `system:read` would become a cross-registry findings grant, wider than `findings:read`, which RFC 0018 kept for the administrator. |
| Serialise `RegistryConfig` with known secret fields masked (deny-list) | It fails open: the next secret field added to `RegistryConfig` ships visible until someone remembers the mask. |
| Show the raw `[[registries]]` TOML slice | It is the same leak as the `content` endpoint, and the merged config has no per-layer text to slice. |
| Per-finding triage (accept / false positive) | New persisted state, and a second override path that a `findings:read` holder could use to silence what `gates:exempt` exists to control. |
| Serve-before-scan bypass | It defeats quarantine. The exemption already covers "I accept this version". |
| SSE for live status | A connection per open tab, plus a broadcast path from the worker, for two transient states a bounded poll covers. |
| Collapse the package page's sections instead of tabs | It still loads one long document. A collapsed section cannot be linked, and decision 4 settled on a link to "the security of this package". |
| A separate `/packages/:r/:n/security` page | It is a second route with its own loading and header. The tab gives the same link (`?tab=security`) without them. |
| Extend `/admin/verdicts` instead of adding `/admin/scans` | `/admin/verdicts` lists verdict rows and requires one `registry`. The queue (leases, attempts, deferrals) lives in `scan_jobs`, which that endpoint never reads, so it would change shape for its existing caller. The Scans page uses both endpoints. |
| Build the findings registry list with `authorize_grants` (as `/admin/stats` does with its own `configured` check) | It answers `Ok` under an active shadow (`chain.rs:271`), which is acceptable for one coordinate per request and not for a registry's whole findings list in one page. |
| Do nothing; document the SQL | It leaves motivations 1 and 2 untouched (readers, not operators), and each admin keeps writing the same queries. |
| BatleHub queries Prometheus and draws error/latency history itself | Prometheus has no grants, so a delegate scoped to one registry would read every registry. Forcing `registry="…"` into user PromQL means a PromQL rewriter to secure. It also adds a new upstream with credentials, timeouts and a failure mode, duplicates Grafana, and ties BatleHub to one backend when operators run Grafana, ClickStack or others. |
| Add `upstream_errors` and latency columns to `stats_history` | A migration and new persisted data (decision 2), for a history the metrics already hold per registry. |
| One link template with an "all" value for the dashboard | Each tool spells "all" differently (Grafana `All`/`$__all`, ClickStack a missing filter), so BatleHub would have to know the tool. Two keys let the operator write both. |
| Ship a default URL pointing at the bundled Grafana dashboard | BatleHub does not know where, or whether, the operator runs Grafana. A wrong default is a dead button on every install. |
| Three RFCs | The status function is shared by every surface. Splitting would argue it three times. Phases keep the parts independently shippable. |

---

## 13. Rollout and compatibility

- **Default behaviour:** without `[registries.security]`, nothing visible
  changes except the tabs and the Registry page; every list is the same.
- **API compatibility:** additive optional fields; the generated client is
  regenerated; old clients ignore the key. Checked while writing: there is
  no `deny_unknown_fields` anywhere in `cli/src`, the CLI reads explore
  detail as `serde_json::Value`, and `VerdictView`
  (`cli/src/api/security.rs:37`) is a plain derive. The verdict GET keeps
  its `404` with no verdict row, which `batlehub-cli wait` reads as "no
  verdict" (`cli/src/cli/security.rs:328`).
- **Migration:** no data and no column. Decision 33 adds two **indexes**
  (on `artifact_findings` and on closed `scan_jobs`) in a new numbered
  migration with its `mig!` entry. An index is reversible with `DROP INDEX`
  and changes no row.
- **Deep links:**
  - The old package URL still works and opens Overview.
  - Links into the versions index (`?q=`, `?page=`) open Versions (§4.2).
  - The page has no section anchors today besides `subject-heading`,
    `versions-heading` and `admin-heading` (`PackageDetailPage.vue:1283,
    1596, 1932`). Those three map to Overview, Versions and Administration.
- **Config:** two optional keys. `StatsConfig` is
  `#[serde(deny_unknown_fields)]` (`schema/mod.rs:277`), so **an older
  release refuses to load a config that sets either key**. Rolling back
  past phase 5 means removing them first. The upgrade notes say so; it is
  the same rule every new `[stats]` key has followed.
- **Rollback:** revert the release, after removing the two `[stats]` keys
  if they were set. Two things stay behind, both harmless to an older
  release:
  - the two indexes, which it never reads and which `DROP INDEX` removes;
  - `last_error` values prefixed `exhausted:` on closed jobs. The older
    release reads `last_error` only for its `deferred since ` prefix
    (`db/security.rs:365`), and only on open jobs.

  No §7 risk reopens.
- **Feature flag:** none. Each phase is visible when it ships.

---

## 14. Test plan

- **Unit** (`crates/core/src/services/scan_status.rs`): every row of the
  §4.2 table, including:
  - rule 2 over rule 5 (rescan of a failed version);
  - a **deferred** job (`leased_by` null, `leased_until` an hour ahead)
    reads `queued`, not `scanning`;
  - an exhausted `rescan` job on a version whose verdict is `allowed` reads
    `failed`;
  - a failure followed by a successful later job reads `scanned`;
  - a verdict with `SCAN_PENDING` and no job;
  - a lease expired by one second;
  - `next_rescan` with an absent interval, a past date, and SBOM disabled;
  - each suggestion fires and does not fire at its boundary.
- **Unit** (`crates/web/src/handlers/back_office/registries.rs`):
  `project()` maps every allow-listed field and redacts every URL field.
- **Unit**: `redact_text_urls` on a `last_error` holding
  `https://u:p@h/?token=x` and a 300-char tail. The result has no `p` or
  `x`, and truncation happens after redaction.
- **Integration** (`crates/web/tests/scan_visibility.rs`, new): list, detail
  and admin list carry `scan_status`; two batched statements per page
  whatever the number of registries; the queue count equals the gauge; a
  failing batch sets `scan_status_unavailable` and still serves the list;
  `scan_status_for` makes no `enqueue`/`upsert` (T15); the verdict GET
  still answers `404` with no verdict row; `batlehub-cli wait` exits as
  today against it.
- **Integration** (`crates/web/tests/admin_findings.rs`, `admin_registry_view.rs`,
  new): filters, grouping, pagination cap, `400` on a bad enum.
- **UI** (`*.test.ts` beside each new page and component): every mock state
  of §4.5; tab routing and the fallback; the poll stops when hidden and
  after 10 min; buttons absent without the verb.
- **Proof:** §6.P, step 10 `console` of `tests/heavy/quarantine.sh`.
- **Security**, one negative test per §7.1 row:
  - T1 `anonymous_list_has_no_scan_status_on_quarantined_registry`;
  - T2 `hidden_and_no_security_are_indistinguishable` (byte-equal JSON
    rows);
  - T3 `findings_list_without_registry_is_scoped_to_granted_registries`,
    `findings_list_for_ungranted_registry_is_empty_200` and
    `findings_list_held_nowhere_is_403`;
  - T3b `registry_delegate_of_a_gets_nothing_from_b_without_hierarchy`
    (an instance-tier `findings:read` holder does get B, by design) and
    `findings_list_ignores_active_shadow`;
  - T3c `findings_list_respects_namespace_seal`: a registry-tier
    `findings:read` holder sees no row from a sealed namespace, exactly as
    the per-version GET refuses it;
  - T1 also `anonymous_list_under_shadow_has_no_scan_status`;
  - T4 `registry_view_never_contains_sentinel`: every secret-typed field of
    `RegistryConfig` is set to a sentinel and the body is grepped. The list
    is seeded from every secret-bearing type: `UpstreamAuthConfig`,
    `ApkSigningConfig.private_key_pem` and the
    `signing`/`repo_signing`/`vsx_signing`/`nix_signing` blocks;
  - T4b `registry_view_redacts_upstream_urls` (userinfo and `?token=`
    sentinel) and `registry_view_hides_urls_without_upstreams_read`;
  - T5 `scan_detail_withheld_without_findings_read` and
    `last_error_with_query_token_is_redacted`;
  - T6 `registry_view_without_system_read_is_403_before_lookup`;
  - T7 `admin_lists_reject_per_page_over_200`;
  - T8 the UI poll test above, plus the backoff and the stop after 3
    failures;
  - T8b `anonymous_list_runs_no_status_statement` and the `db_calls`
    budget;
  - T9 the existing `a_rescan_needs_gates_exempt_and_queues_one_job`
    (`crates/web/tests/security_registry.rs:1288`), re-run unchanged, and a
    new `bulk_rescan_without_system_write_is_403` (the existing bulk test at
    `:1179` only covers the allowed side);
  - T16 `scans_list_omits_registry_without_quarantine_read`;
  - T10 `findings_reference_filter_escapes_like_metacharacters` (`%` matches
    literally).
- **Config** (`crates/config/src/schema/tests.rs`): each §4.3 rejection
  (`javascript:`, `data:`, empty, `{` in `dashboard_url`, `{other}`,
  missing `{registry}`, `{registry}` in the host, userinfo), plus an `http`
  URL producing a `ConfigWarning`. `monitoring_url_for` with `a/b c&d#e"f`
  round-trips through `Url::parse` and its query value decodes back to the
  name.
- **Security**, the new rows:
  - T11 `monitoring_url_rejects_non_http_schemes`, plus a UI test that a
    `javascript:` value forced into the response is not bound;
  - T12 `monitoring_url_encodes_query_metacharacters`;
  - T13 `monitoring_url_absent_without_system_read`, plus a UI test for
    `rel`;
  - T14 `stats_url_with_userinfo_is_rejected`;
  - T15 above, in `scan_visibility.rs`.
- **Gates:**
  - `openapi_contract.rs`: each new `200` has a body;
  - `authz_matrix.rs`: each new route joins the admin inventory **in the
    phase that adds it** (2, 3, 5), or that phase is red;
  - `profile_route_coverage.rs`: each route is added, in its phase, to
    `perf/profile_routes.txt`. The explore list is `skip: no arm yet`
    there (`:137`), so phase 1 adds its soak arm, which carries the batch
    under load;
  - `ui:lint`, `ui:build`, `task docs:design:rendered`.
- **Audit:** `task security` unchanged and green.
- **Existing suites** that must pass unchanged: `quarantine.sh` (steps
  1–9), the verdict GET tests in `crates/web/tests/` (the
  `findings_withheld` behaviour), `tests/heavy/db_calls.sh` on every route
  this RFC does not touch.
- **Existing suites that change:** `PackageDetailPage.test.ts` (selectors
  move into tabs; timing is unchanged, §4.5), `db_calls.budget.json` (the
  authenticated list routes, §10.3).

---

## 15. Decisions and open questions

### Resolved

| # | Question | Decision |
| --- | --- | --- |
| 1 | One RFC or several? | **One RFC, phased; write-adjacent parts last.** The status function is shared, and the read phases can be accepted without waiting on the rest. |
| 2 | UI only, or new endpoints too? | **Read endpoints allowed; no new persisted data.** Everything is computable from existing tables. |
| 3 | What does "propose changes" to a registry mean? | **Computed suggestions + a link to the existing editor.** A four-eyes proposal workflow is a non-goal: new state, new approval model. |
| 4 | Long package page: tabs, collapsing, or a separate page? | **Tabs, linkable with `?tab=`, sticky header with the badge.** A link to "the security of this package" is the common need. |
| 5 | Who sees the scan indicator? | **Coarse state to every caller allowed to learn a version is held; detail to `findings:read`; next-rescan and queue to `system:read`.** Refined from the draft's "every reader" by decision 15's finding: `hold_visibility` already defines this, and anonymous callers must not learn of a hold. |
| 6 | How are the states derived? | **Computed server-side by one function, five states, §4.2 order.** `scan_jobs` has no status column, and the UI must not recombine fields. |
| 7 | What is the "bypass"? | **The existing gate exemption plus "rescan now"; no serve-before-scan.** Quarantine exists to prevent exactly that. |
| 8 | What does the findings view look like? | **A paginated `GET /admin/findings`, by finding or by reference; no per-finding triage.** Triage would be new state and a second override path. |
| 9 | Should the raw `content` endpoint also be restricted? | **No, out of scope; the new view shows less than it does.** Changing what `config:read` returns breaks the editor. |
| 10 | Is a new verb needed? | **No.** Every surface maps to an existing verb (§7.2). |
| 11 | Which next-rescan dates are shown? | **Both clocks, labelled; "never" when unconfigured; at `system:read`.** They answer different questions. |
| 12 | What status do lists carry? | **The most recently cached version, batched; per version in the Versions tab.** No N+1. |
| 13 | What goes on the registry page? | **Allow-listed effective config, health, scan freshness, three suggestions, an editor link; no provenance, no configurable thresholds.** `load_layered` keeps no provenance. |
| 14 | Approver = author in config staging? | **Out of scope, recorded as a non-goal.** It deserves its own change. |
| 15 | Which verb gates the cross-registry findings list? | **`findings:read`, per registry — not `system:read` as the grilling settled.** Found while writing §7: `findings:read` is the verb RFC 0018 created for exactly this disclosure. Gating the list by the weaker verb would widen it. |
| 16 | Is the config shown via an allow-list or a deny-list? | **Allow-list DTO, `AuthPresence` for credentials, sentinel test.** It fails closed on new fields. |
| 17 | Is per-layer provenance shown? | **No; "N layers loaded".** It would require rewriting the merge. |
| 18 | Admin lists across registries? | **`registry` optional; filtered to granted registries before the query.** It avoids enumeration and an N-request UI. *Revised by decision 35:* held nowhere is `403`, and only an explicit registry the caller may not read is `200` empty. |
| 19 | Where are suggestions computed? | **Server-side, closed list of three.** Testable in Rust; the CLI can reuse them. |
| 20 | How does live update work? | **Bounded 5 s poll during transient states, stopped when hidden or after 10 min.** No SSE. |
| 21 | Is the CLI or TUI in scope? | **No.** The field reaches the CLI for free. |
| 22 | In what order are the phases? | **§16.** Each security control ships with its surface. |
| 23 | Which mocks? | **ASCII wireframes, every state.** `batlehub-design` is not installed. |
| 24 | Registry page layout? | **A status strip always visible, then tabs `?tab=overview\|activity\|scan\|config`, with a ⚠ beside the configuration key a suggestion targets** (chosen over a single long column and over two side-by-side columns). It keeps problems visible from any tab without recreating the long page; same mechanism as the package page. |
| 25 | Keep error/latency history in BatleHub? | **No.** It already exists in Prometheus (`batlehub_upstream_errors_total`, `batlehub_upstream_request_duration_seconds`, per registry), and decision 2 forbids new persisted data. History drawn in the page is what `stats_history` keeps. |
| 26 | Query Prometheus from BatleHub, or link out? | **Link out, through an operator-written URL template; never query.** Querying would bypass grants (§12). The link costs a validated config string. |
| 27 | Make the bundled Grafana dashboard filterable? | **Yes, a `registry` variable in `deploy/grafana/batlehub-production.json`.** Without it, the documented example URL would filter nothing. |
| 28 | Per registry only, or the admin dashboard too? | **Both, as two keys: `dashboard_url` (no placeholder) and `registry_dashboard_url` (`{registry}`).** Asked by the author, against the draft's "per registry only". The tool (Grafana, ClickStack, …) is the operator's choice, so BatleHub cannot build an "all" value and does not try. |
| 29 | Should a caller without `findings:read` see "Findings withheld", or nothing? | **"Findings withheld", naming the verb, as `findings_withheld: true` already does.** It applies only to a caller who holds `quarantine:read`; the verdict state already says something was found, and naming the verb tells a developer what to ask for. A caller without `quarantine:read` sees nothing at all (§4.2). |
| 30 | Should the Registries list replace `AdminHealth`'s per-registry table or sit beside it? | **Beside it, linked both ways, in phase 5.** It keeps `AdminHealth`'s cache-clear action where operators know it; merging is a follow-up once both are in use. |

### Resolved by the final review (2026-11-10)

Six cold readers (user experience, security, resilience, code quality,
stability, honesty) read the draft against the code. Their findings that
change no decision the author made are in the text, recorded here so the
change is not silent:

| # | Finding | Decision |
| --- | --- | --- |
| 35 | The obvious per-registry filter (`authorize_grants`) answers `Ok` for a registry without grants and under an active shadow (`chain.rs:225,271`), and an empty `200` for a caller holding the verb nowhere widens an admin surface to every bearer (security) | **`authorize_control` over configured registries; held nowhere → `403`; explicit unreadable registry → `200` empty.** Revises decision 18. |
| 36 | `system:read` cannot be granted per registry (`schema/mod.rs:108`) (security) | **`system:read` is checked at the instance tier everywhere it appears; the "per registry" wording is gone.** Decisions 5 and 11 keep their meaning: the verb is the same, only its tier was misstated. |
| 37 | A deferred job has `leased_until` in the future and no worker; rule 2 called it `scanning` (resilience) | **Rule 2 requires `leased_by IS NOT NULL`.** |
| 38 | An exhausted `rescan`/`backfill` writes no `SCANNER_ERROR` (`scan_worker.rs:850`), so a failed rescan read `scanned · allowed` (honesty, resilience) | **Rule 5 reads the latest closed job.** How "exhausted" is read is decision 34. |
| 39 | `RegistryConfig` is not in `HotConfig`, and `core` cannot import config types (code quality) | **`project()` and `RegistryView` live in `crates/web`, rebuilt on each applied reload beside `ConfigWarnings`; core functions take plain values.** |
| 40 | The registry view showed upstream URLs, which can carry a token and name internal hosts (security) | **Every URL field through `redacted()`, shown only with `upstreams:read`.** |
| 41 | `hold_visibility` per row, and the batch on the anonymous list, would turn a 0-statement warm route into one that hits Postgres (security, stability) | **Visibility once per (registry, identity); batch skipped when all `Hidden`; 500 ms timeout; `scan_status_unavailable` on failure.** |
| 42 | The verdict GET answers `404` with no verdict row, and `batlehub-cli wait` depends on it (stability) | **The `404` stays; the Security tab polls the explore detail.** |
| 43 | The verdict GET re-evaluates and writes through `current()`; a list reads stored rows (stability) | **Lists show "as last recorded"; `scan_status_for` never calls `current()` (T15).** |
| 44 | Two definitions of "degraded" side by side on the Dashboard; `ProxyMetrics` is per replica (user experience, resilience) | **No new chip row; the existing table links to the registry page; live health is labelled "this replica, since …".** |
| 45 | Badge colours named tokens that do not exist and disagreed with `VerdictPanel`; mocks renamed existing strings (user experience) | **Reuse `Badge.vue` variants and `stateVariant`; reuse `verdictPanel.*` and `catalog.*` keys; the scan badge sits in the existing State cell.** |
| 46 | The poll had no backoff on errors, and landed in phase 2 while the phase-1 Security tab needs it (resilience, code quality) | **Backoff ×2 to 60 s, stop after 3 failures, `Retry-After`; `usePoll` in phase 1.** |

### Resolved after the final review

| # | Question | Decision |
| --- | --- | --- |
| 31 | Do registry-scoped delegates get the console pages? | **No; the pages stay admin-only and delegates use the API, which filters per registry.** A verb-based route guard changes the console's navigation model and is its own change. |
| 32 | Should the two `[stats]` link keys hot-reload? | **Yes, one `RELOAD_KEYS` row each; they ride in the `RegistryViews` snapshot rebuilt on reload; `docs/guide/hot-reload.md` regenerated.** Otherwise the editor reports "restart required" for a link. |
| 33 | Should a migration add indexes for the two new reads? | **Yes: `artifact_findings (registry, reference)` and `scan_jobs (registry, package_name, version, completed_at DESC) WHERE completed_at IS NOT NULL`.** An index is not data (decision 2 holds) and drops with `DROP INDEX`. |
| 34 | How does the web process know a closed job was exhausted? | **`close_exhausted` writes `last_error = 'exhausted: <last error>'`; the status reads the prefix, as `deferred_since` already does.** `max_attempts` is a worker setting the web process may not share. |

### Resolved by the second, shorter review (2026-11-10)

Three cold readers (security, code quality, consistency) found no blocker.
Their majors are fixed in the text:

| # | Finding | Decision |
| --- | --- | --- |
| 47 | `authorize_control` sees only the registry node, so the findings list skipped namespace seals the per-version GET honours (security) | **Each row is re-checked with `resolution_path_for_coordinate`, resolved without shadow; T3c.** |
| 48 | List visibility fell back to `hold_visibility`, which serves through an active shadow (security) | **Shadow-free resolution, as `installable_registries` does; a list may be stricter than the GET during a dry run, never wider.** |
| 49 | `/admin/scans` named held versions to a `system:read` holder lacking `quarantine:read` (security) | **Rows filtered per registry by `quarantine:read`, `last_error` by `findings:read`; T16.** |
| 50 | No `AppConfig` in the reload `apply`, and live data frozen in the config snapshot; no write path for the `exhausted:` marker (code quality) | **`PendingReload.new_registry_views` built in `build_pending`, live data joined per request; `ScanQueue::complete_exhausted`.** |
| 51 | Mocks showed times no field returns; wrong DB-budget baselines; rollback said nothing persists (consistency) | **`queued_since`/`deferred_since` in `ScanDetail`, no scanning start time; real budget numbers; rollback lists the indexes and the prefix.** |

### Still open

None.

---

## 16. Implementation phases

| Phase | Content | Useful alone |
| --- | --- | --- |
| 1 | `scan_status` (core `derive`, ports' `jobs_for`/`get_many`, `scan_status_for` with per-registry visibility, the skip, the timeout and `scan_status_unavailable`, the field on the list, detail and admin-list responses) + `ScanBadge` in the catalog's State cell, `AdminPackages` and the header; package page tabs; `usePoll` with its backoff; the worker's `exhausted:` marker (decision 34) and the index migration (decision 33); `config.db-calls.toml` gains a security registry and the budgets are re-measured; the explore-list soak arm; T1/T2/T5/T8/T8b/T15 | yes: motivations 1 and 2 |
| 2 | `GET /admin/scans` with its per-registry row filter, `AdminScans.vue` (queue, failures, verdicts by state), [Rescan] wired to existing endpoints; the route in `authz_matrix.rs` and `profile_routes.txt`; T7 for `/admin/scans`, T9, T16 | yes: motivation 4 |
| 3 | `GET /admin/findings` built on `authorize_control` with the per-row seal check, `AdminFindings.vue`, [Exempt…] reusing the exemption dialog; the route in `authz_matrix.rs` and `profile_routes.txt`; T3/T3b/T3c/T7 for `/admin/findings`/T10 | yes: motivation 3 |
| 4 | `next_rescan` on the verdict GET and in the Security tab | yes: motivation 5 |
| 5 | `GET /admin/registries/{name}`, `registries::project` allow-list and `AuthPresence`, suggestions, `AdminRegistry.vue` (strip + tabs) + `AdminRegistries.vue`, the extracted `StatsTrendChart`, the Activity tab on `/admin/stats/history`, links from Dashboard/Health; `[stats] dashboard_url`/`registry_dashboard_url` with their validation, `monitoring_url` on both responses, the Grafana `registry` variable; URL fields through `redacted()` behind `upstreams:read`; `RegistryViews` via `PendingReload.new_registry_views`; the two `RELOAD_KEYS` rows and `hot-reload.md` regenerated (decision 32); T4/T4b/T6/T11/T12/T13/T14 in the same PR as the endpoint | yes: motivations 6 and 7 |

Every phase regenerates `ui/openapi.json` and `ui/src/client/`, and adds its
docs and both locales. The allow-list and its sentinel test (T4) land in the
same phase as the endpoint that could leak, never after it.
