# BatleHub — Roadmap

Planned features and improvements, grouped by theme. Within each group the order reflects rough implementation priority.

For discussion or to propose a feature, open an issue on the [project repository](https://git.batleforc.fr/batleforc/batlehub).

---

## New registry types

Current adapters: npm, Cargo, GitHub, Forgejo/Gitea, GitLab, OpenVSX, VS Code Marketplace, Go modules, Maven, RubyGems, Terraform, Composer, PyPI, Conda, NuGet, Deb (APT), RPM (YUM/DNF), Pacman, JetBrains (IDE archives), JetBrains Marketplace (plugins), Generic (path-addressed file mirror).

- [x] **PyPI** — Python package index; Simple API proxy with URL rewriting; wheel / sdist downloads; private publishing via `twine` in `local`/`hybrid` mode
- [x] **Maven / Gradle** — Maven Central-compatible metadata XML + JAR / POM downloads; private publishing via `mvn deploy` in `local`/`hybrid` mode
- [x] **RubyGems** — gem downloads and version listing (proxy + local/hybrid with publish/yank/unyank)
- [x] **NuGet** — .NET package protocol; NuGet v3 service index + flat container proxy; `.nupkg` and `.nuspec` downloads; private publishing via `dotnet nuget push` in `local`/`hybrid` mode
- [x] **Deb / RPM** — Debian APT (`type = "deb"`) and Red Hat YUM/DNF (`type = "rpm"`) repository proxying **and** private hosting in `local`/`hybrid` mode: `.deb`/`.rpm` publish, `Packages`/`Release` and `repodata/` regeneration, Ed25519 OpenPGP-signed metadata (`InRelease`/`Release.gpg`, `repomd.xml.asc`). Signing is hand-rolled (Ed25519 only) to avoid the banned `rsa` crate
- [x] **JetBrains IDE archives** — `type = "jetbrains"`: proxy-only path-based cache for JetBrains IDE installer archives (default upstream `download.jetbrains.com`); reuses the generic `PathProxyRegistryClient`. No private hosting. IDE archives are large (~1-1.7 GB), so `limits.max_artifact_size_bytes` must be raised
- [x] **JetBrains Marketplace** — `type = "jetbrains-marketplace"`: full marketplace emulation for the plugin ecosystem (default upstream `plugins.jetbrains.com`) in `proxy`/`local`/`hybrid` mode. IDE-facing surface (search, compatible updates, `meta.json` blobs, `plugin/download`, `pluginManager`) complete enough for `idea.plugins.host` full replacement, plus `updatePlugins.xml` for the additive `idea.plugin.hosts` flow; marketplace-compatible multipart publish (`POST /api/updates/upload`) so `plugin-repository-rest-client`/Gradle `publishPlugin` work against local/hybrid registries. Metadata, artifacts, and forwarded query blobs are all cached with stale fallback, so previously-seen plugins keep resolving if plugins.jetbrains.com is unreachable
- [x] **VS Code extension gallery** — `type = "openvsx"` / `"vscode-marketplace"`: the client-facing gallery protocol (`POST …/vscode/gallery/extensionquery`, assets, `resourceUrlTemplate`, `item`) so an editor can be pointed at BatleHub with `extensionsGallery` in `product.json` and search, install and update through it, plus the OpenVSX REST API (`/api/{namespace}/{extension}`, `/api/-/search`, `…/file/{name}`) for `ovsx`. Both protocols render from one entry list, so blocked versions are hidden from both. Assets — manifest, README, changelog, licence, icon — are extracted from the cached VSIX, so one artifact answers them all. **The editor sends no credentials to its gallery**, so a gallery registry needs `anonymous` read or an authenticating ingress
- [x] **Terraform registry** — provider and module proxy protocol; private module + provider publishing in `local`/`hybrid` mode
- [x] **GitLab releases and packages** — `type = "gitlab"`: paginated release list/tag, link assets, source archives + raw files via the `/-/` URL scheme, nested groups, `PRIVATE-TOKEN`/Bearer auth; package registry passthrough (`/api/v4/…`, ideal for generic packages). Ecosystem package registries (npm/Maven/PyPI/…) are proxied via the matching typed adapter pointed at the GitLab package endpoint
- [x] **Forgejo releases and packages** — `type = "forgejo"`: paginated Gitea/Forgejo `/api/v1` release list/tag, assets, source archives, raw files (reuses the GitHub URL scheme); package registry passthrough (`/api/packages/…`). Ecosystem registries via the matching typed adapter
- [x] **Composer** — PHP Composer registry (Packagist v2 protocol — `packages.json`, p2 metadata, dist downloads); private package publishing via ZIP upload in `local`/`hybrid` mode
- [x] **Anaconda / Conda** — Python data science package registry; `repodata.json` proxy and channel merging; `.tar.bz2` and `.conda` package parsing; private channel publishing in `local`/`hybrid` mode
- [x] **Arch Linux / Pacman** — `type = "pacman"`: proxy upstream Arch mirrors **and** private hosting in `local`/`hybrid` mode: `.pkg.tar.{zst,xz,gz}` publish (metadata read from `.PKGINFO`), per-arch `<repo>.db`/`<repo>.files` database regeneration, Ed25519 OpenPGP-signed database (`<repo>.db.sig`) and packages (`.sig` + embedded `%PGPSIG%`) so `SigLevel = Required` works. Signing reuses the hand-rolled Ed25519 signer (the `rsa` crate is banned)
- [x] **Generic file mirror** — `type = "generic"`: proxy-only path-addressed cache for any HTTP file tree that has no package protocol at all (toolchain tarballs, vendor CDNs, release buckets). Reuses `PathProxyRegistryClient` like `jetbrains`, but with a mandatory explicit `upstreams` entry and a mandatory `path_allow` glob allowlist so a registry pointed at a shared host (e.g. `storage.googleapis.com`) cannot become an open relay for every other path on it. Covers the toolchain sources that no typed adapter reaches — rustup (`static.rust-lang.org`), the Go toolchain (`dl.google.com/go`, distinct from the `goproxy` module adapter), and single-binary vendor CDNs (`get.helm.sh`, `dl.min.io`, `binaries.sonarsource.com`). Archives can be large, so `limits.max_artifact_size_bytes` may need raising. Node (`nodejs.org/dist`) was on this list and is the reason `nodedist` below exists: `generic` mirrors it correctly and can enforce nothing on it, because a path-addressed registry has no version to block. `generic` stays the right answer for a tree you want cached without policy

**Not yet started, in rough priority order:**

- [x] **Node distributions (`nvm`, fnm, `n`, mise)** — `type = "nodedist"`: the `nodejs.org/dist` tree as a *typed* adapter rather than a `generic` mirror, so a Node release can be blocked rather than merely cached. `index.tab` and `index.json` are filtered listings; the tarballs and `SHASUMS256.txt` are artifacts under `{name}/{version}/{file}`. The point is the identity, not the bytes: `generic` already caches this tree, and because it addresses everything as one synthetic package (`repo`/`_`) there is no version to block, nothing in explore and no per-version statistics — a gap the working cache hides. `index.tab` is also the enforcement chokepoint, because `nvm_remote_version` resolves *every* install through it, including a fully specified one. `SHASUMS256.txt` is passed through byte-exact: nvm verifies against it and a sibling `.asc`/`.sig` signs it. Shipped with RFC 0010 phases 1–4 on 2026-09-03, verified by `tests/heavy/nvm.sh` against nvm 0.40.3; the console setup snippet and the registry page landed with phase 9 on 2026-09-04 (RFC `docs/rfc/0010-toolchain-managers.md` §13.1, §13.2)
- [x] **JVM toolchains (SDKMAN)** — `type = "sdkman"`: the JDK, Gradle, Maven-the-distribution and the Kotlin compiler, none of which any BatleHub instance has ever seen. Proxies the candidates API and the download broker as one registry (`broker_url` beside cargo's `index_url`), filters `versions/all`, `candidates/default` and the rendered `sdk list` table, and refuses a blocked version at `candidates/validate` so SDKMAN prints its own *"is not a valid … version"* rather than failing mid-download. The broker answers `302` to third-party CDNs, so the redirect chain is followed **server-side** through the SSRF guard — otherwise the 200 MB JDK still leaves the site and the proxy has mediated a policy decision and none of the bytes. Hook scripts are relayed byte-exact; they are bash the client sources and runs Shipped with RFC 0010 phases 5–9 on 2026-09-04, verified by `tests/heavy/sdkman.sh` against sdkman-cli 5.23.0 (the refusal, the broker redirect, the relayed hook and the cache all observed on the wire); `warm_platforms` and the `.nvmrc`/`.sdkmanrc` inputs of `registry suggest` landed with it, as did the console entries and registry pages for both toolchain kinds (RFC `docs/rfc/0010-toolchain-managers.md` §13.2)
- [ ] **Generic file mirror, `local`/`hybrid` mode** — publish arbitrary files under `{name}/{version}/{filename}`, the equivalent of GitLab's "generic packages": internal build artifacts, installers, blobs from CI. Inherits quotas, ownership, Ed25519 artifact signing, SBOM, yank and dedup from the shared local-registry machinery
- [ ] **Helm charts** — `type = "helm"`: a real chart repository (`index.yaml` + `.tgz`) needs URL rewriting in the generated index, so it warrants its own adapter rather than a `generic` instance; `local` mode would regenerate `index.yaml` from the DB the way `conda` regenerates `repodata.json`. Note that the *helm binary* (`get.helm.sh`) is already covered by `generic` — this entry is about charts. OCI-based charts stay out of scope (see below)

> **Not planned:** Docker / OCI artifacts. [Harbor](https://goharbor.io) solves this better than we could, unless concrete demand arises.

---

## Cache policy

- [x] Honour `Cache-Control` headers from upstream responses (`no-cache`, `max-age`, `no-store`) to decide whether and how long to cache
- [x] Eviction policies: TTL-based expiry, "not accessed for N days", garbage-collect all versions except the latest N, storage size cap with LRU eviction
- [x] Cache index coherence: compare what is actually in the storage backend against what the registry metadata expects, and recover from corruption or manual deletions
- [x] Content-addressable deduplication: identical artifact bytes are stored once regardless of how many logical keys (registries, package names) reference them — ref-counted via `artifact_dedup_index` / `artifact_dedup_refs`, backwards-compatible with pre-dedup artifacts
- [ ] **Storage-backend migration** — move stored artifacts from one configured backend to another (filesystem ↔ S3) when `[storage]` changes. Artifacts already record which backend holds them, but there is no migrate, move or rebalance operation, so changing the backend strands everything already written: it stays reachable only while the old backend remains configured. Needs a resumable walk, partial-failure tolerance and dedup ref-count awareness (RFC 0004-bis §13.3)
- [x] Proactive cache warming: pre-fetch known versions of configured packages on startup and on demand via the admin API (`POST /api/v1/admin/registries/{registry}/warm`); configurable per registry with `warm_packages`, `warm_latest_n`, and `warm_concurrency`

---

## Metrics & observability

- [x] Prometheus metrics endpoint (`/metrics`): request counts, cache hit/miss rates, latency percentiles, error rates per registry
- [x] Health check endpoint (`/healthz`) that verifies connectivity to the database and all configured storage backends
- [x] Stats dashboard on the admin home screen: hits/misses, bandwidth saved, per-registry and aggregate

---

## Artifact integrity & security

- [x] Verify checksums for downloaded artifacts when the upstream provides them — per-registry `[registries.integrity]`: on the proxy fetch path the buffered bytes are hashed and compared against the metadata checksum (Cargo SHA-256, npm SRI/`shasum`, PyPI SHA-256). Supports SRI (`sha512-…`) and bare hex (algorithm inferred from length)
- [x] Block serving an artifact if its integrity check fails, or optionally if the upstream provides no integrity metadata at all — a mismatch fails the download with `502` and is never cached (not bypassable); `require_metadata = true` additionally blocks downloads with no advertised checksum (with `bypass_roles`)
- [ ] Sigstore / npm provenance verification for npm packages
- [ ] `cargo verify-project`-style verification for Cargo crates
- [x] Detect and optionally require signed releases for GitHub, OpenVSX, and VS Code Marketplace — `RequireSignedReleaseRule` reads a best-effort `is_signed` signal populated by the GitHub/Forgejo and OpenVSX/VS Code Marketplace adapters; `deny_missing_signature` + `bypass_roles` control enforcement
- [x] Allowlist of trusted publishers — `trusted_publisher` rule; supported for GitHub/GitLab/Forgejo (owner/group), npm (scope or publishing user), OpenVSX/VS Code Marketplace (publisher segment). **Cargo crate owners not yet supported** — crates.io ownership isn't in the sparse index and would need a separate API call
- [x] Allowlist of approved versions; blocklist of specific versions with known issues — `version_gate` rule (`allow`/`block` with exact or semver-range matching)
- [x] Vulnerability scanning via the [OSV API](https://osv.dev) to block or warn about packages with known CVEs — periodic SBOM re-scan plus a per-registry `cve_gate` rule (`min_severity`, `block`/warn-only, `bypass_roles`)
- [x] **Licence policy rule** — `license_gate`: allow/deny SPDX expressions per registry, `allow_unknown` for the case where the licence is not yet known, `block` vs warn-only and `bypass_roles` like `cve_gate`. Distinct from RFC 0002's licence *flag kind*, which needs an external source to assert one; this reads what the package declares in its own manifest. Licence extraction landed with it: `ArchiveSbomExtractor` now returns the declared licence alongside the dependencies from the same parse, in five ecosystems (cargo, npm, maven, pypi, nuget), stored on `artifact_sboms.license` and surfaced on both the admin and explore package-detail pages. Two limits, both warned about in config rather than left to be discovered: the licence is read during **SBOM generation**, so the rule is inert unless `[registries.sbom].enabled = true` (`license-gate.sbom-disabled`); and it is inside the archive, so the first request for an uncached package cannot be gated (`allow_unknown`). A `license_gate` on one of the sixteen registry types with no parser raises `license-gate.no-extractor`, or `license-gate.denies-everything` when `block` + `allow_unknown = false` would refuse every download (RFC 0004-bis §13.1, §14.1)
- [ ] YARA rule evaluation for custom malware or policy patterns
- [ ] Antivirus scanning for binary artifacts (VSIX, Go module zips) via a configurable external REST API
- [x] Warn when an upstream registry is returning high error rates or slow responses and cached data may be stale — in-process EMA of upstream error rate / latency per registry (`ProxyMetrics`); a `batlehub_upstream_health_degraded{registry}` gauge plus a `tracing::warn!` on the healthy→degraded transition; also surfaced in `GET /api/v1/admin/stats` (`upstream_degraded`, `upstream_error_rate`, `upstream_latency_ms`)
- [x] Warn when a registry does not provide integrity metadata for its artifacts — the proxy logs a warning and increments `batlehub_integrity_checks_total{outcome="missing"}` when an artifact is fetched with no advertised checksum (and blocks instead when `require_metadata = true`)

---

## Authentication providers

- [x] **Static tokens** — plain-text and Argon2id-hashed Bearer tokens in `config.toml`; `batlehub hash-token` CLI
- [x] **OIDC** — JWT validation via OIDC discovery + JWKS; browser SSO (Authorization Code flow); role and group mapping from claims; namespaced group prefixes for multi-provider setups
- [x] **Kubernetes service accounts** — TokenReview API validation; role and group mapping; in-cluster defaults
- [x] **GitHub / Forgejo Actions OIDC** (`type = "actions-oidc"`) — validate short-lived JWTs issued to workflow jobs; map claims (`repository`, `ref`, `environment`, `actor`, …) to groups and roles via configurable rules; supports static group names and dynamic group templates (e.g. `"{name}/{repository}/{ref_name}"` → `"forgejo-action/batleforc-batlehub/main"`); glob and regex pattern matching; AND / OR condition logic per rule
- [x] **Groups on a personal access token** — a PAT resolves to its creator's subject and a *subset* of their groups. `user_tokens.groups` (migration 046) holds a snapshot taken at creation; `snapshot_pat_groups` (`crates/core/src/entities/grant.rs`) caps it to what the creator holds and refuses anything else with a `403` naming it, rather than dropping it silently; `UserTokenAuthProvider::to_identity` returns it, so RFC 0015's `group:` subjects match a PAT and `pat_is_within_owner` no longer compares against a set that can only be empty. The console picks the groups from the caller's own on `TokensPage.vue` and lists what each token carries — a snapshot goes stale silently, and that column is where an owner sees a team they have left; the CLI takes `--groups`/`--all-groups` and shows the snapshot in `auth token create` and `auth token list`. Omitting them means none, which is what every token minted before this carried. RFC 0011-bis phase 1, the one gap of the three that RFC 0015 absorbed by requirement and did not build (`docs/rfc/0011-bis-namespace-scoped-visibility.md`); 0011-bis's first open question closed with it — expiry is mandatory and capped at 90 days, which is what bounds how long a stale snapshot grants read

---

## Rate limiting & DoS protection

- [x] Per-user and per-registry rate limits on API requests and artifact downloads, with configurable thresholds and time windows (in-memory token bucket; state resets on restart)
- [x] Configurable enforcement policies: hard block vs. soft warn when a limit is reached
- [x] Explicit rate-limit warnings in API responses (`Retry-After`, `X-RateLimit-*` headers)
- [x] Per-group rate limits (shared token-bucket pools per OIDC/Kubernetes group; enforcement override per group)
- [x] IP-based blocking for abusive clients, with configurable block duration and thresholds
- [ ] Integration with external IP reputation services to automatically block known malicious IPs

---

## Quota management

- [x] Per-user, per-group, and per-registry quotas on storage usage and number of published packages
- [x] Enforcement policies: block publish requests that exceed the quota, or allow with an explicit warning
- [x] Quota warnings in API responses and admin UI when a limit is being approached
- [x] Admin API for resetting quotas for specific users, groups, or registries

---

## Hot reloading & dynamic config

- [x] Watch the config file for changes and prompt an admin for confirmation before applying — file watcher (using `notify` crate) loads a pending reload; admin confirms via `POST /api/v1/admin/config/pending/apply` or discards it
- [x] Validate the new config before applying it (schema check + connectivity probes) to avoid breaking a running server — schema validation runs immediately; connectivity probes (`HEAD` to each upstream with a 5s timeout) run before the pending reload is stored
- [x] Partial reloads: update RBAC rules or add/remove a registry without restarting the process — registries, policies, versioning, signing, and beta-channel maps are all behind `Arc<RwLock<HotConfig>>`; in-flight requests finish with the old data before the swap
- [x] API endpoint for triggering a config reload (`POST /api/v1/admin/config/reload`) for automation when file-watching is unavailable — also `GET /api/v1/admin/config/pending`, `POST /api/v1/admin/config/pending/apply`, `DELETE /api/v1/admin/config/pending`
- [x] Audit trail for all config changes (who triggered, what changed, when) — stored in `config_changes` table; retrievable via `GET /api/v1/admin/config/changes`
- [x] **Global admin banner** — broadcast a message (info / warning / error) to all website visitors; automatically set during a reload and cleared on completion; backed by in-memory, Redis, or PostgreSQL depending on the cache backend; `PUT/DELETE /api/v1/admin/banner` + `/admin/config-reload` UI page
- [x] `BATLEHUB_DISABLE_HOT_RELOAD=1` — disable the file watcher and all reload endpoints (e.g. when config is a read-only Kubernetes ConfigMap)
- [x] **Config warnings** — non-fatal problems (`AppConfig::warnings()`) logged at startup and on every reload, served from `GET /api/v1/admin/config/warnings`, returned inline by `/config/validate` and `/config/from-content`, and rendered on the Config Reload admin page. Each carries a stable `code` and the `path` of the offending config location. `/config/from-content` reports the *candidate's* warnings even when the submitted bytes match the last load attempt, rather than those of the config still in force
- [x] `pending_created` on the reload responses — `/config/from-content` answers `200` with an empty diff both when it stages a pending and when the submitted content is identical to the last load attempt and there is nothing to stage. The flag distinguishes them, and the Config Reload page shows a distinct notice instead of a success that sends the admin to an Apply button answering `404 No pending reload`
- [ ] **Clear the upstream-detail absence cache on reload** — `UpstreamDetailCoordinator` remembers which coordinates an upstream answered `404` for, and `clear_absent` exists to forget them. Its doc says "for tests, and for a config reload that changes what 'absent' would mean"; audited 2026-09-01, no reload path has ever called it. So a reload that adds an upstream, fixes its base URL or repairs its credentials leaves every coordinate that failed under the old configuration still remembered as missing, and the operator's fix appears not to work until the process restarts. One call on the reload path, and a test that a reload forgets an absence
- [ ] Dynamic blocking rules fetched from an external trusted source (e.g. a signed Git repository); verify signatures before applying
- [ ] Dynamic allowlists of trusted publishers or approved versions, fetched from an external source and merged into RBAC / block rules automatically

---

## Ingress & routing

- [x] **Host-based (subdomain) registry routing** — bind a registry to one or more hostnames whose root serves it, in addition to `/proxy/{name}/…`. `[subdomain_routing]` derives `<name>.<base_domain>` for every registry; `registries[].hosts` adds vanity hosts. One outermost middleware rewrites the URI, so none of the ~249 route definitions change; the host table is hot-reloadable like every other registry-scoped map (RFC `docs/rfc/0001-subdomain-routing.md`)
- [x] Every self-referencing URL reflects the ingress the client used — one `registry_public_base` helper replaced 8 ad-hoc base-URL builders, ~25 `format!("{base}/proxy/{registry}/…")` sites and the `base_url` contract of 5 `LocalRegistryService` methods
- [x] `registries[].path_routing = false` — make a registry reachable *only* by host, so isolated content does not also answer on the shared main host (404, not 403)
- [x] **Proxy trust** (`[server].trusted_proxies`) — one CIDR list governing `Forwarded` / `X-Forwarded-Host` / `X-Forwarded-Proto` / `X-Forwarded-For` alike, resolved once per request into a `PeerTrust` verdict every downstream middleware reads. Closes the previously unconditional trust of the forwarded host and scheme; deprecates `[ip_blocking].trusted_proxies`, whose unparseable entries are dropped with a config warning rather than refused at startup (that key predates the validator, so rejecting them would fail the boot of a config that never changed)
- [x] The policy is hot-reloadable and swapped *before* the host table it guards, so a reload that turns host routing on can never run under the startup verdict — the two are not atomic together, and this order makes the gap fail closed
- [x] The rate limiter and the IP-block middleware bucket and ban on the same client IP — both read the shared verdict, so behind a trusted proxy neither keys on the proxy address
- [x] Registry names are matched on the path actix routes on, not the raw URI — percent-encoding a character of the name no longer slips past the `path_routing = false` 404 or a registry's rate limit
- [x] `RegistryInfo.public_url` on `GET /api/v1/registries`, used by the Setup Guide and namespace upload snippets — the `.netrc` block lists every host a client may authenticate against (entries are matched by hostname, so the main host alone left host-routed registries unauthenticated), and the Terraform `source` drops the registry segment on a host-routed registry, where it is no longer part of the path
- [x] Helm `ingress.extraHosts` + documented `config.server.trusted_proxies`
- [ ] Per-host TLS certificate management inside the server (currently the ingress's job)
- [x] **Instance-to-instance transfer for air-gapped estates** — export a set of approved artifacts plus their metadata as a signed bundle, and import it into a disconnected instance. `[air_gap]` makes a server that will not dial out: a miss is a `503` that names itself and is recorded, so the next bundle's contents are produced by the estate rather than guessed. `batlehub mise plan|seed|export|import` turns `mise.lock` into the bill of materials, and the bundle is a tar of a signed manifest plus sha256-addressed blobs — so identical bytes ship once and the identity is the same one content-addressable dedup uses (RFC 0004-bis §13.2). Verification moves to the connected side and the verdict travels with the bytes. Proven end to end by `tests/heavy/mise.sh` §4: `mise install` against a second instance with egress denied (RFC `docs/rfc/0008-mise-in-an-air-gapped-estate.md`)
- [ ] A bundle carries artifacts and the metadata entry that finds them, not proxied *documents* — a release listing, a packument, a flat index. An install from a lock needs none; a client resolving a version on the disconnected side does, and gets a `503` recorded under the `document` kind (RFC 0008 §14.7)

---

## Webhooks & notifications

- [x] Subscribe to notifications for specific packages, versions, or registries (new version published, version deprecated, package removed)
- [x] Multiple notification channels: email, Slack, Microsoft Teams, outbound webhooks
- [x] User-configurable notification preferences and channel configuration in the UI
- [x] Inbound webhook API so external systems (CI pipelines, security scanners) can push events into BatleHub and trigger notifications or policy updates

---

## Private registry features

Applies to registries running in `local` or `hybrid` mode.

- [ ] **Seed a registry from an incumbent** — import the *contents* of an existing Nexus, Artifactory or Verdaccio, not just the config for a new instance (`batlehub-cli registry suggest` already does the latter). This is the migration path for the users most likely to adopt BatleHub, and without it every adoption starts from an empty cache. Needs a per-source adapter plus an ingestion path that reuses the existing publish machinery so quotas, ownership, signing and SBOM all still apply (RFC 0004-bis §13.4)

### Per-registry additions

- **npm** — versioning policies (enforce semantic versioning, allowlist version patterns)
- **Cargo** — versioning policies; verify full compatibility with the yank protocol from crates.io
- **VS Code extensions** — deprecation and unlisting; upload via the UI (form for VSIX + metadata), in addition to the existing `PUT` API
- [x] **Maven** — private artifact publishing via `mvn deploy`; POM-triggered three-phase publish; JAR/checksum pre-upload; dynamically generated `maven-metadata.xml` from DB; `local` and `hybrid` modes
- [x] **Terraform** — private module publishing (tar.gz upload, `X-Terraform-Get` redirect); private provider publishing (version manifest + per-platform binary upload); `local` and `hybrid` modes
- [x] **PyPI** — private wheel / sdist publishing via `twine`; Simple API served from DB; `local` and `hybrid` modes
- [x] **Conda** — private channel with `repodata.json` generation from DB; `.tar.bz2` and `.conda` package upload; `local` and `hybrid` modes

### For all private registry types

- [x] **Writing grants at the package and version tiers** (RFC 0017, `docs/rfc/0017-writing-grants-at-the-package-and-version-tiers.md`) — RFC 0015 built the two deepest tiers of the grant hierarchy and left them without an editor: the only `put_grant` caller was the ownership projection, which writes *package* rows carrying exactly `releases:publish` + `owners:read` + `owners:write`, so `releases:read` on one package for one group — RFC 0015 §4.4's own opening example — was unexpressible and the version tier, read on every `authorize`, could not be populated at all. Both halves shipped in one change, which the RFC requires rather than recommends: `grants:read`/`grants:write` in the vocabulary and held by `role:admin` at the instance tier, `GrantAdminService` as the single write funnel (expansion at write, the ownership-verb `409`, the redundancy and spent-coordinate warnings), three admin routes, `batlehub admin grants list|set|rm`, and a console panel on the package detail page — **and** `filter_by_grants` in `load_visible_versions`, which gave `filter_listing` and `package_visibility` their first caller. Rule 3 landed with them: `DocumentAudience` now names the caller's version-tier grants, because the rubygems compact index filters every version of every gem and two callers differing by one version row are entitled to different documents. Inert until an operator writes the first version-tier grant, which is asserted rather than assumed. Two corrections to the RFC. Its before/after example described a version grant *narrowing* what another subject sees, which §4.3's "grants only widen" forbids — the filter is what a `releases:list`-without-`releases:read` caller sees, and that is now what the document says. And §6.3's "`explain` needs no change, it resolves through the same path" was false: `resolution_path` composes the config-declared tiers, and the stored ones are appended by `authorize_grants` after a short-circuit no diagnostic reaches, so `explain` and `access-check` both answered *deny* where the server answers *allow* — `explain` while naming `package:…`/`version:…` in `tiers_walked`. Both now take `resolution_path_for_coordinate`; the enforcement path keeps its short-circuit, because grants union and the two agree. A third defect was found by Bundler rather than by any test: every whole-registry document gates each name on `Readable::contains`, which composed the config and package tiers and stopped — so a caller whose only grant was a version-tier row was told the package does not exist, and the per-version filter was never reached for it. `/info/{gem}` filtered correctly throughout, which is why nothing caught it: `curl` asks `/info` directly and Bundler does not, it resolves from `/versions`, and an empty one sends it to the legacy `specs.4.8.gz` index this server answers `404`. Failed closed, so no disclosure — but §4.4 rule 2 was unreachable in practice. `Readable::with_version_grants` admits the name; `tests/heavy/authz.sh rubygems` now asserts `bundle install` resolves to the granted `2.0.0` while `3.0.0` exists and is newer
- [x] Artifact signing framework: publish with `X-Artifact-Signature` / `X-Signature-Type` headers; signature stored in DB and returned on download; optional `signing.required` enforcement. Optional download-time verification of stored `ed25519` signatures against configured `signing.trusted_keys` (`signing.verify_on_download`); Ed25519 only, since the `rsa` crate is banned
- [x] Ownership and team management: per-package owner table (user/group, admin/maintainer roles); `initialize_owner` on first publish; `can_publish` check on subsequent publishes; admin API to list/add/remove owners
- [x] Versioning policies: `enforce_semver`, `allow_prerelease`, `version_pattern` (regex) per registry; enforced at publish time with HTTP 422
- [x] Beta / pre-release channel: allow specific users or groups to access unpublished versions before general release
- [x] Bulk operations: `POST /api/v1/admin/registries/{registry}/bulk-yank|bulk-unyank|bulk-delete`
- [x] **A published version coordinate is never reused** (RFC 0016): delete is a soft delete, so `1.4.0` cannot mean two different things to two different lockfiles. The bytes go, a tombstone stays, and a re-publish onto the coordinate is refused for good — the crates.io/PyPI model rather than npm's, which matters more here because a private registry is frequently the only copy of what it holds. A package *name* is released when its last version goes, its owners with it; the version numbers stay spent
- [x] **Retention for locally published versions** (RFC 0016): `keep_versions`, `keep_for_days`, `keep_if_pulled_days` as a union of vetoes — a version survives if *any* matches, so wrong configuration fails toward keeping. `dry_run` on by default, a per-version pin that outranks the policy, a floor date before which an absent download record proves nothing, and a rate limit for the first live run. Distinct from cache eviction, which discards something a re-fetch brings back
- [x] Tombstone compaction: a deleted version's checksum, publisher and metadata age out on a window; the coordinate claim never does, and there is deliberately no setting that removes it
- [x] Content-addressable deduplication for stored artifacts (ref-counted via `artifact_dedup_index` / `artifact_dedup_refs`)
- [x] Integrity verification: verify checksums on re-serve, not only at publish time — `integrity.verify_on_serve` re-hashes stored bytes against a self-computed SHA-256 (recorded when first cached) on every serve (proxy cache hits and local reads); a mismatch fails with `502` and evicts the corrupt entry

### CLI tool - `batlehub-cli`

- [x] a CLI for common private registry tasks (`publish`, `yank`, `list`), suitable for use in CI pipelines — `batlehub-cli` binary in `cli/`; global flags `--profile`, `--server`, `--token`, `--registry`, `--json` and env-var equivalents (`BATLEHUB_*`)
  - [x] Publish command that wraps the upload API, with support for multiple registry types and automatic metadata extraction from the artifact (e.g. extension, archive contents, manifest files) — `batlehub-cli publish <file>`; `detect_meta` auto-detects registry type, name, and version
  - [x] Version management commands for yanking, unyanking, or deleting specific versions — `batlehub-cli version yank|unyank|delete`
  - [x] Package management commands for listing versions, viewing metadata, or managing owners — `batlehub-cli package list|versions` and `batlehub-cli owners list|add|remove`
  - [x] Authentication support for static tokens and token management — `batlehub-cli auth whoami` and `token list|create|revoke`; token passed via `--token` / `BATLEHUB_TOKEN`
  - [x] List of available registries and their types, with per-registry configuration details — `batlehub-cli registry list|info`
  - [x] Suggest the set of registries a project needs — `batlehub-cli registry suggest` scans the working directory (`mise.toml`/`mise.lock` tool backends plus the usual manifests: Cargo.toml, package.json, go.mod, …), maps each detected source to a registry type, and emits the `[[registries]]` TOML block (or JSON) to paste into `config.toml`. `--client-env` additionally prints the environment variables that point each toolchain at BatleHub
  - [x] List packages and versions in a registry, with filtering options — `batlehub-cli package list|versions`
  - [x] Autocompletion support for shell integration — `batlehub-cli completion bash|zsh|fish|...` generates and prints the completion script; pipe to shell RC file
  - [x] Config file support for storing credentials and default options, with CLI overrides — `~/.config/batlehub/config.toml` with named profiles; `batlehub-cli config init|show|set`
  - [x] Config file output for both CI automation and human use — `config init` interactive wizard; `--json` flag on all commands for machine-readable output
- [x] A TUI mode for interactive use — `batlehub-cli tui` launches a `ratatui` / `crossterm` terminal UI
  - [x] List of registries with search and filter capabilities — `registry_list` screen
  - [x] Per-registry package explorer with version details and management actions — `package_list` screen (live search/filter) + `package_detail` screen (yank / unyank keybindings)
  - [x] Interactive prompts for publishing new versions — `publish_form` screen with auto-detected name and version fields
  - [x] Help setup registry for a current project by scanning local files — TUI `SetupWizard` screen (`s` from registry list); detects Cargo.toml, go.mod, package.json, pyproject.toml, pom.xml, composer.json, *.gemspec, *.nuspec, *.csproj, *.tf, environment.yml; shows per-type config snippets and publish commands
  - [x] Auth workflow integration for OIDC and Kubernetes service accounts, including token caching and refresh — `batlehub-cli auth login` (OIDC browser flow + K8s token path); `auth refresh`; `oidc_refresh_token` / `oidc_expires_at` / `kubernetes_token_path` persisted in profile; auto-refresh on startup; TUI `Login` screen (`L` from registry list) with three-tab method selector

---

## SBOM support

- [x] Proxy existing SBOMs from upstreams that provide them (GitHub dependency graph API, npm `bom.json`) — enabled by `fetch_upstream = true` in `[registries.sbom]`
- [x] Generate a minimal per-artifact SBOM (SPDX 2.3 or CycloneDX 1.4) at proxy time, from registry metadata and the downloaded archive
- [x] Org-level SBOM export: all artifacts served in a time range as a single merged document (`GET /api/v1/sbom/export?from=…&to=…&format=spdx|cyclonedx`) — admin UI at `/admin/sbom`
- [x] Generate SBOMs at upload time for private registries, extracting dependency manifests from the archive (`go.mod`, `Cargo.toml`, `package.json`, `pom.xml`, `requirements.txt`)
- [x] Policy option: deny publishing a private package if no manifest is found in the archive (`required = true` in `[registries.sbom]`)
- [x] Per-artifact SBOM accessible from the Package Explorer version detail view (SPDX and CycloneDX download buttons per version)
- [x] Periodically re-check cached SBOMs against vulnerability databases (see [Artifact integrity](#artifact-integrity-security)) and update block / warn metadata automatically — `[vulnerability_scan]` background task queries OSV and records findings, surfaced per-version in the Package Explorer and admin views
- [ ] **Third-party vulnerability flags** — a declared external source (SOC, vendor feed) pushes version flags over an HMAC-authenticated, idempotent API, stating what it asserts (`kind`: cve, malware, compromised release, licence…) separately from how hard BatleHub reacts (`effect`: inform / warn / gate / hard_block). Per-source `max_effect` ceiling; `hard_block` denies regardless of the registry's CVE-gate settings and does not fail open (RFC `docs/rfc/0002-vulnerability-flags-and-exposure.md`)
- [ ] **Vulnerability exposure reporting** — join the download audit trail with pushed flags and scanner findings alike, so an admin can ask "which consumers pulled a flagged version in the last N days", including the retroactive case where the advisory landed after the pull. Admin API + CLI + UI panel, read-only (same RFC)

---

## UI improvements

- [x] **Package explorer** (`/explore`) — collapsible catalog with registry sidebar; search and sort across all cached and upstream packages; per-package detail page showing version history with gate/firewall status per version; `[registries.rbac.explore]` config block for independent search permissions
- [x] Package explorer caching and pagination for large registries (e.g. npm) to avoid fetching the entire index on every request; cache invalidation on new versions published or cache expiry
- [x] Package detail pages with version history and per-version download links (proxy URL constructed per registry type — cargo, npm, nuget, rubygems, pypi, conda, vsix)
- [x] User listing and block management in the admin panel (OIDC and Kubernetes-sourced identities, not just static tokens)
- [x] Config editor with validation and apply button (integrates with hot reload)
  - [x] Read-only warning when the config file is mounted from a Kubernetes ConfigMap, with instructions for applying changes externally
- [ ] **Web console redesign** — full rework of `ui/`: one catalog surface and one canonical package URL (today `/packages` and `/explore` duplicate the job, with two different detail URL shapes), an identity-driven shell, a designed first run for empty instances, four first-class list states (empty / filtered-empty / error / denied), a scope-and-count contract on destructive actions, one token source shared with `docs/`, WCAG 2.2 AA, and French/English localisation (none exists today — every string is hardcoded English). Visual identity is reopened; the name `BatleHub.` and the Monofolio lineage stay binding. Carried out with the [Impeccable](https://impeccable.style) design skill as the working method — `PRODUCT.md` is written, `DESIGN.md` is the first deliverable, and its deterministic detector becomes a CI gate (RFC `docs/rfc/0003-ui-rework.md`)
- [x] **Admin composition and the API surface the console is missing** — the follow-on RFC 0003 deferred, in five phases. Every documented `200`/`201` declares a body, enforced by a gate that walks the generated `ApiDoc` (136 responses carried only a `description`, so generated clients emitted `unknown` and the docs site's API reference was blank), `ui/src/lib/registry-types.ts` and its four hand-written DTO mirrors are gone, and `GET /api/v1/me/{quota,downloads,advisories}` give a developer their own usage, pulls and advisories without an admin — scoped in the ports rather than the handlers, so no future caller can assemble the query without the filter. `/` gained the quota meter and the advisories widget on what you recently pulled; the dashboard's trend reads a persisted hourly rollup rather than counters that reset with the process, under a `[stats]` block that also makes `/metrics` switchable. All fifteen admin pages went through an Impeccable pass with authority to keep, update, split, merge, remove or add, one verdict per page; the three verdicts it only partly discharged were finished by RFC 0004-bis (RFC `docs/rfc/0004-admin-composition-and-api-surface.md`)
- [x] **What RFC 0004 left, and the gates that could not see it** — the residue RFC 0004 discovered and deliberately deferred, in six phases. Three gates that reported green over conditions they could not observe now observe them: the i18n audit reads component props and `<script>` assignments rather than an enumerated attribute list, the catalogue test asserts every key is *referenced* (94 were not), and one merged gate measures the type ramp and display face on **every** rendered route instead of `/admin/*` only. The access-check simulator consults the account and IP block stores it used to ignore, so it stops answering `allow` for an account the adjacent page shows as blocked. Seven API gaps closed and one (`A5`, a config revert path) declined with its reason recorded. Every admin page gained the component test it never had, the explore cache stopped outliving the identity it was filled for, and fourteen free-text fields that named something the server already knew became `Select`s or a real combobox. §13 adds four product gaps nothing was tracking, of which the licence gate is built and the other three are specified. `/packages` stays pinned red against its own design proof until O3 decides which side moves — a visible disagreement rather than a green gate over a page nobody was comparing to anything (RFC `docs/rfc/0004-bis-what-rfc-0004-left.md`)

---

## Testing

- [~] Unit tests for all registry adapters and policy evaluation logic — significant coverage added (entities, services, auth, storage router, registry adapters, web middleware, handler guards); ≥80% line coverage enforced by `task coverage-check`; 2422 tests across 69 suites as of last run (1405 of them lib unit tests); recent additions cover the percent-encoded-registry bypass on both the `path_routing = false` 404 and the rate limiter, `ProxyTrust::replace_from` reaching every clone, and the two `pending_created` branches of `load_pending_from_content`
- [x] CLI test suite — 23 unit tests (`parse_oidc_paste`, `is_token_expiring_soon`, `detect_project_types` for all 9 manifest types) + 16 integration tests (registry, package, version yank/unyank/delete, publish, auth, shell completion, Kubernetes login); fixed `InMemoryLocalRegistry` case-sensitivity bug so yank/delete tests pass end-to-end
- [x] **Protocol conformance fixtures — the client's paths, not ours** — `crates/web/tests/protocol_conformance.rs`: one table per ecosystem of the literal request lines a real package manager sends, each carrying the source it was read from, asserted to route; plus a `must_find` class for collection endpoints, because a route that returns an empty `200` is indistinguishable from a stub by every other signal. Unserved paths carry `not_yet("phase N")` and assert the opposite, so the list is a published inventory that can only shrink. **This is the priority item in this section**, and RFC 0009 is the argument for it: six findings, every one of them shipped green — four endpoints at addresses no client calls, one whose default client reads the single index we do not filter, and one stub the service index advertises as a feature. In each case a test existed and passed, because it was written from the implementation and could not discover that the implementation answers the wrong question (RFC `docs/rfc/0009-protocol-coverage.md` §5)
- [x] Integration tests against real upstream registries (gated, opt-in) — the discharge for RFC 0009 §12: the conformance tables encode what we *believe* each client calls, and until a real `npm audit` and `bundle install` are captured against a live instance, that belief is reasoning rather than evidence. **Captured, and it was not reasoning**: seven ecosystems were run against a real server with real clients (npm, bundler, terraform, dotnet, micromamba, composer, pip, ovsx), finding twelve shipped bugs that every test in the repository passed (RFC 0009 §12). **Now scripted, all of them**: `tests/heavy/{bundler,npm,pypi,openvsx,conda,nuget,composer,terraform}.sh` plus `marketplace.sh` — each starts a real server behind a logging tap, drives the real client and asserts on the wire (`task test:heavy`, or one `task test:<ecosystem>-heavy`). They run in CI as `heavy-bundler`, `heavy-marketplace` and the `heavy-client` matrix, with the server under `cargo llvm-cov` so the client's path counts toward coverage, and nightly because the clients come from outside this repository. Writing the scripts found **six more shipped bugs** in one pass, none of them a wrong path (RFC 0009 §12.16)
- [ ] Broader fuzzing targets beyond the current four (RBAC, cache key, deny-latest, release age)
- [x] Cover code with [SonarCloud](https://sonarcloud.io/project/overview?id=batleforc_batlehub) — `.github/workflows/sonar.yaml` runs frontend (vitest lcov) + backend (cargo-llvm-cov lcov, with Postgres/MinIO/Redis services) and uploads both reports to SonarCloud on every push to `main`
