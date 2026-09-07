---
reference: true
---

# Testing

This document describes how BatleHub is tested: the categories of automated
tests, what each layer covers, how to run them, and — most importantly — **what
is currently exercised by the integration suite**. It is a map, not a tutorial;
for how to add a test when you add a registry, see
[`adding-a-registry.md`](adding-a-registry.md) § Testing.

> Test-function counts in this document are grep-derived snapshots
> (`#[test]` / `#[tokio::test]` / `#[actix_web::test]`) and drift as the suite
> grows. Treat them as orders of magnitude, not contract. The file lists and the
> *shape* of coverage are the stable part.

## 1. Test taxonomy

BatleHub's tests fall into six layers, in increasing order of infrastructure cost:

| Layer | Where | Infra | Runner |
|-------|-------|-------|--------|
| **Unit** | `#[cfg(test)] mod tests` inline in each source file | none (HTTP upstreams mocked with `mockito`) | `cargo test --workspace --lib --bins` |
| **In-process integration** | `crates/web/tests/*.rs`, `crates/examples/tests/*.rs` | none — full actix app on in-memory backends | `cargo test -p batlehub-web --test '*'` |
| **CLI subprocess integration** | `cli/tests/integration.rs` | none — CLI binary vs. in-memory actix server | `task test:cli:integration` |
| **External integration** | `crates/adapters/tests/*.rs` | real Postgres / MinIO(S3) / Redis via Podman | `task test:pg-*`, `task test:s3` |
| **Heavy client** | `tests/heavy/*.sh` | real Postgres **and a real client** — VS Code, IntelliJ, Bundler, npm, pip, ovsx, micromamba, dotnet, composer, terraform, nvm, mise, cargo, go, mvn, apt/dnf | `task test:heavy`, or one `task test:<ecosystem>-heavy` |
| **Heavy authorization** | `tests/heavy/authz.sh` | real Postgres, grants from a **real config file**, and the same clients | `task test:authz-heavy`, or `task test:authz-matrix-heavy` for the fast half |
| **Fuzz** | `fuzz/fuzz_targets/*.rs` | nightly toolchain to *run*, none to check | `task fuzz:check`, `task fuzz` |
| **Editor patch** | `patches/che-code/*.test.ts` | none — Node strips the types itself | `task test:patch` |
| **API contract** | `crates/web/tests/openapi_contract.rs` | none — walks the generated spec *and* the handler sources | `cargo test -p batlehub-web --test openapi_contract` |

The in-process layer is the workhorse: every test there spins up a real
actix-web application wired to `InMemoryPackageRepository`,
`InMemoryStorageBackend`, `InMemoryCacheStore`, and `FixedRegistry`, so it
exercises the true request lifecycle (auth middleware → handler → service →
rules → storage) without any external dependency.

---

## 2. Running the tests

```bash
# Everything that needs no external infra
cargo test --workspace

# One package / one filter
cargo test -p batlehub-web namespaces
cargo test -p batlehub-adapters --lib rbac

# In-process integration only
cargo test -p batlehub-web --test '*'
cargo test -p batlehub-cli --test integration

# batlehub-cli (`--bins`, not `--lib`: the crate is binary-only)
task test:cli                 # unit + subprocess integration
task test:cli:unit            # inline unit tests only, seconds
task test:cli:integration     # the built binary vs. an in-memory server
task test:cli:lint            # clippy -D warnings + fmt --check
task test:cli -- setup_detect # any of the above take a filter after `--`

# External integration (each starts its own container via Podman)
task test:pg-cache            # Postgres — PgCacheStore
task test:pg-local-registry   # Postgres — PostgresLocalRegistry
task test:pg-storage-router   # Postgres — StorageRouter
task test:pg-artifact-meta    # Postgres — PgArtifactMetaRepository
task test:pg-vulnerability    # Postgres — PgVulnerabilityRepository
task test:pg-air-gap          # Postgres — the miss log's upsert, cap and purge (RFC 0008)
task test:patch               # the editor credential patch (RFC 0011) — plain `node --test`
task test:s3                  # MinIO    — S3StorageBackend (feature storage-s3)

# Repo interop (real apt/dnf/pacman consume signed repos)
task test:repo-interop

# Heavy client integration (needs DATABASE_URL; each drives a real client)
task test:heavy               # every suite below except the marketplaces
task test:marketplace-heavy   # headless VS Code + IntelliJ install an extension
task test:bundler-heavy       # `bundle install` resolves through the compact index
task test:npm-heavy           # publish/install/whoami/dist-tags/search + `npm audit`
task test:pypi-heavy          # `twine upload`, `pip install`, PEP 658 metadata
task test:openvsx-heavy       # `ovsx publish` (query-parameter token) + `ovsx get`
task test:conda-heavy         # micromamba: the HEAD probe and a post-warm publish
task test:nuget-heavy         # `dotnet nuget push` / `package search` / `add package`
task test:composer-heavy      # local + proxy resolution with Packagist disabled
task test:terraform-heavy     # `terraform init` over TLS, host-routed discovery
task test:nvm-heavy           # `nvm ls-remote` / `nvm install` against a nodedist registry
task test:sdkman-heavy        # `sdk list` / `sdk install java` against an sdkman registry (RFC 0010)
task test:mise-heavy          # `mise install github:…` through a forge registry (RFC 0019),
                              # then the whole air gap: plan, seed, export, import into a
                              # second `[air_gap]` instance, install through it (RFC 0008)
task test:cargo-heavy         # RFC 0018 §4.4: yanked mark, 403/404 refusal, recovery, `cargo publish`
task test:go-heavy            # …same axes for `go`, plus the GOPROXY `direct` fallback and the sumdb
task test:maven-heavy         # …same for `mvn`, incl. its cached failure and `deploy:deploy-file`
task test:pathproxy-heavy     # …same for `apt` and `dnf`, whose signed indexes cannot hide anything
task test:quarantine-heavy    # RFC 0018 from npm's side: a hold, the worker clearing it, a real OSV
                              # (and RFC 0002: a pushed flag refusing npm, then revoked)
                              # advisory refused, the listing agreeing, warn mode, the sandbox's egress
task test:upstream-audit-heavy # RFC 0014 from the receiving end: a served directory loses a package,
                              # two probes confirm it, a webhook receiver the suite runs gets the event
task test:authz-matrix-heavy  # every verb in the vocabulary, both directions, over curl
task test:authz-heavy         # …plus signed-URL expiry/rotation and each real client

# Coverage (starts Postgres + MinIO + Redis; HTML report in coverage/html/)
task coverage
task coverage-check           # fails if line coverage < 80%

# Fuzz
task fuzz:check               # every target still compiles — the per-PR gate
task fuzz TARGET=fuzz_deny_latest MAX_TIME=30   # actually fuzz one (nightly)
```

