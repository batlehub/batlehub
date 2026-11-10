---
reference: true
---

# Security gates

A gate is a check that can refuse a download after the caller's grants have
allowed it: a release too young to trust, a version with a known
vulnerability, an unsigned asset, a package an administrator blocked. This page
is the operator's reference for every gate BatleHub runs — what each one reads,
what it does when that data is missing, and how the set changes once a registry
has a `[registries.security]` profile. The person whose install was refused has
their own page: [Why a download was refused](/use/refused-downloads).

---

## Two regimes {#regimes}

Every registry runs its gates in one of two ways, and which one is decided by a
single section.

| | Without `[registries.security]` | With `[registries.security]` |
| --- | --- | --- |
| **Where gates run** | On the request: each gate answers allow or deny as the download happens | Mostly in the scan worker. Its findings are stored as a **verdict**, and the request reads that verdict |
| **`release_age_gate`** | A rule | Replaced by `security.min_age_secs`. Declaring both is a config error |
| **`cve_gate`, `license_gate`, `require_signed_release`, `trusted_publisher`** | Rules, with their own `block`, `min_severity` and `bypass_roles` | Internal scanners. A finding is judged by `security.max_severity` and `security.mode`, with no bypass role |
| **`deny_latest`, `version_gate`** | Rules | Still rules: they judge the request, not the artifact |
| **Admin block list, SOC flags** | Rules | Internal scanners whose findings are always `denied` |
| **What a refused client sees** | A plain `403` naming the reason | The registry's native error shape, plus `X-BatleHub-*` headers for callers allowed to see them |
| **Can a version be held and released later?** | No: each request is judged fresh | Yes: `quarantined` lifts on its own once the hold's condition clears |

