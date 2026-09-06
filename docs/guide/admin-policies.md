---
# Just over 4 000 words: the block/hide/quota surface, and each of the three
# behaves differently on a `[security]` registry, which is where the length
# comes from. `docs:structure` asks for this line above 4 000 words — not a cap,
# a declaration someone had to type (RFC 0005-bis §4.5).
reference: true
---

# Policies & packages

## Cache policy {#cache-policy}

For a full explanation of how caching works end-to-end — request lifecycle, backend selection, rate-limit counters, deduplication — see the dedicated **[Caching guide](/guide/caching)**.

All cache settings live under `[registries.cache]` and are per-registry.

### Eviction

```toml
[registries.cache]
metadata_ttl_secs = 300      # re-check version lists after 5 minutes (default)
serve_stale       = true     # serve cached metadata when upstream is down (default)

artifact_ttl_secs = 2592000  # delete artifacts older than 30 days
idle_days         = 14       # delete artifacts not accessed for 14 days
max_size_bytes    = 10737418240  # 10 GiB storage cap — evicts LRU when exceeded
keep_latest_n     = 5        # keep only the 5 most-recently-cached versions per package
```

All eviction fields are optional. Omitting a field disables that eviction strategy. Strategies compose: an artifact is evicted as soon as **any** active strategy triggers.

| Field | Default | Description |
|-------|---------|-------------|
| `metadata_ttl_secs` | `300` | Metadata cache TTL in seconds |
| `serve_stale` | `true` | Serve stale metadata on upstream 5xx instead of propagating the error |
| `artifact_ttl_secs` | — | Evict artifacts older than N seconds |
| `idle_days` | — | Evict artifacts not accessed for N days |
| `max_size_bytes` | — | Storage cap; LRU artifacts are removed when exceeded |
| `keep_latest_n` | — | Keep only the N most recent versions per package |

#### Running it, and previewing it {#cache-eviction-run}

```sh
batlehub admin cache evict acme-npm --dry-run   # what would go, and nothing goes
batlehub admin cache evict acme-npm             # actually evict
```