The Redis adapter tests (`redis_cache`, `redis_rate_limit`,
`redis_warm_coordinator`) and `pg_rate_limit` / `actions_oidc` have no dedicated
`task test:*` wrapper but run under `task coverage` and in CI.

---

## 3. Unit tests

Unit tests live inline (`#[cfg(test)] mod tests`) in the file they cover. The
notable convention is **registry-client tests**: they mock the upstream HTTP API
with `mockito::Server` rather than hitting the real registry.

Registry adapters with test modules under `crates/adapters/src/registry/`:

- **Standalone `<name>/tests.rs`** (used when tests span more than one sibling
  file): `composer`, `conda`, `jetbrains_marketplace`, `pypi`, `rubygems`,
  `terraform`.
- **Inline `mod tests`**: `cargo`, `npm`, `openvsx`, `goproxy`, plus the shared
  infrastructure modules `fanout`, `http_client`, `path_proxy`, `ssrf`; and the
  directory clients `forgejo/client.rs`, `github/client.rs`, `gitlab/client.rs`,
  `maven/models.rs`, `nuget/client.rs`, `vscode_marketplace/client.rs`.

Every registry adapter has a test module. The `ssrf` and `http_client` modules
additionally cover SSRF protection and the shared upstream HTTP client
(auth forwarding, TLS, private-CA support).

---

## 4. In-process integration tests

`crates/web/tests/*.rs` — **88 files, 1 325 test functions** (counted 2026-09-06).
Shared app-factory infrastructure (`make_app`, `make_local_svc`,
`access_config*`, `LocalRegistryAppParts` / `build_local_registry_app`) lives in
`crates/web/tests/common/mod.rs`; every other file begins with
`mod common; use common::*;`.

Feature areas covered (file → area):

| File | Area |
|------|------|
| `proxy_basic.rs` | Core proxy: cache-first read, stale-on-error, streaming |
| `proxy_npm_edge_cases.rs`, `proxy_cargo_edge_cases.rs` | npm / cargo proxy edge cases |
| `proxy_openvsx_vscode_goproxy.rs` | OpenVSX / VS Code Marketplace / Go proxy |
| `cargo_and_downloads.rs` | Cargo proxy paths + download counting |
| `generic_proxy.rs` | Generic file-mirror `GET /proxy/{reg}/generic/{path}` |
| `repo_deb_rpm_pacman.rs` | Deb / RPM / Pacman path repositories |
| `terraform.rs` | Terraform modules + providers (v1 API) |
| `namespaces_and_visibility.rs` | Namespace claim/release + package visibility |
| `admin_packages.rs`, `admin_stats.rs`, `admin_health_and_bulk.rs`, `admin_access_check.rs` | Admin API: packages, stats, health, bulk ops, access-check |
| `bulk_and_quota_and_cache.rs` | Bulk operations, per-registry quotas, cache clear/warm |
| `tokens_and_pagination.rs` | API token CRUD + list pagination |
| `rate_limit.rs` | Rate-limiting middleware |
| `ip_blocks.rs`, `user_blocks.rs` | IP block enforcement, user block/unblock |
| `dynamic_groups.rs` | Dynamic group membership / RBAC groups |
| `beta_channel.rs` | Beta / pre-release channel gating |
| `banner_and_config_reload.rs` | Service banner + hot config-reload endpoint |
| `notifications.rs` | Notification channels / dispatch |
| `explore.rs` | Package Explorer discovery backend |
| `vuln_proxy_endpoints.rs`, `vuln_findings.rs` | Vulnerability proxy endpoints + findings store |
| `sbom_and_misc.rs` | SBOM read endpoints |
| `publish_traversal_guards.rs`, `upload_traversal_and_enforcement.rs` | Cross-registry publish/upload traversal guards + policy enforcement |
| `air_gap.rs` | RFC 0008: an instance that will not dial out, and what it answers instead |
| `security_registry.rs`, `flags.rs` | RFC 0018 quarantine end to end; RFC 0002 pushed flags on a registry with no `[security]` |
| `vsx_signing.rs` | RFC 0020: the VSIX signature asset the registry signs |
| `forge_refs.rs`, `forge_security.rs`, `forge_api_reads.rs` | RFC 0019 phases 1–3: ref resolution and commit keying, what a ref does to a request, the raw policy and typed reads |
| `authz_matrix.rs`, `authz_explain_oracle.rs`, `vocabulary_dead_ends.rs` | The route-by-route authorization matrix, `explain` agreeing with the decision, and RFC 0015 §11.5's no-dead-ends property |
| `grants_editor.rs`, `grants_shadow.rs`, `gate_exemptions.rs` | RFC 0017's grants editor, shadow mode, and the `gates:exempt` verb |
| `admin_policy.rs`, `admin_subjects.rs`, `tiered_versioning.rs` | The policy table's admin API, `GET /admin/subjects`, and `immutable` / `monotonic` on publish |
| `local_read_authorization.rs` | Per-package visibility on the artifact routes that read storage directly |
| `blocked_versions_hidden*.rs` | **Twelve files, 91 tests.** One property per ecosystem: a blocked version disappears from the *listing*, not only from the download, and whatever the protocol calls "newest" is repaired |
| `tombstones.rs`, `upstream_audit.rs`, `listing_audit.rs` | RFC 0016 coordinate reuse, RFC 0014 confirmed disappearance, and what a listing writes to the audit trail |
| `explore_fetch.rs`, `explore_upstream_detail.rs` | The Explorer's fetch-this-version button, and the package page for something this instance holds nothing of |
| `package_readmes.rs`, `readme_images.rs`, `readme_search.rs`, `search.rs` | README capture, the image endpoint's refusals, README search, and search across the five ecosystems that share one path |
| `host_routing.rs`, `spa_csp.rs`, `oidc_sso.rs`, `me_endpoints.rs` | RFC 0001 subdomain routing, the console's CSP, the browser sign-in flow, and the caller-scoped `/me` reads |
| `upstream_calls_are_cached.rs`, `document_cache_audience.rs` | Two invariants rather than features: every outbound call goes through the caching helper, and one caller's document is never replayed to another |
| `protocol_conformance.rs`, `vscode_gallery.rs`, `misc_standalone_endpoints.rs` | The paths clients actually send, BatleHub as an editor marketplace, and the endpoints that belong to no group |