The rest of this page describes the gates one at a time, and says for each how
the second regime changes it. The profile itself, its fields and the scan worker
are in [`[registries.security]`](/guide/configuration#registries-security) and
[The scan worker](/operations/scan-worker).

::: warning Settings the verdict does not read
On a `[security]` registry, `bypass_roles` on the four wrapped gates,
`block = false` on `cve_gate` and `license_gate`, `cve_gate.min_severity`,
`cve_gate.unknown_severity`, and any namespace override of those four gates have no effect. A reload reports
each of them as `security.rule-settings-ignored`, rather than leaving them in
the file looking as though they did something.
:::

---

## The order a download meets them {#order}

A proxied download goes through these steps in order and stops at the first
refusal:

1. **IP and account blocks.** These are middleware, so they refuse before
   anything below runs. See [IP blocking](/guide/access-control#ip-blocking).
2. **Grants.** The caller must hold `releases:read` on the coordinate. This is
   checked before any upstream fetch, so a caller without the grant costs no
   egress. See [The authorization model](/guide/access-control#authorization).
3. **Metadata.** It is resolved cache-first. On the very first request for an
   uncached version, the upstream has therefore already answered before any
   gate runs, so a publish date or a signature flag is real data and not a
   guess.
4. **The registry's gate chain**, in this order:
   - the admin [block list](#block-list) and the [flags](#flags) gate, or the
     [verdict gate](#verdict-gate) on a `[security]` registry, which carries
     both;
   - on `github`, `gitlab` and `forgejo` only, the [forge ref](#forge-refs) and
     [raw content](#forge-refs) gates;
   - then every `[[registries.rules]]` entry, in the order written in the file.

The chain stops at the first gate that refuses. Which chain applies is decided
per package: when a `[[registries.namespaces]]` block overrides a gate, the
deepest matching namespace supplies the chain (see
[Per-namespace gates](#namespaces)).

**Locally published reads** (`local` and `hybrid` registries) run the same
chain, judged against the stored version. A hybrid read of a version that is
not stored locally skips the three gates that need real metadata
(`release_age_gate`, `require_signed_release` and the verdict gate), then falls
through to the proxy path, which runs the full chain on the upstream's
metadata.

**Listings and indexes run no gate.** A packument, a version list or an
`index.json` checks the `releases:list` grant and nothing else. Two things are
removed from a listing anyway: versions on the admin block list and, on a
`[security]` registry, versions whose verdict is not served. `latest` is
recomputed without them. No other gate hides a version from a listing: a
version `cve_gate` refuses is still listed, and refused when downloaded.

---

## Gates you configure {#rules}

Each of these is a `[[registries.rules]]` entry, selected by `kind`. Every
field is listed in [the configuration reference](/guide/configuration).

### `release_age_gate` {#release-age-gate}

Refuses a release published less than `min_age_secs` ago. The point is to let
the ecosystem notice a compromised release before you install it.

```toml
[[registries.rules]]
kind                   = "release_age_gate"
min_age_secs           = 259200   # 3 days; the default is 3600
bypass_roles           = ["admin"]
deny_missing_timestamp = false
```

- **Reads:** the publish date the upstream reports, or the stored date for a
  locally published version.
- **When there is no date:** the version is allowed, unless
  `deny_missing_timestamp = true`. On `nodedist`, `sdkman`, `apk`, `galaxy`,
  `nix` and `devfile` the field has no default and must be written. On `nix`,
  `sdkman` and `devfile`, no version is ever dated: `true` refuses everything
  and `false` makes the gate do nothing.
- **Refusal:** `release is too recent (age: …s, minimum: …s, … seconds remaining in quarantine)`.
- **Under `[security]`:** declaring it is a config error. Use
  `security.min_age_secs` (at least 3600) instead. The verdict holds the version
  as `MIN_AGE_NOT_MET`, with the date it becomes available, and holds an undated
  one as `TIMESTAMP_MISSING` while `hold_missing_timestamp` is on.

### `deny_latest` {#deny-latest}

Refuses a request that asked for the `latest` tag instead of a version, so
builds are reproducible.

```toml
[[registries.rules]]
kind         = "deny_latest"
bypass_roles = ["admin"]
```

- **Reads:** the version string the client sent. Only the exact,
  lower-case string `latest` matches.
- **Refusal:** `requests for the 'latest' version tag are not allowed; pin an explicit version`.
- **Under `[security]`:** unchanged. It still runs on the request.

### `version_gate` {#version-gate}

An allowlist of approved versions, a blocklist of known-bad ones, or both.

```toml
[[registries.rules]]
kind  = "version_gate"
allow = [">=1.2.0, <2.0.0"]   # when set, only matching versions are served
block = ["1.4.7", "1.5.0"]    # checked first: a block always wins
bypass_roles = ["admin"]
```

- **Matching:** an entry that contains `<`, `>`, `=`, `^`, `~`, `*` or `,` and
  parses as a semver range is a range. Anything else is an exact string. A
  version that is not semver never matches a range.
- **Refusal:** `version '…' is blocked by policy (known issue)` or
  `version '…' is not in the approved allowlist`.
- **Under `[security]`:** unchanged. It still runs on the request.

### `trusted_publisher` {#trusted-publisher}

Serves only packages whose publisher is on an allowlist. The publisher is read
from metadata already resolved, so the check makes no extra upstream call.

```toml
[[registries.rules]]
kind         = "trusted_publisher"
allow        = ["my-org", "trusted-user"]
bypass_roles = ["admin"]
```

| Registry type | The publisher is… |
| --- | --- |
| `github`, `gitlab`, `forgejo` | the first path segment (`owner/repo` → `owner`) |
| `npm` | the scope (`@scope/name` → `scope`), else the publishing user |
| `openvsx`, `vscode-marketplace` | the first segment of the extension id (`publisher.extension` → `publisher`) |
| any other type, `cargo` included | unknown, so **every request is refused** |

Matching is case-insensitive. An empty `allow` list allows everything. A
publisher that cannot be determined is refused: this gate fails closed.

- **Under `[security]`:** an internal scanner whose finding is
  `UNTRUSTED_PUBLISHER`, judged by `max_severity`, with no bypass.

### `require_signed_release` {#require-signed-release}

Refuses a release the upstream reports as unsigned. This uses the upstream's
own signal, not a cryptographic verification.

```toml
[[registries.rules]]
kind                   = "require_signed_release"
enabled                = true
deny_missing_signature = false
bypass_roles           = ["admin"]
```

- **Reads:** whether the release is signed. `github`, `gitlab` and `forgejo`
  report a `.asc`/`.sig` release asset. `openvsx` and `vscode-marketplace`
  report a signature blob. `nix` reports a `Sig` line. A locally published
  version is signed when a signature was uploaded with it. Every other type
  reports nothing.
- **When there is no signal:** the release is allowed, unless
  `deny_missing_signature = true`. A release reported as unsigned is always
  refused.
- **Refusal:** `this release is not signed; require_signed_release is enabled for this registry`.
- **Under `[security]`:** an internal scanner whose finding is
  `SIGNATURE_MISSING`, at `high`, with no bypass.

### `cve_gate` {#cve-gate}

Refuses versions with a recorded vulnerability at or above a severity.

```toml
[[registries.rules]]
kind         = "cve_gate"
min_severity     = "high"   # unknown | low | medium | high | critical
unknown_severity = "high"   # how an unrated advisory is judged; the default is "high"
block            = true     # the default is false: report only, never refuse
bypass_roles     = ["admin"]
```

- **Reads:** the vulnerabilities recorded for the version by the SBOM rescan
  (`[vulnerability_scan]`, see [SBOM](/guide/sbom)). The rescan runs out of
  band, so **nothing is recorded on the first download** of a version, and
  that download is allowed.
- **Default `block = false`:** findings appear in the console and the gate
  refuses nothing.
- **Store error:** fails open, with a `WARN` log line.
- **Unrated advisories** are judged at `unknown_severity`, `high` by default,
  so with `block = true` an advisory nobody rated is refused like a `high`
  one. `"unknown"` restores the old behaviour, where an unrated advisory never
  refused. See [Unrated findings](#unknown-severity).
- **`MAL-` advisories** are refused whatever `min_severity` says, as long as
  `block = true`. `bypass_roles` still lifts it, so the guarantee that a known
  malicious package is never served belongs to the `[security]` profile, not
  to this rule.
- **Refusal:** `blocked: known <severity> vulnerability <id> (minimum gated severity: …)`,
  `blocked: unrated vulnerability GO-… (judged as high by unknown_severity; minimum gated severity: high)`,
  or `blocked: known malicious package MAL-…`.
- **It also sets the threshold of the [flags](#flags) gate.** With `block =
  false`, `gate` flags stop refusing too.
- **Under `[security]`:** recorded vulnerabilities become `VULNERABILITY`
  findings. `min_severity`, `unknown_severity`, `block` and `bypass_roles` are
  not read: the profile's own `max_severity` and `unknown_severity` decide. The
  `osv` scanner usually answers first in that regime anyway.

### `license_gate` {#license-gate}

Refuses versions by the licence their own manifest declares.

```toml
[[registries.rules]]
kind          = "license_gate"
allow         = ["MIT", "Apache-2.0", "BSD-3-Clause"]
deny          = ["AGPL-3.0", "SSPL-1.0"]   # checked first: a deny always wins
allow_unknown = true
block         = true                       # the default is false: report only
```

- **Reads:** the licence the SBOM extractor read from the archive when it was
  cached or published. Extraction exists for `cargo`, `maven`, `npm`, `nuget`
  and `pypi`. On every other type the licence is always unknown.
- **The first download decides by `allow_unknown`.** The licence is only known
  once the archive has been fetched, so before that the version is judged as
  unknown.
- **Matching** is case-insensitive and otherwise literal: `allow = ["MIT"]`
  does not match `MIT OR Apache-2.0`.
- **Refusal:** `blocked: licence '…' is on the deny list`,
  `… is not on the allow list`, or `no declared licence recorded … and license_gate.allow_unknown is false`.
- **Config warnings:** `license-gate.sbom-disabled`, `license-gate.no-extractor`,
  and `license-gate.denies-everything` (with `block = true` and
  `allow_unknown = false` on a type with no extractor).
- **Under `[security]`:** an internal scanner whose finding is
  `LICENSE_DENIED`, at `high`. It refuses whatever `block` says, with no
  bypass.

---

## Gates that are always there {#built-in}

These are not declared in `[[registries.rules]]`. They are part of every chain
they apply to.

### The admin block list {#block-list}

The versions an administrator blocked, with the reason they gave (see
[Block a package version](/guide/admin-policies#block-a-package-version)). A block on
a bare version covers every artifact of it. A per-artifact block covers that
file only. The [upstream audit](/guide/configuration) with
`on_confirmed = "block"` writes its blocks here too.

The refusal is the administrator's own reason. There is no bypass. A store
error fails open. On a `[security]` registry the block becomes `BLOCK_LIST`,
which is always `denied`, even in `mode = "warn"`.

### Flags pushed by a SOC {#flags}

Flags pushed by a `[[flag_sources]]` entry (see
[Incident response](/operations/incident-response)):

| Effect | What it does |
| --- | --- |
| `hard_block` | Always refused. On a `[security]` registry it becomes `SOC_VERDICT`, always `denied` |
| `gate` | Refused at or above the `cve_gate` threshold (`high` when no `cve_gate` is declared), and only while that `cve_gate` has `block = true` |
| `warn`, `inform` | Never refuse |

A `gate` flag pushed with severity `unknown` is judged at the `cve_gate`'s
`unknown_severity`, `high` when no `cve_gate` is declared. There is no bypass. A store error fails open on the request path. The scanner
used on a `[security]` registry fails closed instead.

### Forge refs and raw content {#forge-refs}

These run on `github`, `gitlab` and `forgejo` registries, and are configured by
`[registries.refs]` and `[registries.raw]` (see
[the configuration reference](/guide/configuration)):

- A branch is `MUTABLE_REF` (default: warn).
- A tag whose commit changed is `TAG_MOVED` (default: refuse).
- A release asset whose digest changed since it was cached is `ASSET_REPLACED`.
- Raw files are refused unless `[registries.raw]` enables them for the
  repository. `PINNED_REF_REQUIRED` refuses a branch when `require_pinned` is
  on, and `RAW_SCRIPT` flags a script path.

Without `[security]`, a refusal is a plain `403` naming the code, and a warning
appears only in the ref headers. With `[security]`, the codes join the
request's verdict.

### The verdict gate {#verdict-gate}

This one exists only on a `[security]` registry, and replaces the block list
and flags gates there. It reads the version's stored verdict:

- **First sight:** a version never seen before queues a scan and is held as
  `SCAN_PENDING`.
- **Re-evaluated on read:** the age hold and the pending scanners are
  recomputed at every read, so a hold lifts on its own.
- **Served states:** only `allowed` and `warned` are served.
  `quarantined` and `denied` are refused.
- **Maturity bypass:** a version older than `mature_age_secs` whose only
  reasons are `SCAN_PENDING` or `SCANNER_ERROR` is served as `warned`.
- **Store error:** unlike every other gate, it **fails closed**:
  `… cannot be served: the verdict store is unavailable`.

Every reason code a verdict can carry is listed, with what it means and what to
do about it, on [Why a download was refused](/use/refused-downloads#codes).
The upstream-presence audit adds one more on these registries,
`UNPUBLISHED_UPSTREAM`, for a version its upstream no longer lists.

---

## How a finding is rated {#severity}

On a `[security]` registry every finding carries a severity, and
`max_severity` decides what it does: at or above it, `denied` in `block` mode
and `warned` in `warn` mode. This section says where that severity comes from,
and what happens when nobody gave one.

### Unrated findings: `unknown_severity` {#unknown-severity}

A finding whose source states no severity is stored as `unknown`.
`unknown_severity` says how it is judged against `max_severity`:

```toml
[registries.security]
mode             = "block"
max_severity     = "high"
unknown_severity = "high"   # unknown | low | medium | high | critical; the default is "high"
```

- **Absent means `"high"`.** Under the default `max_severity = "high"`, a
  version whose only advisory is unrated is refused: an advisory nobody has
  rated yet is not a reason to serve.
- **`"unknown"` restores the old behaviour**, where an unrated finding never
  acted. Set it explicitly, so the choice shows in the file.
- **The stored severity is not rewritten.** The finding keeps `unknown`, and
  the verdict records the substitute beside it as `judged_as`.
  `batlehub-cli why` and the verdict panel on the package page show both:
  severity unknown, judged as high by `unknown_severity`. Like every finding,
  this is visible to callers holding `findings:read`.
- **It is recomputed on every read.** Changing `unknown_severity` takes
  effect at the next download, with no rescan. The exception is an
  [escalation](#escalation) an unrated finding already triggered: that one
  wrote its new severity into the stored finding, and stays.
- **It is judged everywhere a severity meets an operator's threshold:**
  `max_severity`, an escalation's `from`, and a pushed [flag](#flags).
- Setting it below `max_severity` on a `block` registry raises
  `security.unknown-served` at load: unrated advisories are then served. The
  default `"high"` under `max_severity = "critical"` raises nothing, because
  nobody chose it.

Registries without a profile set the same key on their
[`cve_gate`](#cve-gate), with the same default.

### Known malicious packages: `MAL-` {#known-malicious}

An OSV record whose id or any alias starts with `MAL-` is not a vulnerability
with a severity. It is a database's statement that the package is malware, and
the finding is `KNOWN_MALICIOUS`:

- **Always denied**, under `mode = "warn"` too, whatever `max_severity` and
  `unknown_severity` say, like `BLOCK_LIST` and `SOC_VERDICT`.
- **Nothing lowers it.** It is decided first, from the id and every alias,
  before any of the rating rules below.
- **Cached versions are covered without a rescan.** A finding stored before
  the upgrade that names a `MAL-` record is recognised when it is read, so
  that version is refused at its next download.
- **The one way out** is an administrator's per-version exemption on the gate
  `security_verdict` (`batlehub-cli exemption add … --gate security_verdict`).
  It serves the version as `warned` with `ADMIN_OVERRIDE`, visible in the
  response headers and the audit log. It exists for a false `MAL-` listing.

On a registry without a profile, `cve_gate` refuses a `MAL-` advisory only
while `block = true`, and a `bypass_roles` caller still passes.

### What each scanner reports {#scanner-severity}

A scanner that states a severity is never overridden. When its output states
none, the finding takes the default of its kind:

| Finding kind | When the scanner states no severity |
| --- | --- |
| `MALWARE_SIGNAL`, `INSTALL_HOOK` | `high` |
| `TYPOSQUAT_SUSPECT`, the transition codes | `medium` |
| `VULNERABILITY`, every other kind | `unknown`, then judged by `unknown_severity` |

Scanner by scanner:

| Scanner | Where the severity comes from |
| --- | --- |
| `osv` | A `MAL-` id or alias: `KNOWN_MALICIOUS`, `critical`. Otherwise, in order: the record's own `database_specific.severity` (GitHub's `MODERATE` is `medium`); else the highest such rating among its *twins*, the records it names as aliases or that name it; else the highest CVSS base score computed from the `severity[]` vectors (v2, v3, v4) or bare numbers of the record and its twins, a score of `0.0` being `low`. A `RUSTSEC-` record marked informational (`unsound`, `unmaintained`, `notice`) is `low`. A `withdrawn` record is ignored. A record with none of these is `unknown` |
| `trivy` | Trivy's own `Severity`. Its `UNKNOWN` is `unknown`, judged by `unknown_severity` |
| `postmortem` | The finding's `severity`. Without one: malware signals and install hooks `high`, typosquats `medium`. Timeline transitions are `medium`; a version missing from the release history is `high` |
| `socket` | The alert's `severity`. Without one, the default of the alert's kind |
| `guarddog` | By rule family: install and exec rules `high`, exfiltration and steganography `high`, typosquat `medium`, any other rule `medium` |
| `yara` | The rule's `severity` meta, `high` when it has none |
| `sigstore` | `PROVENANCE_MISSING` `high`; `PROVENANCE_INVALID` `high`, `critical` when a cited log entry does not exist |
| `mlab` | Creates nothing. It raises a vulnerability listed in CISA KEV to `critical` |

Before this release, postmortem and socket rated an unstated malware signal `medium`,
and no setting brings that back: an unrated malware signal is what the
threshold is for.

### Go registries {#go}

A `goproxy` registry is where unrated advisories are most common, and so where
`unknown_severity` is most often lowered. The Go vulnerability database rates
nothing: a `GO-` record takes a rating only from a twin, usually a GitHub
advisory, and many have none. `golang.org/x/net@0.22.0` carries 17 `GO-`
records with no rated twin, so under the defaults it is refused.

The choice per registry:

```toml
# Keep the default, refuse what nobody rated, and excuse a version at a time
# with a security_verdict exemption.

# Or serve unrated Go advisories, and accept the load-time warning
# security.unknown-served that says so.
[registries.security]
max_severity     = "high"
unknown_severity = "medium"
```

`MAL-` records are refused either way.

### Raising a combination: escalation {#escalation}

`[scanners.<name>.escalation]` raises a scanner's findings when enough of them
land on one version. It is not set by default. One documented use is
postmortem's timeline on npm: a publisher change on its own is common, but a
publisher change in the same release that adds an install hook is how a taken
over account usually ships its payload.

```toml
[scanners.postmortem.escalation]
kinds = ["transition"]   # publisher_changed, install_hook_added, repository_moved, …
count = 2                # two transitions at the same version…
from  = "medium"
to    = "high"           # …are both raised to high, which max_severity = "high" refuses
```

Escalation counts finding kinds, not codes: any two transitions at one version
qualify, `repository_moved` with `dormant_release` included. Measure how often
that fires on your own cache before turning it on. An escalated finding is
stored at its new severity.

---

## Bypass roles {#bypass}

`bypass_roles` takes `anonymous`, `user` or `admin`. Any other value fails the
config load. The least privileged role listed is the bar, and every role at or
above it passes: `["user"]` lets users and admins through. An empty list
bypasses nobody. Only the caller's role counts, not its groups.

The block list, flags, forge-ref and verdict gates have no bypass. On a
`[security]` registry, neither do the four gates it wraps (see
[Two regimes](#regimes)). Excusing one version is what an
[exemption](#namespaces) is for.

---

## Per-namespace gates and exemptions {#namespaces}

Two ways to judge part of a registry differently, both described in
[Access Control](/guide/access-control#policies):

- **A namespace override** (`[[registries.namespaces.rules]]`) replaces the
  registry's settings for one gate, for the packages under a prefix, and keeps
  the other gates. The deepest matching namespace wins, for proxied and
  locally published packages alike. In 1.8.2 and earlier an override reached
  only locally published packages.
- **An exemption** silences one gate for one version until a date, with a
  reason, and needs the `gates:exempt` grant, which nobody holds by default
  (see [Gate exemptions](/guide/access-control#exemptions)). Only `cve_gate`,
  `license_gate`, `flags` and `security_verdict` can be exempted. In 1.8.2 and
  earlier an exemption applied only to locally published versions, not to
  proxied downloads.

On a `[security]` registry the four wrapped gates are scanners, so neither
mechanism reaches them. There, the exemption to write is `security_verdict`:
it serves the held version as `warned`, with `ADMIN_OVERRIDE` among its
reasons, never as `allowed`. A namespace override of `release_age_gate` is
also not refused at load on such a registry, and runs as a rule next to
`security.min_age_secs`. Prefer the profile's own fields.

---

## What a refused client receives {#responses}

| Regime | Status | Body | Headers |
| --- | --- | --- | --- |
| Without `[security]` | `403` | `{"error":"Forbidden","message":"<reason>"}` | none |
| With `[security]`, caller lacks `quarantine:read` | `404`; `403` on `goproxy` and `maven` | the registry's native "not found" | none |
| With `[security]`, caller holds `quarantine:read` | `403` | the registry's native error shape, carrying the verdict's one-line message | `X-BatleHub-Verdict`, `X-BatleHub-Reason`, `X-BatleHub-Available-At`, `X-BatleHub-Details`, `Retry-After` |

- **Why a `404`:** without `quarantine:read`, the caller cannot tell a held
  version from one that does not exist.
- **Defaults:** `quarantine:read` is granted to `user` and `admin`.
  `findings:read`, which adds the scanners' findings to the message, is granted
  to `admin` only.
- **`Retry-After`:** only on a time-bound hold.
- **Warned downloads:** a `warned` download is a normal `200` with the same
  headers.

The user-facing side, with what each package manager prints, is on
[Why a download was refused](/use/refused-downloads).

---

## Reading scan status {#scan-status}

On a `[security]` registry, the catalogue, the admin package list and each
version of a package page carry a **scan status** next to the existing state
badge. One server-side function derives it, first match wins:

| Status | When |
| --- | --- |
| *(none)* | the registry has no `[registries.security]` |
| `scanning` | an open job is leased by a worker whose lease has not run out |
| `queued` | an open job nobody holds: never leased, lease expired, or deferred on an advisory database outage |
| `not scanned` | no verdict has been recorded for the version |
| `scan failed` | the latest closed job ran out of attempts, or the verdict carries `SCANNER_ERROR` |
| `queued` | the verdict carries `SCAN_PENDING` |
| `scanned · <verdict>` | otherwise, with `allowed`, `warned`, `quarantined` or `denied` |

**Who sees it** follows the verbs that already decide who may learn a version
is held:

- without `quarantine:read`, nothing — the row reads exactly as it does on a
  registry that does not scan, so an anonymous visitor cannot tell the two
  apart or learn that a version is held;
- with `quarantine:read` (the `user` and `admin` roles by default), the status
  and the verdict;
- with `findings:read` too (`admin` by default), the last scan time, the
  attempts and the worker's last error, with any URL in it stripped of its
  credentials and query, cut to 200 characters.

A list decides this per registry from the configured grants and **ignores an
active `dry_run` shadow**: during a dry run a list can show less than the
per-version verdict endpoint, never more.

**A list shows the verdict as last recorded.** `GET /api/v1/verdicts/…`
re-judges against the current policy and clock; the lists read the stored row
and never trigger a scan. The two can differ for the few minutes after a
release-age window lifts or a policy changes.

When the status could not be read within 500 ms, the list is served without it
and carries `scan_status_unavailable: true`; the console then draws "—" in
place of each badge.

### The Scans page {#scans-page}

*Admin → Security & Access → Scans* follows the scan queue
(`GET /api/v1/admin/scans`):

- **Queue**: every open job, the ones a worker holds first, with the trigger,
  the time it was queued, the worker and the attempts. A deferred job shows
  when its deferral began.
- **Failures**: each version whose latest scan job ran out of attempts and that
  has no job open, with the worker's last error. [Rescan] queues a new job
  (`POST /api/v1/verdicts/…/rescan`) and the row moves to the queue.
- **Verdicts by state**: the verdicts of one registry, from
  `GET /api/v1/admin/verdicts`.

Opening the page needs `system:read`. Rows then come only from the registries
where you also hold `quarantine:read`, and the last error only where you hold
`findings:read`, so `system:read` alone names no held version. [Rescan] appears
where the configured grants give you `gates:exempt` on the registry — nobody
holds it by default — and the rescan endpoint checks it again. `per_page` is at
most 200.

A job closed before this release carries no failure marker, so a failure older
than the upgrade does not appear under Failures; its verdict still reads
`SCANNER_ERROR` under *Verdicts by state*.

---

## Watching it {#observability}

- **Every refusal** writes a `denied_download` audit row with the reason, the
  caller, the IP and the user agent, and counts on
  `batlehub_requests_total{outcome="denied"}`. Which gate refused appears only
  in the reason text: there is no gate label.
- **On `[security]` registries:**
  - `batlehub_verdict_denials_served_total{registry,code,status}` counts each
    refused download once per reason code, and once more with
    `code="response"` and the status sent;
  - `batlehub_verdicts_total`, `batlehub_verdict_transitions_total` and
    `batlehub_findings_total` follow the verdicts themselves.
- **Fail-open gates log a `WARN` line** when their store cannot be read:
  `BlockListRule`, `CveGateRule`, `LicenseGateRule` and `FlagsRule`. The
  verdict gate logs at `ERROR` and refuses.
- **To answer "why was this refused" without reproducing it:**
  - `batlehub-cli why <registry>:<name>@<version>` (see
    [the CLI](/use/cli#commands-security));
  - the verdict panel on the package page;
  - the admin *Access Check* page, which runs the same
    chain as a real download, namespace overrides and exemptions included.

---

## Configuration warnings {#warnings}

A reload reports these without refusing the file:

| Code | Meaning |
| --- | --- |
| `security.rule-settings-ignored` | A setting the verdict does not read: see [Two regimes](#regimes) |
| `security.unprotected` | `mode = "warn"` with no `required_scanners`: nothing can hold a version |
| `security.timestamp-hold-unavailable` | `hold_missing_timestamp = true` on a kind that has no publish date: every version is held |
| `security.enrichment-required` | A `required_scanners` entry that never produces findings of its own (`mlab`) |
| `security.unknown-served` | `mode = "block"` with `unknown_severity` set below `max_severity` (`"unknown"` included): unrated advisories are served. `MAL-` records are still refused. See [Unrated findings](#unknown-severity) |
| `security.scanner-error-open` | `mode = "block"` with `scanner_error = "warn"` or `"ignore"`: a scanner that cannot answer — an advisory database down or throttling included — serves the version |
| `license-gate.sbom-disabled`, `license-gate.no-extractor`, `license-gate.denies-everything` | `license_gate` with nothing to read |
| `require-signed-release.unsigned-publishes` | The gate on a registry that accepts unsigned publishes |
| `flag-source.can-hard-block` | A flag source that can refuse downloads with no threshold |

---

## Recipes {#recipes}

**A cooling-off period, without the scan worker.**

```toml
[[registries.rules]]
kind         = "release_age_gate"
min_age_secs = 259200          # 3 days
bypass_roles = ["admin"]

[[registries.rules]]
kind         = "cve_gate"
min_severity = "high"
block        = true
```

**The same, with the scan worker.** Here the hold is released on its own once
the scanners answer.

```toml
[registries.security]
mode              = "block"
min_age_secs      = 259200
mature_age_secs   = 2592000    # 30 days: serve `warned` past this if a scanner is stuck
scanners          = ["osv", "postmortem"]
required_scanners = ["osv"]
max_severity      = "high"
```

**Your own CI's packages skip the cooling-off period.** A namespace override
replaces one gate and keeps the others:

```toml
[[registries.namespaces]]
match = "@acme"

[[registries.namespaces.rules]]
kind         = "release_age_gate"
min_age_secs = 0
```

**Pin to an approved range, and refuse one bad release.**

```toml
[[registries.rules]]
kind  = "version_gate"
allow = [">=4.17.0, <5.0.0"]
block = ["4.17.20"]
```