Live is the default here, the opposite of [`admin retention`](#retention): what
eviction drops is a copy the next request re-fetches, so the two-key interlock
that protects a locally published version would be ceremony. The preview exists
for the other question — *how much would this new size cap actually take?* — and
answers it with the keys, not just a count.

A size-cap preview is bounded: it reads one page of LRU candidates, and says so
in `incomplete_because` if the registry is still over the cap when the page runs
out. A live run keeps going.

#### What a run leaves behind {#cache-eviction-trail}

| | Live run | Dry run |
| --- | --- | --- |
| Run event, registry-scoped | `cache_evict_run` | `cache_evict_dry_run` |
| Per-artifact event | none — see below | none |

There is deliberately **no event per evicted artifact**, which is where this
parts company with [retention's trail](#retention-trail): an LRU sweep evicts by
the thousand, and burying the deletions that are *not* recoverable under the
ones that are would make the whole trail unreadable. What went is in the run's
report and its log line.

Dropping a cached artifact **by hand** is the other case — one operator, one
decision, one package — and it does carry the coordinate:

| Surface | Action |
| --- | --- |
| `DELETE /api/v1/admin/registries/{r}/cache` | `cache_evict` |
| `POST /api/v1/admin/packages/invalidate` | `cache_evict` |
| `POST /api/v1/admin/registries/{r}/clear-cache` | `cache_clear`, registry-scoped |
| `POST /api/v1/admin/registries/{r}/coherence` | `cache_coherence_run` / `cache_coherence_dry_run` |

None of these is a `delete`. A cached copy is not the package, and an auditor
must not have to read a registry's mode out of a config file to tell them apart:

```sh
# Everything that was actually deleted, by hand or by policy
batlehub admin audit-log --action delete,retention_reclaim

# Everything that was merely un-cached
batlehub admin audit-log --action cache_evict,cache_clear,cache_evict_run
```

#### Collecting orphaned blobs {#cache-coherence}

An artifact is cached in two steps — the bytes are stored, then the row that
points at them is recorded. A process killed between the two leaves a blob
nothing references: it occupies disk, no request can ever reach it, and no
eviction strategy will ever consider it, because every strategy reads the table
it is missing from. Deleting a row from the database by hand leaves the same
thing.

```sh
batlehub admin cache coherence acme-npm --dry-run   # what is orphaned
batlehub admin cache coherence acme-npm             # collect it
```

**Two passes before anything goes.** A blob is deleted only if the *previous*
sweep saw it orphaned too — because a cache write in flight looks exactly like
an orphan, and the window between its two steps is milliseconds. So the first
sweep of a fresh estate reports `first_seen_orphaned` and deletes nothing; run it
again to collect. The report keeps the two apart: `deleted_keys` is what went,
`first_seen_keys` is what a second run would take.

`--dry-run` reports without deleting **and without advancing anything toward
deletion**. Previewing twice is not the same as running twice — a preview that
armed what it described would be a trap rather than a preview.

Unlike the eviction strategies, this needs no configuration: orphans do not wait
for a TTL to be set, so the sweep is available on every registry.

To run it on a schedule instead of by hand, enable
[`[cache_coherence]`](/guide/configuration#38b-cache-coherence-optional). Scheduled
sweeps carry `user_id = "system"` in the trail, which is what tells them apart
from an operator's:

```sh
batlehub admin audit-log --action cache_coherence_run
```

### Cache warming {#cache-warming}

Cache warming pre-fetches artifact versions so they are available with zero latency on first request. Configure it alongside eviction:

```toml
[registries.cache]
warm_packages    = ["lodash", "react", "typescript@5.4.5"]
warm_latest_n    = 3   # warm the 3 most recent versions of bare-name entries
warm_concurrency = 4   # up to 4 parallel downloads
```

| Field | Default | Description |
|-------|---------|-------------|
| `warm_packages` | `[]` | Packages to warm at startup. `"name"` warms the latest `warm_latest_n` versions; `"name@version"` warms exactly one. |
| `warm_latest_n` | `1` | Versions to pre-fetch per bare-name entry |
| `warm_concurrency` | `2` | Maximum parallel downloads per warming run |

BatleHub starts warming immediately after binding the server socket, so the HTTP server is available while warming runs in the background.

#### On-demand warming via admin API

Re-warm a package at any time without restarting:

```sh
# Warm using the registry's configured warm_latest_n
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash"}'

# Override the version count for this request only
curl -X POST http://localhost:8080/api/v1/admin/registries/npm/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "lodash", "versions": 10}'

# Warm a single pinned version
curl -X POST http://localhost:8080/api/v1/admin/registries/cargo/warm \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"package": "serde@1.0.200"}'
```

Response:

```json
{"warmed": 3, "skipped": 0, "errors": 0}
```

- `warmed` — artifact versions fetched and stored in this run
- `skipped` — versions already present in the cache (no download needed)
- `errors` — versions that failed to fetch or store

::: tip Registry support
Version enumeration (used for bare-name warming) is implemented for every package-based registry type. A pinned entry is always `name@version`, where the name half is the coordinate that registry addresses packages by — `"lodash@4.17.21"` (npm), `"com.google.guava:guava@33.0.0-jre"` (Maven), `"providers/hashicorp/aws@5.0.0"` (Terraform), `"rails@7.1.0"` (RubyGems), `"monolog/monolog@3.5.0"` (Composer). For **GitHub**, bare names enumerate releases via the Releases API (paginated). For **VS Code Marketplace**, bare names enumerate all extension versions via the Gallery API. For **Conda**, BatleHub synthesises the version list by scanning `repodata.json` across `noarch`, `linux-64`, `osx-64`, `osx-arm64`, and `win-64`. For **JetBrains Marketplace**, an entry is the plugin `xmlId` (`"org.rust.lang"`, `"org.rust.lang@0.4.201"`) and bare names enumerate versions via `/plugins/list` — which covers the **Stable** channel only, so EAP/nightly builds are not pre-fetched.
:::

### Content-addressable deduplication

BatleHub stores artifact bytes at a content-addressed key (`blob/{sha256}`) and maps logical artifact keys (e.g. `artifact:npm/lodash/4.17.21`) to that blob via a reference count. When identical bytes appear under multiple logical keys — the same package mirrored across two registries, a yanked-then-re-released version — only one copy is stored on disk or in S3.

This is automatic and requires no configuration. Pre-deduplication artifacts stored before upgrading continue to be served normally.

---

## Package management {#package-management}

### List packages

```sh
# All packages
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/packages"

# Filter by registry and name
curl -H "Authorization: Bearer <admin-token>" \
  "http://localhost:8080/api/v1/admin/packages?registry=npm&name=lodash"
```

### Block a package version

A block does two things, and both matter:

1. **The version disappears from version listings**, in whatever shape the
   ecosystem's clients read — an npm packument, a NuGet flat index, a
   `maven-metadata.xml`, a PyPI simple page. Whatever that protocol calls
   "newest" is repaired to name a version that is still allowed:
   `dist-tags.latest` and Maven's `<release>` are recomputed, Go's `@latest` is
   re-resolved. A client asking for `latest`, or for a range like `^4.17.0`,
   therefore resolves to an allowed version and installs successfully — it never
   selects the blocked one. See [which listings are
   filtered](#which-listings-are-filtered) for the per-protocol table.
2. **Downloading it returns `403 Forbidden`** to all clients regardless of role,
   with the reason you recorded. Hiding governs which version a resolver
   *picks*; this governs whether someone who names the version explicitly may
   have it. Pinning `lodash@4.17.20` in a lockfile fails with a message that
   says why, rather than looking like a missing package.

On a `[security]` registry the download gate is the version's verdict, so the
block is written into it as a `BLOCK_LIST` finding the moment you record it, and
the `block_list` scanner re-derives it on every later scan. Same `403`.

A block recorded against a version covers every file in it — the npm tarball,
a Maven classifier, a Terraform provider binary.

Blocking one specific artifact (by passing `artifact`) is deliberately
asymmetric: the download gate refuses only that file, but the **whole version
disappears from listings**. A resolver that selects a version whose bytes are
partly refused has no way to know which of its files it may have, so a version
with a blocked artifact is not advertised as installable. Someone who knows the
exact coordinate of an unblocked sibling file can still fetch it.

On a `[security]` registry the per-file form has **no effect on downloads**: that
gate is the version's verdict, and a verdict is keyed on the version, so it
cannot express "this file but not its siblings". The block is still recorded and
still hides the version from listings, and the server logs a warning saying so.
Block the whole version when you need the bytes refused there.

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm", "name": "lodash", "version": "4.17.20", "reason": "CVE-2021-23337"}' \
  http://localhost:8080/api/v1/admin/packages/block
```

#### Which listings are filtered

Every **local/hybrid** registry filters its version listings, through the one
chokepoint every ecosystem's local listing resolves against. For **proxied**
registries, coverage is per protocol — a listing can only be filtered if the
protocol has one and editing it is safe:

<!-- BEGIN listing-coverage: generated by `task docs:listing-coverage`. Do not edit by hand. -->
| Registry | Listing document | Blocked versions hidden |
| --- | --- | --- |
| github | release listings | yes |
| forgejo | release listings | yes |
| gitlab | release listings | yes |
| cargo | sparse index | yes — blocked versions are marked `yanked` rather than removed, which is cargo's own mechanism for "exists, do not select" and keeps lockfile diagnostics honest |
| npm | packument | yes |
| openvsx | extension gallery (`extensionquery`) and the OpenVSX API | yes |
| goproxy | `@v/list` and `@latest` | yes |
| pypi | simple index (HTML and PEP 691 JSON) | yes |
| conda | `repodata.json`, `current_repodata.json` (and their `.zst`/`.bz2` encodings) | yes |
| conda | `channeldata.json` | yes — a blocked newest release drops the package from the channel summary rather than moving it to an older one: channeldata names one version and carries no list to pick a replacement from, so `conda search` stops showing it while `conda install` still resolves it from `repodata.json` |
| composer | p2 metadata | yes |
| vscode-marketplace | extension gallery (`extensionquery`) and the OpenVSX API | yes |
| maven | `maven-metadata.xml` | yes |
| terraform | module and provider versions | yes |
| rubygems | compact index (`/versions`, `/info/{gem}`) | yes — `/versions` describes the whole registry, so a new block reaches it within the blocked-set snapshot's 30-second TTL rather than instantly; `/info` is per-gem and immediate |
| rubygems | versions and gem JSON APIs | yes |
| rubygems | `specs.4.8.gz`, `quick/Marshal.4.8` | no — hiding a version from a Ruby Marshal index would need a Marshal encoder in Rust, and nothing reads it: Bundler resolves from the compact index above, and the JSON APIs answer every other client released this decade |
| nuget | flat index | yes |
| nuget | registration pages | yes — inline pages only; paged registrations pass through, and are logged |
| deb | signed repository indexes | no — editing one invalidates its signature and the client rejects the whole repository, which is a worse failure than the one filtering fixes |
| rpm | signed repository indexes | no — editing one invalidates its signature and the client rejects the whole repository, which is a worse failure than the one filtering fixes |
| pacman | signed repository indexes | no — editing one invalidates its signature and the client rejects the whole repository, which is a worse failure than the one filtering fixes |
| jetbrains | — | no listing document |
| jetbrains-marketplace | `updatePlugins.xml`, `/plugins/list` and the plugin-updates API | yes |
| generic | — | no listing document |
| nodedist | `index.tab` | yes |
| nodedist | `index.json` | yes |
| sdkman | `versions/all` | yes |
| sdkman | `candidates/default` | yes |
| sdkman | the rendered `versions/list` table (`sdk list`) | yes |
<!-- END listing-coverage -->

Filtering is invisible when it works, which is exactly when you want evidence
that it did. The Prometheus counter
`listing_versions_hidden_total{registry,kind,document}` records how many entries
each listing dropped, so "did the block take effect" is answerable from the
metrics endpoint without turning on debug logging in production.

::: tip Whole-registry indexes lag by up to 30 seconds
Most listings are filtered against a blocked set queried per request, so a block
disappears from them on the very next call. Three documents describe a **whole
registry** rather than one package, and are fetched on every install:

| Registry | Document |
|---|---|
| conda | `repodata.json`, `current_repodata.json`, `channeldata.json` |
| rubygems | the compact index's `/versions` |

For those, the blocked set is read from a snapshot refreshed every **30
seconds** rather than queried per request — re-reading the whole registry's
block list on the hottest path in the ecosystem costs more than the seconds it
saves. A block can therefore take up to half a minute to disappear from one of
them.

**The `403` on the download never lags**, for any registry. So the window is one
where a client may still be *offered* a version it will then be refused — the
mid-resolve failure this feature exists to avoid, narrowed to half a minute
rather than eliminated. Per-package listings, including RubyGems' `/info/{gem}`
and its JSON APIs, are immediate.
:::

### Unblock

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm", "name": "lodash", "version": "4.17.20"}' \
  http://localhost:8080/api/v1/admin/packages/unblock
```

### Bulk block

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages": [{"registry":"npm","name":"bad-pkg","version":"1.0.0"}]}' \
  http://localhost:8080/api/v1/admin/packages/bulk-block
```

### Invalidate cache

Removes the cached artifact so the next request re-fetches from upstream:

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"registry": "npm", "name": "lodash", "version": "4.17.21"}' \
  http://localhost:8080/api/v1/admin/packages/invalidate
```

---

## Deleting a published version {#deleting-versions}

Deleting a version from a `local` or `hybrid` registry does two things, and the
second one surprises people:

1. The **artifact is dropped**. The bytes are gone; nothing serves them again.
2. The **version number is spent**. `1.4.0` can never be published again in that
   registry — not by you, not by anyone, not in a year.

The second is deliberate and there is no setting that turns it off.

::: warning Delete-and-re-upload is not a fix
If you published broken bytes under `1.4.0`, deleting it does not free `1.4.0`
for a corrected upload. Publish `1.4.1`. If you need the broken version to stop
being installed *without* spending its number, [yank it](/use/cli)
instead — a yanked version stays resolvable by exact pin and can be unyanked.
:::

### Why a name is never reused

A lockfile pins `1.4.0` and records its checksum. If deleting `1.4.0` freed the
coordinate, then some later publish — by a different person, months afterwards,
for perfectly good reasons — could occupy it with entirely different bytes. Every
consumer that resolves that lockfile then installs something that merely shares a
name with what they reviewed.

This is npm's model, and npm has been exploited through it. crates.io and PyPI
take the other one, and so does BatleHub. It matters more here than upstream: a
private registry is frequently the *only* copy of what it holds, so there is no
second source to notice the substitution.

The mechanism is a **tombstone**: the version row survives the delete with a
`deleted_at` timestamp, and the publish path consults it. A publish onto a spent
coordinate is refused with `409`:

```
my-pkg@1.4.0 was published and deleted on 2026-08-27 in registry 'acme-npm';
a published version coordinate is never reused — publish under a new version
```

| | After deleting `1.4.0` |
| --- | --- |
| The artifact | gone from storage |
| Every registry listing — packument, sparse index, flat index, Simple page, `maven-metadata.xml`, compact index, `@v/list` | `1.4.0` is absent |
| Downloading `1.4.0` by exact coordinate | `404` |
| Publishing `1.4.0` again | `409`, permanently |
| The package **name** | free, if every version is gone — see below |
| The audit trail | records who deleted it and when |

Deleting every version of `@acme/widgets` releases the *name*: someone the grants
permit may create `@acme/widgets` again. The version numbers that existed stay
spent. Re-creating `@acme/widgets` is allowed; re-creating `@acme/widgets@1.4.0`
is not.

**Its package owners go with it.** When the last version of a package is
deleted, every owner entry on that name is dropped, and the next publisher
becomes the owner of the name they created. The alternative is worse in a way
that is easy to miss: owner rows keyed by a name, surviving the package, mean the
previous owner still holds publish and owner-management rights over a package
they have never seen — and, more immediately, their stale row *refuses* the
newcomer trying to take the released name. The version tombstones stay, because
they are the invariant; the owners go, because they are a decision about a thing
that no longer exists.

### Deleting

```sh
batlehub version delete acme-npm my-pkg 1.4.0
```

It prompts, because of the second effect above. `-y` skips the prompt. The same
endpoint takes a list:

```sh
curl -X POST \
  -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages": [{"name": "my-pkg", "version": "1.4.0"}]}' \
  http://localhost:8080/api/v1/admin/registries/acme-npm/bulk-delete
```

Deleting a coordinate that is already deleted, or never existed, counts as
success — re-running a bulk delete that half-applied is safe.

### Reading what was deleted

```sh
curl -H "Authorization: Bearer <admin-token>" \
  'http://localhost:8080/api/v1/admin/registries/acme-npm/tombstones?name=my-pkg'
```

```json
{
  "registry": "acme-npm",
  "total": 1,
  "tombstones": [
    {
      "registry": "acme-npm",
      "name": "my-pkg",
      "version": "1.4.0",
      "deleted_at": "2026-08-27T09:14:22+00:00",
      "deleted_by": "alice",
      "published_at": "2026-03-02T11:40:05+00:00",
      "published_by": "ci-runner",
      "checksum": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    }
  ]
}
```

The `name` query parameter is optional; without it you get every tombstone in the
registry, newest deletion first.

You cannot un-delete. Restore the bytes from a backup and publish them under a
new version number. If the deletion was recent, the tombstone still carries the
original checksum, so you can verify that what you restored is what was there.

### Tombstone compaction {#tombstone-compaction}

A tombstone row holds two things with two different lifetimes:

| Part | Example | Lifetime |
| --- | --- | --- |
| **The claim** | `(acme-npm, my-pkg, 1.4.0)` | permanent — it *is* the invariant |
| **The detail** | index metadata, checksum, publisher, signature | audit history |

Only the detail grows. A cargo index line carries a version's full dependency
graph and an npm manifest its scripts and `dist` block — kilobytes each — while
the coordinate is a hundred bytes. Compaction strips the detail after a window
and keeps the claim.

```toml
[registries.retention]
tombstone_detail_for_days = 730   # strip detail after two years
dry_run = false                   # defaults to true
```

**Unset by default**, so nothing is stripped until you ask. An auditor
investigating a deletion is the reader most likely to be surprised by a default
here, and the cost of keeping the detail is disk — which is recoverable — where
the cost of losing it is a question that can no longer be answered. Full field
reference: [`[registries.retention]`](/guide/configuration).

```sh
curl -X POST -H "Authorization: Bearer <admin-token>" \
  'http://localhost:8080/api/v1/admin/registries/acme-npm/tombstones/compact?dry_run=true'
```

```json
{ "registry": "acme-npm", "compacted": 412, "skipped": 38, "dry_run": true,
  "coordinates": ["my-pkg@1.4.0", "..."] }
```

`409` if the registry has no `tombstone_detail_for_days` — an unconfigured
registry is not a run that found nothing.

- **`dry_run` defaults to `true`.** A configured window reports and strips
  nothing until you set `dry_run = false`. Once you do, the server raises
  `retention.compaction-live` on every config reload; that is intentional,
  because it is the one setting in this block that destroys something.
- **`?dry_run=` can only make a run safer.** `?dry_run=true` previews against a
  registry configured live; `?dry_run=false` does *not* override a configured
  `dry_run = true`.
- **Compaction never touches a live version**, and never touches a tombstone twice.
- **There is no way to delete a tombstone.** Not a setting left off, not an
  endpoint behind a flag — the schema has no representation for it, because
  collecting a tombstone reopens exactly the hole tombstones exist to close.

### Reclaiming versions nobody is using {#retention}

Everything above is about deleting a version *by hand*. Retention does it on a
policy — and because a locally published artifact is frequently the only copy in
existence, every default is set so that getting the policy wrong keeps too much
rather than too little.

```toml
[registries.retention]
keep_versions       = 10
keep_if_pulled_days = 90
dry_run             = false
```

**A version survives if *any* configured condition matches.** There is no
expression to write and no ordering to get wrong; the only way to reclaim a
version is for every condition to decline. A block with no keep condition at all
is refused at startup, because it is the one that would reclaim everything on its
first run.

`keep_if_pulled_days` is the one that matters. `keep_versions = 10` alone throws
away the version half your estate is pinned to, because it happens to be eleventh
by date. With the pull veto, whatever anyone is actually using stays, regardless
of age or count — and configuring reclamation without it warns on every reload.

Run it:

```sh
batlehub admin retention acme-npm              # report; changes nothing
batlehub admin retention acme-npm --show-kept  # …and why each survivor survived
batlehub admin retention acme-npm --reclaim    # actually reclaim
```

`--reclaim` is only half the interlock: the registry also needs `dry_run = false`.
Two decisions in two places, one of them a config file someone reviewed.

```
Retention on acme-npm: dry run — nothing was changed
  examined 1284   kept 1201   reclaimed 83

would reclaim:
  internal-tool@0.1.0
  …
```

A reclaimed version is deleted the same way a hand deletion is: the bytes go, a
tombstone stays, and **the coordinate is spent**. Freeing disk must not free the
namespace, or retention becomes a supply-chain mechanism by accident.

#### What a run leaves behind {#retention-trail}

| | Live run | Dry run |
| --- | --- | --- |
| Run event, registry-scoped | `retention_run` | `retention_dry_run` |
| Per-version event | `retention_reclaim` | none |
| Tombstone | one per version | none |

A run is triggered with an operator's own token, so the *subject* of the event
cannot tell a policy apart from a person — the action does. `retention_reclaim`
is never `delete`, and it always means the version is gone:

```sh
# What the policy took, and what somebody took by hand
batlehub admin audit-log --registry acme-npm --action retention_reclaim
batlehub admin audit-log --registry acme-npm --action delete
```

A dry run records **itself and nothing else**. That is on purpose in both
directions: the preview is an operator's decision against a production registry
and belongs on the record, but a row saying a version was reclaimed when it is
still there would make the trail unreadable. What a dry run *would* have taken
is in the report it prints and nowhere else — so keep the output, or re-run it.

#### Pinning a version against retention

The escape every automatic policy needs — the release an LTS customer runs, which
the pull statistics will eventually stop defending:

```sh
batlehub version pin   acme-npm my-pkg 2.4.0
batlehub version unpin acme-npm my-pkg 2.4.0
```

A pinned version is never reclaimed, whatever the policy says. It changes nothing
else: the version resolves, downloads and lists exactly as it did. There is
deliberately no opposite — no way to make retention *more* aggressive for one
version — because a policy that deletes should not be reachable one version at a
time.

#### Reading the download signal, and its gaps

`keep_if_pulled_days` counts **downloads**, not index reads. One `mvn` resolution
touches a `.jar`, a `.pom` and a checksum beside each: the checksum records as a
metadata view, the `.pom` as a download, because a `.pom` is a file a build
actually consumes. So a version kept alive only by checksum fetches is *not*
kept, and one whose `.pom` is still being resolved is.

The Maven and NuGet local artifact paths recorded no download event at all before
2026-08-26. Retention will not read that silence as disuse: a version with no
download record that was published before the floor is kept.
`download_signal_floor_days` moves the floor if this instance's audit history
begins later — after a restore, or an `audit_purge`.

A `keep_if_pulled_days` policy on a deployment with no package repository refuses
to run rather than reclaiming what it cannot prove is idle.

### This is not cache eviction

`[registries.retention]` and `[registries.cache]`'s eviction keys look alike and
govern opposite things.

| | `[registries.cache]` eviction | `[registries.retention]` |
| --- | --- | --- |
| Governs | proxy-cached artifacts | locally published versions and their tombstones |
| Another copy exists | yes, upstream | frequently not |
| Cost of a wrong reclaim | a re-fetch | the artifact |
| Default | configured per registry | keep everything, forever |
| Preview | `--dry-run`, opt in | on unless `dry_run = false` |
| Audited | per run | per run **and** per version |

A `[registries.retention]` block on a `proxy`-mode registry is a config error,
not a silent no-op: that registry publishes nothing locally, so the block would
govern an empty set — [`[registries.cache]`](#cache-policy) is what you meant.

Why it works this way, and what the reclamation half of retention will look
like: [RFC 0016](/rfc/0016-retention-and-the-permanence-of-a-published-name).

---

## Rules {#rules}

Rules are optional per-registry policies evaluated after RBAC.

### Release age gate

Block packages published less than `min_age_secs` ago:

```toml
[[registries.rules]]
kind         = "release_age_gate"
min_age_secs = 3600       # 1 hour
bypass_roles = ["admin"]  # admins can still install new packages
```

### Deny latest tag

Force clients to pin exact versions:

```toml
[[registries.rules]]
kind         = "deny_latest"
bypass_roles = ["admin"]
```

### Trusted publisher

Restrict downloads to packages published by an allowed org, user, or scope. The publisher is derived from metadata already resolved during the proxy fetch — no extra upstream calls.

```toml
[[registries.rules]]
kind         = "trusted_publisher"
allow        = ["my-org", "trusted-user"]
bypass_roles = ["admin"]
```

Publisher support by registry type (matching is case-insensitive):

- **GitHub**, **GitLab**, **Forgejo** — the top-level owner/group segment of the package path (`"owner/repo"` → `"owner"`)
- **npm** — the scope for scoped packages (`"@scope/name"` → `"scope"`); otherwise the publishing user
- **OpenVSX**, **VS Code Marketplace** — the publisher segment of the extension id (`"publisher.extension"` → `"publisher"`)
- **Not yet supported: Cargo** and any other registry type — configuring this rule there denies every request (fail-closed)

See [`docs/guide/configuration.md`](https://github.com/batlehub/batlehub/blob/main/docs/guide/configuration.md) for the full field table.