---

## 5. Per-registry local-registry tests

Each registry that supports **local/hybrid** mode has a dedicated
`local_<type>_registry.rs` file with a `make_local_<type>_app(mode)` factory and
a publish-payload helper. These assert the full private-registry lifecycle:
publish (with anon-403 / proxy-404 / duplicate-409 rejections), download, the
registry-specific metadata endpoints, yank/unyank/delete, and hybrid
local-vs-proxy precedence.

| File | Highlights |
|------|-----------|
| `local_cargo_registry.rs` | Sparse index, download, yank/unyank, unlist, admin-gated deprecate, owners, hybrid precedence |
| `local_npm_registry.rs` | Packument, version metadata, tarball download, name + version traversal guards |
| `local_maven_registry.rs` | PUT pom (version-mismatch-400 / dup-409), jar-before-pom, `maven-metadata.xml`, proxy-mode rejection |
| `local_nuget_registry.rs` | v3 service index (+ vulnerabilities resource), `X-NuGet-ApiKey` auth, flat-index, registration/catalog, search |
| `local_composer_registry.rs` | `packages.json`, p2 metadata (local/proxy/hybrid fallback), dist streaming, invalid-zip-422 |
| `local_go_registry.rs` | `@v/list`, `.info`, `.mod` extraction, `.zip` download, `@latest` |
| `local_vsx_registry.rs` | VSIX publish + download-after-publish (shared by OpenVSX & VS Code Marketplace) |
| `local_jetbrains_marketplace_registry.rs` | Plugin publish (jar / nested-zip / descriptor validation), `updatePlugins.xml` build filtering, search, compatible-updates, offline/stale serving |
| `local_rubygems_proxy.rs` | Proxy-mode gem download, info, versions, specs (full/latest/prerelease); publish/yank return 404 in proxy mode |
| `local_rubygems_compact_index.rs` | The compact index (`/versions`, `/info/{gem}`) served from a local registry — what Bundler actually reads |
| `local_nodedist_registry.rs` | The `nodejs.org/dist` tree as a typed registry (RFC 0010 phases 2–3) |
| `local_sdkman_registry.rs` | SDKMAN as a typed registry (RFC 0010 phases 5–6): candidates, the broker, the post-install hook |

Additional local-registry coverage (Deb, RPM, Pacman, Conda, PyPI, Terraform)
lives in the feature-area files above (`repo_deb_rpm_pacman.rs`, `terraform.rs`,
and the proxy/upload files).

---

## 6. Path-traversal guard tests

Rejecting `..` and path separators in package coordinates before they reach a
storage key is a **hard requirement** for every registry (see the security note
in `CLAUDE.md` and `adding-a-registry.md`). The canonical regression is
`<type>_publish_traversal_version_returns_400`, which publishes with
`version = "../../etc/x"` and asserts `400`.

Traversal guards are currently tested for: **cargo, composer, conda, deb,
generic, jetbrains-marketplace, maven, npm (name + version), nuget (id +
version), openvsx, pacman, pypi, rpm, rubygems, terraform (provider artifact
path)** — plus a delete-cached-artifact traversal case. New registries **must**
add the matching test.

---

## 7. External integration tests (real infra)

`crates/adapters/tests/*.rs` — these need real infrastructure, opt in via
environment variables (`DATABASE_URL`, `S3_TEST_ENDPOINT`, `REDIS_URL`), and
**skip gracefully** when the variable is unset.

| File | Infra | Verifies | Task |
|------|-------|----------|------|
| `pg_cache.rs` | Postgres | `PgCacheStore` | `test:pg-cache` |
| `local_registry.rs` | Postgres | `PostgresLocalRegistry` (publish/yank/delete) | `test:pg-local-registry` |
| `artifact_meta.rs` | Postgres | `PgArtifactMetaRepository` | `test:pg-artifact-meta` |
| `storage_router.rs` | Postgres | `StorageRouter` | `test:pg-storage-router` |
| `pg_vulnerability.rs` | Postgres | `PgVulnerabilityRepository` | `test:pg-vulnerability` |
| `pg_rate_limit.rs` | Postgres | `PgRateLimitStore` | coverage / CI |
| `s3_storage.rs` | MinIO / S3 (`storage-s3`) | `S3StorageBackend` | `test:s3` |
| `redis_cache.rs` | Redis (`cache-redis`) | `RedisCacheStore` | coverage / CI |
| `redis_rate_limit.rs` | Redis (`cache-redis`) | `RedisRateLimitStore` | coverage / CI |
| `redis_warm_coordinator.rs` | Redis (`cache-redis`) | `RedisWarmCoordinator` | coverage / CI |
| `actions_oidc.rs` | none (mockito) | GitHub-Actions OIDC bootstrap / discovery / JWKS failures | CI |
| `selfhosted.rs` | none (mockito + in-test TLS) | private-CA HTTPS upstreams, `UpstreamHttpOptions` (bearer/basic auth, TLS) | CI |
| `repo_interop.rs` | none at test time (`#[ignore]`d generator) | writes a fully-signed Deb + RPM + Pacman repo with production signing code, consumed by `tests/interop/verify.sh` | `test:repo-interop` |

The **repo-interop** flow is notable: `repo_interop.rs` generates signed
repositories using the real signing code path, and `tests/interop/verify.sh`
then has genuine `apt`, `dnf`, and `pacman` clients consume and verify them —
end-to-end proof that the OS-package output is standards-compliant.

---

## 7-bis. Heavy client tests (a real package manager)

`tests/heavy/*.sh` — each one starts a real BatleHub against a real Postgres,
puts a transparent logging proxy (`http_tap.py`) in front of it, drives that
ecosystem's **real client**, and asserts on the wire transcript. Shared
machinery is in `tests/heavy/lib.sh`.

### The config generator's harness

The docs site's config generator (`docs/.vitepress/components/ConfigGenerator.vue`)
writes TOML nobody used to parse. Its pure half — types, defaults, helpers and
`renderConfigToml(state)` — lives in `configToml.ts` so it runs without Vue, and
three things hold it:

- `node --test docs/build/config-generator.test.ts` renders the scenarios of
  `config-generator-scenarios.ts` and asserts on the sections (plain Node, types
  stripped by Node itself);
- `docs/build/config-generator-fixtures.ts` writes each rendering to
  `crates/config/tests/fixtures/config-generator/*.toml`, and
  `cargo test -p batlehub-config --test config_generator_fixtures` loads every
  one with `load_from_str` and `validate()` — the real parser saying the
  generator emits a config the server accepts;
- `task docs:generator:check` (part of `task docs:design`) fails when the
  fixtures drift from the generator. `task docs:generator` regenerates them.

Add a scenario for every section the generator learns to emit.

They exist because the layers above them cannot fail on the defect that matters
most here: a route that is present, tested, and answering `200` with something
no client can use. RFC 0009 §5.2 lists the ways — a resource the client cannot
select, a method the route does not accept, an auth boundary the client does not
cross, a field whose digest is the wrong algorithm — and every one of them was
found by running the client, by nothing else, twice over.

| Suite | Client | What only this layer can prove |
|-------|--------|-------------------------------|
| `marketplace.sh` | VS Code, IntelliJ | an extension that exists **only** here installs by id through the gallery |
| `bundler.sh` | Bundler 4.0.17 | the compact index's `206`/`304` are answers Bundler *accepts* — the assertion is the **absence** of a re-fetch |
| `npm.sh` | npm | publish → install → `whoami`/`ping`/`dist-tag`/`search`, and `npm audit` on the path npm really sends |
| `pypi.sh` | twine, pip | the documented `twine upload` (HTTP Basic) works, and pip's PEP 658 `.metadata` sibling answers |
| `openvsx.sh` | ovsx | `ovsx publish` with its token in a query parameter, and `ovsx get` following the rewritten download URL |
| `vsx_login.sh` | VS Code 1.136.1 (the server build's CLI, headless) | RFC 0011 §4.4: `batlehub-cli proxy serve` in front of a registry whose `anonymous` holds no verb, with `product.json` repointed at the proxy. Unauthenticated, a search through the proxy is the one `batlehub.sign-in` entry with `Code.Engine`, an install by id fails as *not found* with no request reaching the registry, and the sign-in package is refused as `NotSigned` until `extensions.verifySignature` is off, then installs; after `auth write-token-file`, without restarting anything, the same editor installs the fixture by id and every registry request on the tap carries `Authorization: Bearer` |
| `vsx_view.sh` | VS Code 1.136.1 (the server build's workbench, in Chrome over the DevTools protocol — a workspace's sidecar via `CDP_URL`, or a headless `CHROME_BIN`) | RFC 0011 §4.4.2 and RFC 0020 in a **real Extensions view**: unauthenticated, browse and search list the one sign-in entry and opening it renders the sign-in page, with nothing reaching the registry; the entry's Install button is disabled and the editor says *not signed* (pinned — the entry is a page, deliberately unsigned). After `auth write-token-file`, the same page's Refresh lists the fixture with Install **enabled** (the registry signed it); the click passes the publisher-trust dialog, fetches package and signature through the proxy, and the editor's verifier refuses (`UnhandledException`, pinned by running the editor's own `vsce-sign` on the served archive, which `batlehub-cli vsx verify` accepts); the server's CLI refuses the same way and installs once `extensions.verifySignature` is off, and on a second look so does the view. A marketplace extension republished with its own archive attached (`PUT …/vsix/signature`) gets `Success` from `vsce-sign` and installs with the verifier on. Last, RFC 0007-bis §11 q1: an extension whose manifest names an `icon.svg` carrying a `<script>`, an `onload` and a `javascript:` link is served as `image/svg+xml` under the sandbox policy with all three gone and the drawing kept, and the gallery advertises that asset on its entry — which an `application/octet-stream` icon never is, since an entry with no usable icon gets the editor's `defaultIcon`. The real Extensions view lists the extension and shows that asset as its icon, which is why the step runs before anything is installed — a view with an installed extension opens on Installed and filters a typed query against it. Whether the browser *paints* the icon is logged and not asserted: it asks, and the fetch is aborted client-side for a reason not yet established. `tests/heavy/vsx_view.mjs` is the driver |
| `console_fetch.sh` | Chrome over the DevTools protocol (a workspace's sidecar via `CDP_URL`, or a headless `CHROME_BIN`), against the **built** console | RFC 0007-bis §11 q3: the catalogue's **Fetch** button pressed by a person. The SPA and the API are one origin (`static_dir`), which is the deployed shape and keeps the run from proving anything through a CORS arrangement nobody deploys. An anonymous reader — who *can* browse this registry, so the assertion is about the offer and not about visibility — sees the upstream row and no button. A signed-in reader sees a button whose label carries the version the upstream search returned, clicked by its **accessible** name so the control pressed is the one a keyboard user reaches. Afterwards the row has left the upstream half, the version is held according to the package API, its tarball comes back through npm's own path, the tap saw the console's `POST …/fetch`, and the audit attributes it to the reader who pressed it rather than to an administrator. `tests/heavy/console_fetch.mjs` is the driver |
| `conda.sh` | micromamba | the `HEAD` probe for `repodata.json.zst` reaches a handler, and a publish is visible in the *compressed* channel |
| `nuget.sh` | dotnet | the client can *select* the search resource, `skip` advances the page, and `push` hits the path it appends a slash to |
| `composer.sh` | composer | proxy-mode resolution with Packagist disabled, `dist.shasum` the client accepts, and `search.json` reached through the advertised template |
| `terraform.sh` | terraform | `init` over TLS: host-routed discovery, download document, shasums, signature and archive, all through the proxy |
| `upstream_audit.sh` | npm, `webhook_sink.py` | a disappearance is *told*: the suite serves a directory as an npm registry, `npm install` seeds the cache, the files are removed, two `recheck` probes confirm — one miss tells nobody — and `package_disappeared_upstream` lands at a receiver the suite runs, with RFC 0014 §4.5's payload; the restore delivers `package_reappeared_upstream`; then the same directory as a `generic` registry (RFC 0014 §13.5): a held file removed upstream is missed by a `HEAD` on its path, confirmed, refused `403` on its own route while its sibling stays `200`, and served again after the restore |
| `quarantine.sh` | npm, `batlehub wait`/`why` | a `[registries.security]` registry as a client meets it: first contact **held** with the reason code in npm's own output, the embedded worker clearing it and the same cache recovering, a real OSV advisory refused on the wire and named by `why`, the packument hiding the denied version, a reload to `warn` serving it with the verdict headers, a scanner under `bwrap` that *tries* to reach the tap and cannot, and the flip — a version scanned clean by an OSV the suite runs, installed, then refused after the *scheduler's* rescan, with the admin alert at a receiver the suite runs naming who pulled it; and RFC 0002's other producer — a SOC's signed `hard_block` on a version already installed, which refuses the pinned `npm ci` as `SOC_VERDICT`, shows up in `/api/v1/admin/exposure` as a pull that *preceded* the flag, and lifts on the revoke |
| `airgap.sh` | npm, pip, mise | RFC 0008-bis, both halves on one disconnected instance: one version of an npm package, a PyPI wheel and a release asset carried by bundle into a second `[air_gap]` instance. With `synthesise_listings = false` (the phase-0 measurement) `npm install` and `pip install` of a version string stop at the packument and the simple page (`503`, retried, the held artifact never asked for) and `mise install` with no lock at the release *by tag* and then the listing, while `npm ci` from a lock and a PEP 508 direct reference fetch the artifact and nothing else. Then the miss log is purged, the key flipped by a hot reload, and the same three commands complete off listings the instance composed — `X-BatleHub-Listing: synthesised` on the packument, the JSON simple page and the release by tag — with an unheld version failing in npm as `ETARGET` and never asked for, an unheld tag refused at the release by tag and recorded as *requested v2.59.0, held v2.60.0*, which `admin air-gap-missing` prints as two columns; then `cargo`, `go`, `mvn` and `dotnet` resolving a range or an unpinned request through the composed sparse index, `@v/list`, `maven-metadata.xml` (whose `.sha1` the instance answers for itself) and flat index (Maven's plugins warmed against the connected instance first, under the same mirror id); then `bundle install` through the composed compact index and `micromamba create` through the composed `repodata.json`, whose entries the import read out of the gem and the package; then `terraform init` through a composed `versions` and a composed provider `download` document, on a TLS tap of its own (8121, `localhost` bound to the Terraform registry) — the archive, checksum list and signature carried as three plan entries, the publisher's keys read off the connected instance's document by `mise export` and carried on the manifest as `facts`, and Terraform reporting the publisher's signature verified |
| `authz.sh` | all of the above | a caller who **may not pull** is stopped, the one who may is not stopped by accident, and [RFC 0012](/rfc/0012-signed-urls-for-terraform)'s signed URLs let a *closed* Terraform registry install end to end |

Conventions worth knowing before adding one:

- **A fresh registry name per run** (`$HEAVY_RUN`). The database persists, and a
  package left by an earlier run changes what the client sees.
- **Never rewrite `Host`.** The server builds its absolute URL templates from
  it, so a rewriting proxy sees only the first request and the transcript looks
  clean because nothing was observed.
- **Give each client phase its own cache.** `npm publish` seeds the tarball into
  cacache, micromamba caches repodata, NuGet has a global package folder: reuse
  one and the test measures the client's cache, not the server.
- **A missing client is a failure, not a skip** (`heavy_need`). A heavy test that
  skips itself reports success for having done nothing.
- Assertions scope to a phase with `heavy_mark` / `heavy_wire_after`: the
  transcript accumulates, and an unscoped match can be satisfied by an earlier
  phase.

### 7-ter. The authorization suite

`tests/heavy/authz.sh` is a heavy suite in the same harness, aimed at RFC 0015's
grants rather than at one ecosystem's protocol. It takes a target:

```bash
tests/heavy/authz.sh matrix        # every verb, both directions, over curl
tests/heavy/authz.sh signing       # RFC 0012 capabilities: binding, expiry, rotation
tests/heavy/authz.sh npm           # …and the boundary as npm meets it
tests/heavy/authz.sh <ecosystem>   # pypi nuget composer conda openvsx rubygems terraform
```

One target per invocation, because each starts its own server and tap and the
Terraform one has to terminate TLS. `task test:authz-heavy` runs them in
sequence; CI runs them as the `heavy-authz` matrix.

Four rules it is built on, each of which has a scar behind it:

- **Every assertion is a pair.** The denial *and* the identical request working
  for somebody. RFC 0015 §13.16 records all three new ecosystem verbs shipping
  unreachable — `403` to the administrator — with passing tests, because each
  test granted its own verb and nothing asserted anyone could use it. A negative
  test cannot distinguish a correct denial from a denial for the wrong reason.
- **The negative arm is exactly `403`; the positive arm is "not a refusal".** A
  `404` where a `403` was meant hides a routing failure, and a holder can
  legitimately meet a `404`, `400` or `409` past the gate — asserting `200`
  would make every positive control a hostage to its fixture.
- **The vocabulary is read out of the enum at run time**, and the run fails on a
  verb it never exercised. Same shape as `vocabulary_dead_ends.rs`, asked of the
  wire.
- **`explain` is asked about the request the wire just answered.** §11.6: *"a
  diagnostic that can disagree with reality is worse than none, because it is
  trusted"* — and it has disagreed twice, under a shadow and at the instance
  tier.

Its config (`tests/heavy/config.authz.toml`) is deliberately inert in every
respect but grants: every registry is `local` except Terraform, no rule gate is
configured, and every namespace declares `visibility = "public"`. A refusal under
it is the grant hierarchy's, and a test whose denial has three possible causes
proves none of them.

Two things it does **not** claim: `group:` subjects, which need an identity
carrying groups and so belong to `crates/web/tests/dynamic_groups.rs`; and the
expired half of shadow mode, which config load refuses to let anyone write
(`crates/web/tests/grants_shadow.rs` holds it).

**Terraform is the one ecosystem whose whole install is asserted against a
closed registry.** It authenticates the two JSON documents of a provider install
and then fetches the archive, its `SHA256SUMS` and the `.sig` with no
`Authorization` header, so `signed_downloads = true` and `[server.signed_urls]`
are what let `authz-tf` grant anonymous nothing and still serve a complete
`terraform init` — RFC 0012's entire claim, and covered by no heavy suite before
this one. The phase asserts the install completes, that the artifact requests
carried `bh_sig`, that the **same URL with the signature stripped is `403`** (or
the install was served by an open registry and every signed-URL assertion is
vacuous), and that the minted document is `no-store` — it is a bearer
capability, and a shared cache would replay one caller's signature to the next.

**The `signing` target covers the three properties a single `terraform init`
cannot show**, each needing a clock or a key change:

- **Binding.** A capability minted for `shasums` presented on the `shasums.sig`
  route must be `403`. The payload names `art`; this is where that name earns
  its place, and without it one minted URL opens every artifact of the version.
- **Expiry — both edges.** A token is *not* dead at `exp`: `verify_at` refuses at
  `exp + CLOCK_SKEW_SECS`, a deliberate backward-clock allowance so a runner a
  minute behind the minter does not fail every install. The phase asserts it is
  still good just past `exp` and refused past the allowance, which is what makes
  the second assertion mean "expiry is enforced" rather than "something refused
  eventually". The skew is read out of `signed_url.rs` at run time rather than
  copied, so it cannot drift into a sleep.
- **Rotation, in both directions.** Mint under secret A; rotate to B with
  `previous_secrets = [A]` and the old URL must still work — an operator who
  rotates and breaks every in-flight install has caused a worse outage than the
  one they were avoiding — and the new mint must differ, or B is not signing.
  Then retire A and the old URL must be refused, or retiring a key does not
  retire the capabilities it signed.

It drives the key changes through `POST /config/from-content` + `pending/apply`
against a **scratch** config, never the file the server watches: that watcher
polls every two seconds and only *stages* a pending, so editing the watched file
races `load_pending_from_content`'s byte-identical dedup — whichever load wins
marks the content seen, the other returns `pending_created: false`, and the apply
answers "no pending reload". One write also fires the watcher up to five times,
which is its rate limit, so it disables itself mid-run. The phase asserts
`pending_created` rather than trusting the `2xx`, because the dedup path is a
success that changed nothing.

**Two clients cannot carry an identity on a read, and the phases say so rather
than pretending otherwise.** `ovsx get` sends no credential at all — not a
header, not the `?token=` it uses on publish. And `dotnet restore` attaches
`packageSourceCredentials` to `HttpClientHandler.Credentials`, which .NET offers
only in answer to a `401` carrying `WWW-Authenticate`; this server refuses an
unauthenticated read with a bare `403`, so the credential is never sent and a
caller holding every read verb is refused exactly like one holding none. For
both, the phase drives the client through a **publish** into a sealed namespace
— which does carry a credential, and which a seal refuses to everybody including
the administrator — and asserts the read boundary over `curl` beside it. The
NuGet phase pins the missing challenge, so if the `403` ever grows a
`WWW-Authenticate` the run says which arms to restore.

---

## 8. CLI integration tests

`cli/tests/integration.rs` — a single file with **~83 test functions**. It:

1. Starts a genuine actix-web `HttpServer` on in-memory backends
   (`TestServer::start`) and waits for the port.
2. Invokes the CLI **as a subprocess** via
   `Command::new(env!("CARGO_BIN_EXE_batlehub-cli"))`, so cargo builds the
   binary automatically before the test run.
3. Seeds admin-visible packages directly with `TestServer::seed_package()`.

**In-memory store separation caveat.** `InMemoryLocalRegistry` (used by
`LocalRegistryService` for publish/yank/delete) and `InMemoryPackageRepository`
(used by `AdminService` for package list/block) are **separate** stores in
tests, though they share tables in Postgres. Consequence: a package published
via the local-registry HTTP endpoint does **not** appear in
`GET /api/v1/packages`. Tests use `seed_package()` for `package list`, and query
registry-specific endpoints (e.g. the NuGet flat-index at
`/proxy/{reg}/nuget/v3/flat/{id}/index.json`) to verify yank/unyank/delete state.

Flows exercised include: `registry list/info/suggest` (from `mise.lock`,
`Cargo.toml`, …), `auth whoami/token/login (kubernetes,oidc)/refresh`,
`package list/versions`, `version yank/unyank/delete`, CLI publish for
nuget/npm/pypi/cargo (with a publish→proxy round-trip), `admin` (quota, ip-block,
banner, cache clear/warm, config reload/changes, audit-log list/purge/export,
stats, health, visibility, users, namespace, sbom, bulk ops, access-check,
notifications), `config show/set`, `completion`, `setup detect` (per-manifest
detection with depth / monorepo / hidden-dir handling), `setup ide`, and
`registry-types`.

---

## 9. Example-project tests

`crates/examples/tests/*.rs` — in-memory backends, no external DB, run in CI via
`batlehub-examples --test '*'`:

- `local_registry.rs` — local-mode upload/pull cycle through a real BatleHub app.
- `real_proxy.rs` — the real proxy against public upstreams; each test **skips
  gracefully** (early `return;`) when the toolchain or network is unavailable.
- `smoke.rs` — end-to-end smoke tests for every example project, with a recording
  HTTP server standing in for the proxy.
- `structure.rs` — project-structure assertions.

---

## 10. Fuzz targets

`fuzz/fuzz_targets/` (libfuzzer, `task fuzz`) — all fuzz `batlehub-core` domain
logic:

- `fuzz_rbac_evaluate.rs` — RBAC rule evaluation.
- `fuzz_deny_latest.rs` — `DenyLatestRule`; asserts only the exact string
  `"latest"` denies (unicode homoglyphs / whitespace must neither bypass nor
  over-block).
- `fuzz_release_age.rs` — `ReleaseAgeGateRule` (durations capped at one year).
- `fuzz_package_id_cache_key.rs` — `PackageId` cache-key generation is
  deterministic and always contains the registry component.
- `fuzz_readme_render.rs` — the README pipeline: for arbitrary input, in any
  format and under either image policy, the output carries no `<script`, no
  `on*=` handler and no scheme outside the allow-list, and rendering is stable.
- `fuzz_svg_sanitize.rs` — the SVG allow-list: output is well-formed XML and
  reaches outside its own document nowhere.
- `fuzz_grant_resolution.rs` — grant resolution over arbitrary hierarchies:
  the tier order holds, a seal stops inheritance, and widening never narrows.

**The targets are a separate workspace, and that is a trap.** `cargo check
--workspace`, `cargo clippy --workspace` and `cargo test --workspace` do not see
`fuzz/`, so a target can stop compiling against a type it uses and nothing says
so — the module docs go on naming a guard that is no longer running. Four of the
seven had drifted that way before `task fuzz:check` existed. That check is a plain
`cargo check` over `fuzz/Cargo.toml`, needs no nightly, and runs on every PR in
the `Fuzz targets` job of `test.yaml`; the same job fuzzes each target for 60
seconds on the nightly schedule and uploads any crash artefact.

Running one locally needs the nightly toolchain and `cargo-fuzz`. If a run
produces a **0-byte** `crash-da39a3ee…` artefact, that is LeakSanitizer failing
to start under a restricted `ptrace_scope`, not a finding — re-run with
`ASAN_OPTIONS=detect_leaks=0`.

---

## 11. Coverage

- **Gate: 80% lines.** `task coverage-check` runs
  `cargo llvm-cov report --fail-under-lines 80`.
- `task coverage` aggregates the workspace tests **plus** each adapter
  integration test explicitly (pg_cache, pg_vulnerability, local_registry,
  artifact_meta, pg_rate_limit, storage_router, s3_storage [+`storage-s3`],
  actions_oidc, and the Redis suite [+`cache-redis`]).
- Excluded paths live in the `COVERAGE_EXCLUDE` variable in `.tasks/coverage.yaml`
  (one variable, read by both `coverage` and `coverage-check`, which share their
  whole collection step and so can no longer drift). They are code that is
  either environment-glue or exercised only against real infra: `server/src`
  wiring (`main`, `server_factory`, `setup`, `stores`, `watcher`), the OIDC auth
  handlers, `crates/adapters/src/db/`, the Postgres/Redis adapter
  implementations, `cli/src/tui/` (terminal UI), and a few others. When you add
  code that genuinely can't be unit-tested, add it to `COVERAGE_EXCLUDE` with the
  same reasoning — don't lower the gate.

---

## 12. CI wiring

`.github/workflows/`:

- **`test.yaml`** — the main Rust matrix:
  - `lint`: `cargo clippy --workspace -- -D warnings` + `cargo fmt --all --check`.
  - `unit`: `cargo llvm-cov --workspace --lib --bins` + `cargo test --workspace --doc`.
  - `integration`: web (`-p batlehub-web --test '*'`), CLI
    (`-p batlehub-cli --test integration`), examples
    (`-p batlehub-examples --test '*'`), adapters default + Postgres, S3
    (`--features storage-s3 --test s3_storage`), Redis (`--features cache-redis`).
  - `heavy-marketplace`: `bash tests/heavy/marketplace.sh` (headless VS Code +
    IntelliJ).
  - `heavy-bundler`: `bash tests/heavy/bundler.sh` (a real `bundle install`
    against a local rubygems registry).
  - `heavy-client` (matrix): one job per ecosystem — `npm`, `pypi`, `openvsx`,
    `conda`, `nuget`, `composer`, `terraform`, `nvm`, `sdkman`, `mise`,
    `cargo`, `go`, `maven`, `pathproxy`, `quarantine`, `upstream_audit`,
    `airgap` — each
    running `tests/heavy/<suite>.sh`. A matrix rather than seventeen jobs because only
    the toolchain setup differs; `fail-fast: false`, because one unhappy
    client says nothing about the others.
  - `heavy-authz` (matrix): one job per target of `tests/heavy/authz.sh` —
    `matrix`, then `npm`, `pypi`, `openvsx`, `conda`, `nuget`, `composer`,
    `rubygems`, `terraform`. Its own job rather than more rows in `heavy-client`
    because every row runs the *same* script with an argument, and the `matrix`
    row — the one that matters most and costs least — needs no client toolchain
    at all, so it must not queue behind seven installs.

  Every heavy job runs the server under `cargo llvm-cov`, so what the *client*
  exercised counts toward the merged coverage table — the compact-index paths,
  the Terraform provider chain and the conda `HEAD` probe are reached by no
  other job.

  Two heavy suites are **not** in any of those jobs and are run by hand:
  `vsx_view.sh` and `console_fetch.sh`. Both drive a real browser over the
  DevTools protocol — one against the VS Code web build's workbench, the other
  against the built console — and both want a Chrome and a warm download cache
  that the matrix rows do not carry. They are the same exception
  `heavy-marketplace` is, for the same reason.

  `test.yaml` also runs nightly (`50 23 * * *`). The heavy jobs are why: they
  drive clients fetched from outside this repository — Bundler and npm from
  their own registries, pinned VS Code, IntelliJ, Terraform, .NET and
  micromamba builds — so a new client release can break a tree that no commit
  touched, and only a scheduled run finds it.
- **`front-test.yaml`** — frontend (`ui/`): install, regenerate the OpenAPI spec
  + TS client, `pnpm run coverage`.
- **`repo-interop.yaml`** — `bash tests/interop/verify.sh` (apt + dnf + pacman
  accept signed repos).
- **`sonar.yaml`** — SonarCloud: rebuilds full Rust + frontend coverage and
  uploads `lcov.info`.

The `.github` workflows start their own Postgres/MinIO/Redis service containers
in YAML; the `task test:*` targets are the local-dev equivalents (Podman).
`.forgejo/workflows/` handles image builds, Helm, the website, and updatecli —
the Rust test matrix runs on the GitHub side.
