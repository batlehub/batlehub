# Changelog

All notable changes to BatleHub will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

Nothing yet.

---

## [1.3.0] - 2026-09-19

Four registry kinds, and the machinery that found out they worked. Every kind
this release adds was driven by its real package manager before it shipped, and
the suite built to do that — one closed-world phase per kind, with the client
able to reach nothing but this instance — then turned on the kinds that had
shipped green months ago. Three of them were unusable. Those, and the rest of
what a real client found, are in **Fixed** below.

### Added

- **Alpine `apk` (RFC 0026).** `type = "apk"` completes the OS family beside
  `deb`, `rpm` and `pacman`: in proxy mode a path tree per
  `{branch}/{repo}/{arch}` with `APKINDEX.tar.gz` and the `.apk` files under it;
  in `local`/`hybrid` mode a repository this instance hosts, regenerating and
  **signing** its index on every upload the way `pacman` regenerates
  `<repo>.db`.

  Two things decide the shape, and both were read out of apk-tools rather than
  remembered. The index cannot be a filtered listing: every shipping apk — 2.14
  on Alpine 3.21/3.22, 3.0.8 on 3.23, 3.24, `latest-stable` and `edge` —
  verifies Alpine's RSA signature before reading a byte of it and refuses an
  unverifiable index unless `--allow-untrusted` is on, which also switches off
  the package identity check. So the upstream index is relayed byte-exact and
  the block is enforced at the `.apk`, whose file name carries a real name and
  version. That makes `apk` the first member of the path-proxy family with a
  coordinate — a block list, an age gate, a row in explore — and the honest
  limit is that apk's solver still *selects* the blocked version and fails on
  the download, in its own words rather than as a not-found.

  Publishing signs with **RSA and without the banned crate**: apk 3.0.8 accepts
  exactly the `.SIGN.RSA*` entries 2.14 does and OpenSSL refuses an Ed25519 key
  for them, so "wait for an apk that takes Ed25519" was waiting for something
  that did not happen. The ban (RUSTSEC-2023-0071) is on the pure-Rust `rsa`
  crate's timing side channel, not on the algorithm, so signing goes through
  `aws-lc-rs` — already in the graph as this tree's TLS provider — and
  `cargo deny check` staying green is the regression test that nothing smuggled
  `rsa` back in. `[registries.apk_signing]` takes the key name and PEM, with
  `previous_keys` for rotation and `apk_unsigned` for a repository that is not
  signed at all.

- **Ansible Galaxy (RFC 0031).** `type = "galaxy"` serves the collections API
  v3: `/api/` is the discovery document, `v3/collections/{ns}/{name}/versions/`
  is the listing and the enforcement chokepoint, the version document carries
  `download_url` and `artifact.sha256`, and the `{ns}-{name}-{v}.tar.gz` tarball
  is the artifact. `local` and `hybrid` mode accept `ansible-galaxy collection
  publish` and answer the import-task poll the client makes afterwards. Roles
  (the v1 API) are served read-only. The client switch is `server_list` in
  `ansible.cfg`; `roles` selects the role mode.

  Three facts about `ansible-core` decide the design and all three came from its
  source. Every pagination link the client follows loses a path prefix —
  `get_collection_versions` does `urljoin(api_server, next_link)`, so an
  absolute-path `next` sends the following request to the root of the host
  rather than to `/proxy/{registry}/galaxy/…` — so every listing this instance
  serves is one page with its `next` null and the adapter walks upstream's pages
  itself. The client hashes the body against `artifact.sha256` from the version
  document. And the credential arrives under the `Token` scheme, which
  `raw_auth_from_request` now normalises.

- **Nix binary cache (RFC 0028).** `type = "nix"` serves the substituter
  protocol: `nix-cache-info`, one `{hash}.narinfo` per store path, and the NARs
  under `nar/`. In proxy mode it fronts `cache.nixos.org` (or any other cache),
  relaying every narinfo's `Sig:` lines byte-exact so the client's own
  `trusted-public-keys` keep doing the verifying, and rewriting the one field
  the signature does not cover — `URL:` — so the NAR is fetched under a
  coordinate this instance can block, cache and count. In `local`/`hybrid` mode
  it accepts `nix copy --to`, verifies what was uploaded and signs each narinfo
  with the registry's own Ed25519 key in the `name:base64` form `nix.conf`
  already expects. No OpenPGP, no `rsa`, and nothing upstream signed is
  re-signed.

  The package model is Nix's own: `builtins.parseDrvName` splits a store path's
  name at the first dash not followed by a letter, so `hello-1.0.0.2-doc` is
  package `hello` at version `1.0.0.2-doc`. A block makes every narinfo whose
  path parses to it answer `404`, which is the protocol's own "this cache does
  not have it" — Nix moves to the next substituter or builds from source, and
  says so itself.

- **Rust toolchains (`rustup`, RFC 0024).** A BatleHub instance proxied every
  crate a Rust build resolved and not the compiler it resolved them with: the
  dist tree is a file tree with no package protocol, so the only way through was
  a `generic` registry, which caches bytes and enforces nothing. `type =
  "rustup"` serves it as one package, `rust`, whose versions are rustup's own
  toolchain names — `1.98.1`, `beta-2026-09-11`, `nightly-2026-09-05` — plus a
  `rustup` package for the installer's self-update tree.

  The channel manifests are the filtered listing and the chokepoint, because
  rustup resolves **every** install through one: a blocked toolchain fails on
  rustup's own *"could not download nonexistent rust version"* with nothing
  downloaded. rustup verifies each manifest against its `.sha256` sidecar, so
  the sidecar this instance serves is always the hash of the bytes it serves —
  a filtered manifest with the upstream's sidecar would break every install.
  `deny_components` refuses a component (`rust-analyzer`, `miri`) across every
  toolchain.

- **Forgejo's attachment endpoint (RFC 0019 §4.2).** Forgejo gives every release
  asset a uuid and serves it from `{forge}/attachments/{uuid}`, a repository-less
  path beside the API. `mise`'s `forgejo:` backend builds that URL itself rather
  than following the `browser_download_url` in the release document — and *only*
  that — so rewriting the document's URLs, which is what routes every other
  forge client here, left that one going straight to the forge: a failure in a
  closed world, and a silent bypass of the policy, the cache and the audit trail
  in an open one. BatleHub answers the same shape. The uuid names no repository,
  so the coordinate is not read out of the request but remembered from the
  release document this proxy rewrote; a uuid nothing has been remembered for is
  a `404`, because the route resolves what this instance has served rather than
  relaying the forge's whole attachment space.

- **`batlehub registry suggest` derives the registries a project needs from what
  it actually downloads.** `mise.lock` is the precise input — it records the
  exact URL of every tool the project installs, per platform, and each URL maps
  either onto a typed registry (a `github.com` release asset → `type =
  "github"`) or, where the host speaks no package protocol at all, onto a
  `generic` mirror of it. `mise.toml` and the usual manifests are the
  best-effort fallback, mapped by backend prefix and tool name. The output is a
  set of `[[registries]]` blocks plus the client-side environment variables that
  point each toolchain at them, deduplicated by registry name.

- **The allocator's own accounting, as gauges.** RSS says how much memory this
  process holds and cannot say who holds it — and those differ exactly when the
  allocator keeps pages the program already freed, which is jemalloc by design
  and is what a leak looks like from outside. Five jemalloc series are exported
  now: `allocated` (the leak signal), `active`, `resident`, `mapped` and
  `retained`. Settling one 16.6% idle-RSS report against a server whose live
  heap never moved cost four ten-minute runs and two seventeen-minute builds
  because the process could not be asked. It can now.

- **A leak suite, a saturation suite and a per-release performance record.**
  `soak.yaml` runs a fixed arrival rate across the paths that allocate
  differently and, beside it, npm in a loop against a served upstream; both
  compare two *idle* windows, so what they report is what was not given back.
  `breaking-point.yaml` escalates the offered rate per backend until something
  gives and reports what every rate below the knee cost. Neither is scheduled —
  each is minutes to an hour of load answering a question no single commit
  changes the answer to — and both are run before a release. `perf-report.yaml`
  runs on the release tag, attaches the report and diffs it against the previous
  release's copy: a record rather than a gate, because a shared runner is too
  noisy for a threshold that would hold, but peak RSS doubling or a scenario
  that started erroring shows up in the shape.

- **One closed-world phase per registry kind, and two gates that keep it that
  way.** `tests/heavy/closed_world.sh` gives the package manager no egress at
  all while BatleHub keeps its own, so anything a phase obtains it obtained
  here, and asserts on the wire transcript rather than on the client's exit
  code. `registry_kind_coverage.rs` refuses a kind added to `RegistryKind::ALL`
  without a phase that drives it and an air-gap claim, checking both against the
  files rather than trusting the declaration; `soak_kind_coverage.rs` does the
  same for the soak arms, because a soak that drives npm alone would be just as
  green against a kind that leaked a megabyte per listing. Three more suites
  joined them: `hybrid.sh` (local shadows upstream, upstream fills the rest,
  with "never asked" as a count of zero), `backends.sh` (S3, Redis and a real
  OIDC issuer under real clients — the shape every deployment has and no other
  heavy suite ran), and `external_tests.sh`, which runs the Postgres/S3/Redis
  integration tests for real on a machine with no Podman, where they had been
  skipping silently and reporting green.

- **Two scanners for what the other one cannot see.** `vuln-scan.yaml` sends all
  four dependency roots to vuln.mlab.sh on every pull request, with `ui/` and
  `docs/` going through a CycloneDX SBOM because the endpoint has no pnpm
  parser. `mise-scan.yaml` scans the *installed* development toolchain, because
  the lockfile scan reads four of the forty-odd tools in `mise.lock` — the rest
  arrive as GitHub release assets and come back with no findings, which is
  indistinguishable from clean. A `trivy rootfs` over the same toolchain found
  854 findings, 461 of them fixable HIGH or CRITICAL, the day it was reported
  clean. Both are advisory and say why in their own headers. `vex/batlehub.openvex.json`
  is the OpenVEX document for findings that are not exploitable here, validated
  by `task vex`, which also checks that it and `.trivyignore.yaml` still agree.

- **`pr-checklist.yaml` posts what a diff implies.** A new `RegistryKind`
  variant owes nine things in eight files; a file under `migrations/` owes a
  `mig!` entry; a `values.yaml` edit owes a regenerated chart README. Advisory
  by design — a required check that turns green when somebody ticks a box
  measures the ticking — and one comment per pull request, edited in place.

- **Thirteen more fuzz targets**, taking the set from 7 to 20: bundle reads,
  escaping, image hosts, integrity parsing, listing filters, URL normalisation,
  path safety, release coordinates, SBOM and scanner extraction, signed URLs and
  version ordering.

- **`cargo_auth_required`.** Whether a cargo registry advertises itself as
  closed is derived from whether an anonymous caller can read it, and the
  derivation reads the *registry* tier — so a registry that closes the tier and
  re-opens one package to `*` through a grant was advertised as fully closed and
  cargo demanded a token for the open package too. The knob is the override for
  that case.

- **The config-change history is reachable by keyboard.** The row disclosure on
  the reload page was a `@click` on a `<tr>` with `cursor-pointer`, no
  `tabindex`, no `role` and no key handler: unreachable without a mouse and
  announcing nothing. It is a button in the first cell now, carrying the focus
  ring, Enter and Space, and `aria-expanded` — the chevron alone said the state
  only to a reader who could see it.

### Changed

- **Grants are evaluated before anything is fetched.** `authorize_grants_public`
  reads the request coordinate, the identity and the action, and nothing else —
  the answer is the same whether or not any package or version row exists — but
  it ran *after* the metadata resolve, so every refused request first fetched
  the artifact's metadata from upstream and filled the cache with it. An
  unauthenticated caller naming coordinates nobody had asked for could spend the
  upstream's rate limit and this instance's bandwidth one refusal at a time. The
  check now runs first in both funnels. Nothing about *what* is evaluated
  changes: the coordinate is not touched in between. The rule chain stays below
  the resolve, because it genuinely needs what the resolve produces — the
  release-age gate reads `published_at` off the metadata, and a `latest` request
  is only known to be blocked once it has resolved to a version.

- **conda's index is streamed, and cached as bytes.** `conda-forge/linux-64`'s
  `repodata.json` is 424 MiB of about 1.4 million small objects, and a
  `serde_json::Value` of that measured ~11.5 GB resident for a single request —
  past what a client will wait for and past what a 16 GB runner has. The block
  filter now reads the document and writes the filtered one at the same time,
  holding one package entry at a time, emitting the input's own keys, order and
  values so a solver still agrees with the artifacts behind it. The compressed
  index is cached as bytes in the storage backend rather than as base64 inside a
  metadata-cache entry: `repodata.json.zst` is 57 MiB, which held the old way
  was a 77 MiB string, a JSON document around it, a serialisation on every write
  and a decode on every read — paid several times over, because micromamba asks
  per subdir and per encoding. The cache entry is a pointer now and a hit
  streams out of storage.

- **A document parsed at a size that costs real memory says so.** No limit
  covered this — `[limits] max_artifact_size_bytes` applies to artifacts, not to
  documents — so the first symptom of the 424 MiB case was a client timing out
  with nothing in the log to explain it. Past 64 MiB, comfortably above every
  other listing this proxy serves and below the one that hurts, the parse is
  logged with the document named.

- **`paste` is substituted rather than suppressed.** The crates.io `paste` is
  archived upstream (RUSTSEC-2024-0436, "No safe upgrade is available!") and its
  only holder here is `tikv-jemalloc-ctl 0.7.0`, the latest release, which still
  requires `paste = "1"` — so there is nothing to upgrade to. `patches/paste` is
  a plain lib re-exporting `pastey`, the fork RustSec names as the drop-in
  replacement, which keeps `deny.toml`'s `ignore = []` empty. `tikv-jemalloc-ctl`
  uses the macro only for `[<$id _mib>]` identifier concatenation — no mallctl
  key string passes through it — so the substitution cannot change which key the
  stats reader looks up, and a mismatch would be a compile-time name-resolution
  error rather than a wrong key at run time.

### Security

- **Two routes reached the proxy funnel without a grant check.** The RubyGems
  gemspec route and the `generic` path mirror have no local branch at all, so
  neither passed through a `chain::*` funnel, and `RbacRule` is no longer in the
  rule chain — the rules below it judge only the artifact. Both were found by
  the route-by-route authorization matrix rather than by a test, which is the
  transferable part: a funnel the callers do not all pass through is not a
  funnel. Grants are resolved on the proxy path itself now.

- **An anonymous caller could park bytes in a `nix` registry's staging area.**
  `nix copy --to` sends a NAR *before* the narinfo that names it, so when the
  bytes arrive nothing is known about them — not the package, not the version —
  and the coordinate-scoped check had nothing to answer about. It authorized
  nothing instead: `PUT nar/… -> 200` followed by `PUT ….narinfo -> 403`, which
  the heavy suite caught. `holds_anywhere_in_registry` is the widest question
  that is still a question — *could this subject publish here at all?* — asked
  at the instance tier, the registry node and its namespaces. The
  coordinate-scoped check still runs when the narinfo arrives, so this narrows
  who may consume storage without widening what anyone may claim.

### Fixed

Most of these were found by a real client rather than by a test, in kinds that
had shipped with passing ones.

- **npm refused three packages outright.** A packument is not a schema: every
  version entry is the `package.json` it was published with, including shapes
  npm itself stopped accepting years ago. `tmp@0.0.4` (2012) spells `repository`
  as a one-element array of the object form, and `fs-extra@0.0.1`,
  `jsonfile@0.0.1` and their siblings spell `homepage` as a one-element array of
  the string. serde reads the whole document, so one such entry failed the
  *package* — `data did not match any variant of untagged enum NpmRepository`,
  surfaced as `502 malformed npm packument` — and every version of `tmp`,
  `fs-extra` and `jsonfile` was un-installable through this proxy. Both fields
  feed links on a metadata page and nothing else, so a shape neither reader
  understands is now dropped, the same as absent. A document a client asked for
  is never failed for a field no client reads.

- **Every editor got a gzip file named `.vsix`.** The VS Code gallery's
  `vspackage` endpoint answers `Content-Encoding: gzip` *unsolicited* — this
  client asks for no encoding and is sent one anyway — and a content encoding is
  a property of the transfer, not of the artifact. Relaying it as it came served
  a gzip stream wrapping the VSIX under a `.vsix` name, which every editor
  reported as `Could not find EOCD`, a zip's directory living at the end. The
  body is decoded now.

- **A pinned extension version was a `400`.** There is no version criterion in
  `extensionquery`: the filter types are a closed set naming extensions, and a
  body carrying one the API does not know is refused with *"Value does not fall
  within the expected range"* rather than returning nothing. A pinned version is
  asked for the way the editor asks for it — the extension's whole version list,
  picked from here.

- **Open VSX's asset URLs pointed at a route no `ovsx` client asks for.** Every
  `files.*` URL in an extension document is on the API host and answers `302` to
  the `openvsx.eclipsecontent.org` CDN. The linked-README read follows redirects
  hop by hop and re-checks the origin at each one, with credentials attached only
  while the chain stays on `base_url`; the CDN exception is granted only when the
  configured base really is public Open VSX, since a self-hosted instance serves
  its own files from its own origin and has no business being pointed at
  Eclipse's CDN by a response it proxies. Both fetch paths share one helper, so
  the origin check cannot be present on one and missing on the other — which is
  the shape this bug had.

- **GitLab answered `502` to every single-release read.** The typed route hands
  the client a `PackageId` with no selector on purpose — `proxy_release_document`
  wants the upstream document so it can repoint the asset URLs inside it — and
  the client refused that shape with *"fetch_artifact requires
  PackageId::artifact to be set"*. That is the first thing `mise`'s `gitlab:`
  backend asks for. The Forgejo and GitHub clients had carried the arm all
  along.

- **Forgejo's release listing was fetched from the instance root.** Every other
  API call here goes to `api_base_url`; this one went to `base_url`, so `mise`'s
  `forgejo:` backend asked codeberg.org for `/repos/forgejo/forgejo/releases` and
  was told `404` by the web host.

- **The JetBrains Marketplace could not resolve the only coordinate an IDE ever
  learns.** An IDE addresses a plugin as `/files/{pluginId}/{updateId}/…`, both
  numeric, while the published coordinate is `xmlId@version`. The numeric
  spelling is now recognised — all-digit segments are unambiguous, since an
  xmlId is reverse-DNS-ish and a version is dotted — and resolved through the
  plugin's updates listing plus the one field that listing does not carry, the
  `xmlId` itself.

- **conda paid 57 MiB per `HEAD`.** Clients probe before they fetch —
  micromamba sends a `HEAD` for every subdir and encoding it might use — and
  those were answered by pulling the body and discarding it. The probe is now
  the same question asked upstream, read off the `Content-Length` header rather
  than a decoded body. Any failure gives up probing rather than failing the
  request: plenty of CDNs answer `405` to a `HEAD` and S3-style backends answer
  `403` for a missing key, and propagating either turned a channel that serves
  fine over `GET` into a `502` for every probe.

- **PyPI could not serve a PEP 503-only index.** A missing
  `/pypi/{name}/{version}/json` was not distinguished from an upstream that has
  no JSON API at all, and only the latter should fall back to the simple page.
  The two answers are separate now, and the simple-page path reads what such a
  page can say about a file — its URL and the sha256 in the link's fragment —
  and nothing it cannot.

- **npm fetched each packument twice.** The document the artifact route needs is
  the one `resolve_metadata` already fetched and cached, and resolving the
  coordinate again on the tarball request cost two more upstream fetches per
  first read. The cached packument is read instead — except for a synthesised
  listing, whose URLs point back at this instance and would be refused by the
  origin check for reading the wrong document rather than for anything being
  wrong.

---

## [1.2.0] - 2026-09-09

### Added

- **Fetch a package from the catalogue, not only from its page (RFC 0007-bis
  §11 q3).** A search that finds a package upstream lists it as an `upstream`
  row, and that row was a wall: the only way to pull it was to open its page
  first. The row now carries a button naming the version the upstream search
  returned — **Fetch 4.17.21** — which runs the same download the package
  page's button runs, under the caller's own identity and through every gate,
  the quota and the audit. Cached rows are offered nothing, because there is
  nothing to go and get. Off with `console_fetch = false`, the same switch that
  governs the package page, and not offered where a version is not a single
  artifact.
- **An extension's SVG icon renders (RFC 0007-bis §11 q1).** The VS Code
  marketplace asset endpoint served every SVG icon as an opaque download, so
  the editor's Extensions view and the console both showed no icon at all —
  the only safe answer while nothing in the tree could vouch for a publisher's
  markup. The README image proxy's sanitiser is now a shared service rather
  than a detail of the README service, and the icon goes out sanitised, as an
  image, under the same sandbox policy. A document the sanitiser refuses is
  still an opaque download.
- **A disconnected instance answers the listing a client resolves through
  (RFC 0008-bis).** A bundle carries artifacts and the entry that finds
  them, never the document a package manager reads first, so `npm install
  left-pad@1.3.0` and `pip install six==1.17.0` used to stop at a `503`
  on an instance that held the very thing they wanted. An `[air_gap]`
  instance now composes that document from what it holds — one version per
  held key, nothing else — and marks it `X-BatleHub-Listing: synthesised`
  with the count in `X-BatleHub-Listing-Held`. Every kind with a listing is
  rendered in its own shape, including the four whose answer lives inside
  the artifact and is read at import (RubyGems' compact index, conda's
  `repodata.json`, NuGet's registration pages, Composer's `p2`) and
  Terraform's download document, composed from the held archive, checksum
  list and signature with the publisher's own keys carried on the manifest,
  since this instance signs nothing. A version the instance does not hold
  is absent from the listing rather than served, so the client fails the way
  it fails upstream. The miss log gained the version the client asked for
  and the versions held beside it, in the API, the CLI and the console.
  `synthesise_listings = false` restores the old refusal.

- **An editor with no credential hook can sign in to a private extension
  registry (RFC 0011).** A credential contract file, keyed by origin and
  described by a normative JSON Schema, carries a token or a path to one:
  `batlehub-cli auth token`, `auth write-token-file` and `auth status`
  write and read it, and `--kubernetes-token-path` turns the pod's own
  service-account token into an entry that is re-read per request. For an
  editor whose gallery URL can be repointed, `batlehub-cli proxy serve`
  binds a loopback gallery proxy that attaches the credential, rewrites
  absolute URLs and streams; with no credential it answers a search with
  exactly one entry, the sign-in bootstrap, instead of a blank view. A
  che-code patch in `patches/che-code/` reads the same file, origin-scoped,
  and retries once on `401`. `tests/heavy/vsx_login.sh` drives the real VS
  Code core through all of it, and the editor extension that pairs with it
  lives in the `batlehub-vsx` repository.

- **A security team can push a flag, and ask who already pulled it (RFC
  0002).** A `[[flag_sources]]` entry signs a batch to `POST
  /api/v1/flags/{source}` with `X-Hub-Signature-256`, one item per
  coordinate at an exact version or `*`, and `DELETE
  /api/v1/flags/{source}/{external_id}` revokes it. The effect is capped by
  the source's `max_effect` and scoped to the registries it names. On a
  registry with `[registries.security]` the flag becomes a finding of kind
  `SocVerdict`: a `hard_block` denies the stored verdict at once and
  re-derives on rescan, a `gate` is judged against the registry's
  `max_severity`, and the only relief is a `GateExemption`, since `flags`
  joined the exemptible gates. On a registry without a profile the same
  store is read by a rule of its own beside the block list. The inbound
  `security.verdict` webhook is the degenerate case of the same push and
  lands in the same table, so its name may not collide with a flag source.
  `GET /api/v1/admin/exposure` answers the question the flag raises: one row
  per consumer, coordinate and flag, with how many pulls preceded the flag,
  a coverage block saying what the report could not see, keyset paging and a
  CSV or JSON export. `batlehub admin flags list` and `batlehub admin
  exposure` are the terminal side, and the console carries the flags and
  exposure panels. Step 8 of `tests/heavy/quarantine.sh` drives the whole
  lifecycle with npm.

- **Git-forge registries serve refs, releases and raw content (RFC 0019).**
  `github`, `gitlab` and `forgejo` registries now take a tag, a branch or a
  commit where they used to take a release tag only. The ref is resolved
  once and the coordinate becomes its commit SHA, so a cache entry is a
  commit and never a name that moves; `X-BatleHub-Ref-Kind`,
  `-Commit`, `-Requested` and `-Previous-Commit` say what the name resolved
  to and what it answered last time. `[registries.refs]` sets the branch
  TTL and what a mutable ref costs (`MUTABLE_REF` warned by default, a
  moved tag and a replaced asset denied). `[registries.raw]` serves single
  files, off by default, bounded by size, allowlisted by repository glob and
  path, and refusing a shell script (`RAW_SCRIPT`) on any registry that
  opted into a quarantine. `[registries.api_reads]` turns on three typed
  read-only families — `tags`, `commits`, `branches` — with no wildcard
  passthrough, and every download URL in a release document is repointed at
  the proxy. The commit date, the publisher and the forge's own
  attestations and signatures ride the version into RFC 0018's verdict, and
  the console's package page shows the moving refs and the short SHA.
  `tests/heavy/mise.sh` drives a real `mise` through the whole of it.

- **A forge release becomes an installable package (RFC 0021).** CI builds an
  artifact and attaches it to a release; a `github`, `gitlab` or `forgejo`
  registry made that asset downloadable, and an editor still could not see it,
  because a gallery is served by a registry of its own kind. A
  `[[release_imports]]` block now names a repository, its asset globs and a
  target registry, and the instance publishes what the release carries into it:
  the extension appears in the Extensions view, signed at publish exactly as an
  uploaded one is, scanned and audited like any other version. An import **is**
  a publish, so every gate on that path applies and there is no second door.
  `POST /api/v1/admin/registries/{registry}/import` runs one now (`cache:warm`
  to ask; the configured principal's own `releases:publish` to do it), and
  `interval_secs` runs it on a schedule — free when nothing has been released,
  since a version the registry already holds is skipped. `latest` means the
  newest release that is neither a draft nor a pre-release; a draft is never
  imported. The publisher is a principal declared in the config, never an
  admin: an admin would skip the namespace-membership check and could publish
  into any namespace on the target, so the config refuses one at load. Beyond
  galleries, the coordinate comes from the asset's file name by the convention
  each ecosystem's own tooling produces — the rules `batlehub publish` has
  always used, now shared with the server rather than copied.

- **Signed VSIX assets for `openvsx` / `vscode-marketplace` registries (RFC 0020).**
  A current VS Code's Extensions view greys out Install on any gallery entry
  without a signature asset. A registry that holds an Ed25519 key
  (`[registries.vsx_signing] seed_hex`, `key_id`) now signs every VSIX it
  publishes and serves the signature in Open VSX's archive shape as
  `Microsoft.VisualStudio.Services.VsixSignature`, with the key as
  `…PublicKey` and at `GET …/api/-/public-key/{key_id}`; the Open VSX
  document carries `files.signature` and `files.publicKey`. Versions
  published before the key existed are signed on first request; a rotated
  key re-signs the same way. An upstream's own archive can be attached to a
  republished version instead (`PUT …/{ext}/{version}/vsix/signature`) and
  is served as-is — the marketplace's signature is the one a stock editor
  verifies. `batlehub-cli vsx keygen` prints a seed; `batlehub-cli vsx
  verify` checks a download against the served archive and key.
  `tests/heavy/vsx_view.sh` drives a real Extensions view through all of it.

- **Authorization became one vocabulary on a hierarchy (RFC 0015).** An operator
  who wrote `user = ["releases:read", "source:read"]` had configured half of one
  third of it: those two strings were the entire permission vocabulary, there was
  no write verb at all, and a grant could only attach to a whole registry, so
  "the payments team owns `@acme/billing-*`" was not expressible.
  `[registries.grants]` now attaches a grant at any level of registry →
  namespace → package → version and inherits it downward, the vocabulary
  includes the writes, and a namespace carries its own default visibility,
  immutability policy and gate overrides for everything published beneath it.
  The nine mechanisms that used to answer "who may do what" one at a time —
  ownership, visibility, versioning policy, quota, beta channels, console browse,
  `bypass_roles`, signed URLs, `firewall_only` — are that one model now.
  `batlehub authz explain` resolves what a subject may do and names the tier that
  granted each verb; `batlehub authz shadow` reports what the new decision would
  have refused while it is not yet enforcing.

  It absorbs **RFC 0011-bis**, which is why a team's packages are now visible to
  that team and to the groups it grants read to: the namespace claim knows each
  ecosystem's separator, so `digital` covers `digital.pipeline-tools`, and a PAT
  carries its creator's groups (`--groups` / `--all-groups`, and the group picker
  on the Tokens page), so automation sees what its owner sees rather than
  nothing. A PAT's expiry became mandatory with it — 1 to 90 days.

- **Grants at the package and version tiers have an editor (RFC 0017).** RFC 0015
  built the two deepest tiers and left nothing that could write to them: the only
  caller of `put_grant` was the ownership projection, and no code in the tree had
  ever written a version row. `grants:read` and `grants:write` join the
  vocabulary, `batlehub admin grants list|set|rm` and a console panel write
  either tier, and the per-version listing filter — written for RFC 0015 §4.4 and
  never called, because with no version row to differ from the package answer
  there was nothing to filter — ships in the same release as the writer that
  makes it load-bearing.

- **Retention, and a published name that can never mean two things (RFC 0016).**
  Nothing published locally was ever reclaimed, and nothing published was ever
  safe from being republished as different bytes. Delete is now a soft delete,
  and both follow from it. A retention policy attaches to the tier system —
  `keep_versions`, `keep_for`, and `keep_if_pulled` as a veto, so whatever anyone
  is actually using stays — and defaults to `dry_run = true`, which reports and
  reclaims nothing. A **tombstone** keeps the coordinate forever: a deleted
  `@acme/widgets@1.4.0` is permanently spent, so it cannot become different bytes
  for the next lockfile that asks. Retention's namespace and package tiers are
  described but not built; `NamespaceConfig` refuses a `retention` key outright
  rather than ignoring one.

- **An artifact is scanned before it is served (RFC 0018).** The proxy used to
  decide with a chain of independent gates, each looking at one signal and
  answering allow or deny into a free-form string most clients never display.
  There was no notion of an artifact *not yet known to be safe*. A per-registry
  `[registries.security]` profile now turns the proxy into a quarantine: what
  comes from upstream is scanned by a configurable set of scanners, the findings
  are evaluated into a persisted verdict — `allowed`, `warned`, `quarantined`,
  `denied` — and only the first two are served. Versions older than a
  configurable maturity age are served while their scan is pending, so the layer
  can be switched on over a live cache. The verdict carries machine-readable
  reason codes and reaches a reader through each registry's own protocol error,
  `batlehub why`, the Package Explorer and the notification channels, behind two
  new permissions: `quarantine:read` (that a version is held, and until when) and
  `findings:read` (why, in detail). Overriding one is an administrator action,
  and a signed inbound webhook lets a SOC push a rescan or a verdict of its own.

  Scanning runs in a **worker** role of the same binary. `[server] roles`
  defaults to `["proxy", "worker"]`, so a single process already runs one
  embedded; `--roles worker` and `--roles proxy` split them across deployments
  sharing nothing but the `scan_jobs` queue in Postgres. The proxy never blocks a
  request on the worker, so a dead or saturated one degrades the registries it
  covers rather than failing them. The scanner toolchains — bubblewrap,
  `postmortem`, the Trivy client, optionally GuardDog — live only in the worker
  image, which is why there are now two. See
  [The scan worker](docs/operations/scan-worker.md).

- **A block is now visible to every ecosystem, not only to npm (RFC 0006).**
  Blocking has two halves: the download gate, which has answered `403` with the
  operator's reason for every registry since the block list existed, and the
  listing half — leaving the version out of what a client is told exists, so a
  resolver never picks it. The second was built for npm and only for npm, and on
  the other twenty kinds the client read the upstream listing, resolved `latest`
  to the blocked version and the install *failed*. The block read as breakage
  rather than as policy. Every kind that has a listing document filters it now,
  the ones that cannot are stated rather than left to be discovered, and the
  coverage is compiler-enforced rather than a paragraph in the admin guide.

- **Every endpoint the client actually calls (RFC 0009).** `npm audit` was served
  at `/-/npm/v1/audit/bulk`; npm calls `/-/npm/v1/security/advisories/bulk`. Four
  tests asserted the first path, which is our route and has never been npm's.
  A survey of the other nineteen kinds found four more of the same class and a
  long tail — `openvsx` and `vscode-marketplace` cached VSIX bytes and served
  none of the gallery routes an editor calls, so BatleHub could not be an
  editor's marketplace at all. All of it is served, and two mechanisms keep the
  next invented endpoint from passing: a protocol conformance fixture per
  ecosystem, asserting the client's literal paths route, and generated endpoint
  tables in the registry pages with a drift check. Each ecosystem is now verified
  against its real client by a script in CI rather than by a transcript, which
  found six further shipped bugs.

- **Each version's own README, stored, rendered and shown (RFC 0007).** BatleHub
  received that text on four code paths and threw all four away, then rendered a
  package page that could say what a version costs, what it depends on and
  whether it is vulnerable, but not what it *is*. The README is stored per
  `(registry, name, version)` from whichever source the kind actually has,
  rendered to sanitised HTML on the server — allow-listed and fuzzed, because the
  console serves the console's own origin — and shown under a version selector,
  with the CLI able to print the source. The page also gained a **discovery
  read**: one bounded, cached upstream lookup on the console path, so a package
  this instance holds no bytes of has a version list and a README instead of
  "no versions yet", which is the state the console's own search leads to.

- **What the console owes a reader looking at one package (RFC 0013).** Eleven
  things the catalog and the package page knew and could not act on, or acted on
  and could not say. The search state survives a click rather than dying with the
  component. The version list keeps what you narrowed it to and which page you
  were on. The selected version is marked in something other than a 1.06:1 fill.
  A README rendered from markdown can be read as markdown, and fenced code keeps
  the language the sanitiser used to erase on the way out. The hosts an image may
  come from are stated. Two lists page on the operator's numbers rather than on a
  literal, and the **Fetch this version** button is no longer offered to a reader
  the endpoint would refuse — which it now does, having previously refused
  nothing at all.

- **The toolchain layer: the JDK and the Node runtime themselves (RFC 0010).**
  An instance could proxy every Maven artifact a JVM build resolves and none of
  the JVM, every npm package a Node build installs and not the Node. Two
  proxy-only kinds close that: `sdkman`, a protocol this server did not speak,
  and `nodedist`, a tree it already mirrored as `generic` and could enforce
  nothing on, because a path-addressed registry has no version to block. SDKMAN's
  `versions/all`, `candidates/default` and the rendered `sdk list` table are all
  filtered, and a blocked version is refused at `candidates/validate` so the
  client prints its own *"is not a valid … version"* rather than failing
  mid-download. Its broker answers `302` to third-party CDNs, so the redirect
  chain is followed server-side through the SSRF guard — otherwise the 200 MB JDK
  leaves the site anyway and the proxy has mediated the policy and none of the
  bytes. For `nodedist`, `index.tab` is the enforcement chokepoint every `nvm`
  install resolves through, and `SHASUMS256.txt` is passed through byte-exact
  because nvm verifies against it. `tests/heavy/nvm.sh` and
  `tests/heavy/sdkman.sh` drive the real clients.

- **`mise install` on a host with no route off the site (RFC 0008).** Pointing
  mise at BatleHub was already documented, and none of it was an air gap: it
  assumed the workstation could still reach whatever the rewrite table failed to
  mention. `mise.lock` — which already records the exact URL and checksum of
  every tool, per platform — becomes a plan the instance can be seeded from and
  audited against: `batlehub mise plan` writes it, `mise seed` fetches every
  entry through a connected instance and proves it matches the lock (non-zero on
  a miss or a disagreeing digest, so it is usable as a CI gate), `mise export`
  builds a signed, content-addressed bundle, and `mise import` verifies the
  signature before reading a single blob. `[air_gap] enabled = true` makes a
  proxy-mode registry never dial upstream: a miss fails fast, names itself and is
  recorded, so the list of what the next bundle needs is produced by the estate
  rather than guessed. `bundle_trusted_keys` is required with the mode, since an
  air-gapped instance whose only content path is unauthenticated is worse than
  one with no content path. Mise's own supply-chain verification — cosign, SLSA,
  GitHub attestations, all on by default and all unreachable offline — moves to
  the connected side, performed once at seed time and served as a verdict.

- **A package that vanishes upstream is noticed, held and reported (RFC 0014).**
  A proxy cache exists so an estate survives its upstreams, and this one survived
  an upstream that was *down* while missing one that had *changed its mind*: when
  a version was unpublished, BatleHub kept serving the cached artifact, said
  nothing, and then deleted it at the next TTL sweep, because eviction had no
  idea it was holding the last copy in the estate. A periodic audit now asks each
  proxy/hybrid upstream whether what we cached from it is still there, confirms a
  disappearance across several sweeps before believing it, notifies through the
  channels the admin already configured, and exempts a confirmed disappearance
  from TTL and idle eviction. What happens next is policy: `on_confirmed =
  "audit"`, the default, reports and holds and changes nothing about serving,
  while `on_confirmed = "block"` treats an unpublish as hostile and refuses it on
  the wire. The two are the two honest readings of an unpublish.

- **A closed registry can serve the one request that carries no credential
  (RFC 0012).** Terraform authenticates the two JSON documents of a provider
  install and then fetches the archive with no `Authorization` header — measured
  against a real client, not read, and not a configuration mistake, since the
  client has no mechanism to send one there. The documented workaround was
  `anonymous = ["releases:read", "source:read"]`, which is per registry: opening
  the last step of a provider install opened every read on it, every other
  provider, every version listing, and in hybrid mode everything published
  locally. `signed_downloads = true` on a registry now mints a signed, expiring,
  single-coordinate URL *inside the document that was already authenticated*, and
  the archive route accepts that signature as evidence of the authentication that
  happened. The signature carries the identity that fetched the document and
  verification runs the same rule chain as before: it authenticates a request, it
  authorises nothing. It requires `[server.signed_urls].secret`, and setting it
  without one is a startup error rather than a warning, because a registry that
  believes it is closed and is not is the failure the feature exists to prevent.

- **The documentation is published in French.** 66 pages under `/fr/` — the
  operator's guide, the package manager's guide, the registry pages and the
  operations runbooks. A translation lives at `docs/fr/<same path>` and declares
  in its frontmatter the page it translates and the revision it was translated
  from, so `task docs:i18n:check` fails when an English page moves ahead of its
  translation instead of letting the two drift silently. Three spaces stay in
  English by decision rather than by backlog: `contributing/`, read by people
  changing the code, `rfc/`, which is a record and gets quoted rather than
  rewritten in a second language, and the generated roadmap, whose French copy
  would be a second canonical roadmap no gate could keep true. VitePress does not
  fall back, so an untranslated page is linked at its English URL rather than
  omitted into a 404.

- **`--config` is repeatable, and each further file is a layer.** A deployment
  can keep its credentials in a file with a different lifecycle from the rest of
  its configuration — a Kubernetes Secret beside a ConfigMap, a `0600` file
  beside a readable one — without giving up hot reload on either. The merge is on
  the TOML *documents*, before deserialisation, so a later layer can complete a
  table an earlier one opened; tables merge key by key, arrays of tables match on
  `name` then `type`, everything else is replaced, and later layers win.
  `BATLEHUB_CONFIG` takes the same list separated by `:` where only environment
  variables are available. Every layer is watched and re-read on reload, and the
  first layer stays the only one the config editor reads or rewrites. The
  single-file case keeps its old code path deliberately, so its error spans
  survive.

### Changed

- **The upstream search's `limit` is capped at 100 per registry.** The search
  fans out across every registry the caller may browse, and each hit costs a
  coordinate in the "do we already hold this?" query, so an uncapped `limit` let
  one request size this instance's database work. Nothing observable changes
  today: every registry client already clamped the number lower before sending
  it upstream. A larger value is not an error, it is simply not honoured.

- **One documentation tree, and it wears the design system** (RFC 0005). The
  repository had two: `website/`, published, and `docs/`, in the repo and
  unpublished, with four documents maintained in both and drifted by up to 296
  lines. They are now one tree, `docs/`, split into spaces by *who reads it* —
  `guide/`, `registries/`, `operations/`, `contributing/`, `rfc/`, and an
  unpublished `internal/`.

  For a reader, the visible change is that things that were only in the
  repository are now on the site: the 3 666-line configuration reference (the
  most cross-referenced document in the project, and until now readable only by
  someone who had cloned it), the operations runbooks and compliance material,
  the contributor guides, and every RFC — each under a status banner generated
  from its own `Status` field, so a proposal is never published looking like a
  description of the product.

  **Published URLs do not change.** The merge only adds paths, and the publish
  prefix comes from `BASE_URL`, not from the directory name.

  For a contributor: the site builds from `docs/`, `task website:*` is now
  `task docs:*`, and two files in the tree are generated and must not be
  hand-edited — `docs/.vitepress/theme/tokens.css` (`task ui:tokens`) and
  `docs/guide/roadmap.md` (`task docs:roadmap`). Both have a drift check.

- **The public site now uses the design system, and is measured in a browser.**
  It had been carrying the world `DESIGN.md` replaced: its own colour tokens over
  the top of the real ones, an out-of-gamut crimson, glows and shadows and
  rounded corners in a system whose radius is zero, a sans face in a world with
  no sans, and — the only privacy-relevant part — a Google Fonts `@import`, so
  every reader's IP reached a third party before the first paragraph rendered.
  The fonts are self-hosted, the tokens are the console's, both light and dark
  are authored, and `task docs:design:rendered` measures every published page at
  two viewports in both renditions: axe at WCAG 2.2 AA, the type ramp, and the
  material rules.

- **The guide is split by audience, and every instruction has one home**
  (RFC 0005-bis). `docs/guide/` is now the operator's space — install, configure,
  administer — and the new `docs/use/` is for the person whose package manager
  talks to BatleHub: tokens, publishing, the CLI, the Package Explorer,
  troubleshooting. Pages that moved leave a redirect behind.

  Publishing instructions used to exist twice, sliced two ways: eleven numbered
  walkthroughs in one 1 118-line page, and a three-line section on each of the 21
  registry pages, with neither marked as the one to trust. The registry page is
  now the home and carries the walkthrough; the publishing page keeps only what
  is the same whatever you are publishing.

  The configuration reference gave up the six other documents it had become:
  worked examples, the server binary's subcommands, hot reload, private
  upstreams and capacity planning are pages now, and the SBOM section it
  duplicated is gone in favour of the SBOM page. It is 24% shorter and is only
  the reference.

  The home page opens with three cards instead of thirteen. Nothing was deleted
  — the full feature list is at [`docs/guide/features.md`](docs/guide/features.md).

### Security

- **The dependency invariants are enforced by a gate rather than by a comment.**
  Four constraints held this tree's supply chain in place and lived only in
  prose, so a routine bump could quietly undo one: `rsa` (RUSTSEC-2023-0071) kept
  out by stubbing `sqlx-macros` and `sqlx-mysql`, `aws-sdk-s3` and `aws-config`
  built without default features to avoid the legacy rustls line, `actix-web`
  built with its own default set **minus `http2`** — that feature is the only
  thing pulling `h2 0.3` (RUSTSEC-2026-0258), the fix is in 0.4.16, and there is
  no 0.3 backport — and a lockfile floor of `lru >= 0.18.2`
  (RUSTSEC-2026-0253). Nothing is lost with `http2`: this process never
  terminates TLS, so its HTTP/2 would only ever be h2c, and real deployments
  terminate it at the ingress. All four are now in the `[bans].deny` list of
  `deny.toml`, so a bump that drags one back into the tree fails `cargo deny`
  and CI rather than shipping.

- **CVE detection runs on every layer, and nothing is suppressed.** Rust
  dependencies through `cargo audit` and `cargo deny`, JavaScript through
  `pnpm audit` for both pnpm projects, the source-repo reputation and lockfile
  vulnerabilities of every dependency root through
  [postmortem](https://github.com/mlab-sh/postmortem) with its findings in Code
  Scanning, the built images through Trivy — the proxy image, the scan-worker
  image and its GuardDog variant — blocking on fixable HIGH and CRITICAL, and
  CodeQL, Semgrep and gitleaks over the source. The stance is no suppressions:
  `advisories.ignore` stays empty in `deny.toml` and in `.cargo/audit.toml`, so
  a finding is fixed or patched rather than muted. `task security` reproduces the
  dependency and SBOM gate locally, and
  [Security scanning](docs/contributing/security-scanning.md) carries the full
  matrix.

### Fixed

- **A package found by an upstream search was reported as not held, however
  often it had been pulled.** An upstream search is a relevance search: npm
  answers `left-pad` with `pad-left` and `lpad`, and neither contains the query.
  The catalogue decided "do we already have this?" by looking for packages whose
  name *contains the query*, so no fuzzy hit could ever be credited. It asks by
  the names that came back now.
- **A NuGet, PyPI or Go package the instance held was still reported as
  missing.** NuGet's search answers `Newtonsoft.Json` where `dotnet restore`
  stores `newtonsoft.json`, PyPI's answers `Pillow` where the simple index
  stores `pillow`, and pkg.go.dev answers `github.com/BurntSushi/toml` where the
  `go` client stores `github.com/!burnt!sushi/toml`. The catalogue compared the
  two spellings exactly, so it offered **Fetch** on a package it already had and
  the button answered `409`. Each kind's own naming rule now decides, and the
  read path's normalisers share that one definition.
- **A version fetched from the console stayed missing from the catalogue for ten
  minutes.** The listing is served from a cache invalidated on a publish and on
  a yank; a console fetch is a third write and was not on the list, so the row
  went on offering to fetch a version the instance already held, and a second
  press answered `409`. A package manager's download still does not invalidate
  it, deliberately.
- **Proxied extensions lost their upstream signature.** The gallery proxy
  re-rendered every entry with a fixed six-asset list, so an extension
  proxied from the Microsoft marketplace arrived unsigned and a current
  editor refused to install it. The upstream's `VsixSignature` (and Open
  VSX's `PublicKey`) are now relayed byte for byte, cached beside the VSIX,
  and never re-signed.

- **Setup snippets name the host the client actually talks to.** With host-based
  routing (RFC 0001) a registry answers on its own subdomain, and on that host
  the server prefixes `/proxy/{name}` to every path itself. Three things had not
  followed:

  - The Setup Guide's composite tabs (`mise`, and the `generic` mirror rules)
    rewrite downloads to several registries but printed a single `~/.netrc`
    stanza — the selected registry's. Every other host got no credentials and
    would have 401'd. They now print one stanza per host referenced.
  - Two snippets hand-built `https://{host}/proxy/{name}/…` (the pip.conf
    embedded-credentials line, the apt `sources.list` alternative), which on a
    registry host resolves to `/proxy/{name}/proxy/{name}/…` and 404s. Both now
    derive from the registry's own base URL.
  - `batlehub-cli setup detect` / `setup ide` ignored host routing entirely:
    they never read `public_url` and always printed `{server}/proxy/<registry>`.
    They now ask the server for the registry list, name the real registry, point
    at its own host when it has one, and end with the matching `~/.netrc`
    stanzas. `--offline` keeps the old placeholder output, which is also the
    fallback when the server cannot be reached.


- **Every hand-written table of contents in the documentation was broken, and
  had always been.** VitePress prefixes an anchor that starts with a digit with
  `_`, so every entry pointing at a numbered section — `#1-prerequisites` and its
  115 siblings — resolved to nothing and silently dropped the reader at the top
  of the page. Nothing checked fragments. All sixteen typed contents lists are
  gone (the theme draws the outline from the headings), the surviving
  cross-references were repaired, and `task docs:links` now checks anchors.

- Two documentation cross-references that had been dead
  (`docs/post-mortem-template.md`, `docs/monitoring.md`), an accessibility
  failure in every collapsible sidebar group (a focusable control inside a
  `role="button"`), code blocks that scrolled but could not be focused by
  keyboard, and syntax-highlighting colours that missed AA against the light
  ground. `task docs:links` and the rendered gate keep all of these from
  recurring.

## [1.1.0] - 2026-08-10

### Breaking

- **`[server].cors_allowed_origins` now defaults to same-origin only.** An empty
  or absent list previously meant `allow_any_origin()`: any website a visitor
  opened could issue cross-origin requests to this server and read the responses.
  Credentials are never sent cross-origin, so this was never a path to stealing a
  token — but for a registry proxy inside a private network it let a public page
  enumerate internal package metadata using the visitor's browser as its network
  position.

  **Nothing to do** if the UI is served from the same origin as the API — the
  default, and what every Helm-chart deployment does, since same-origin requests
  never consult CORS. If the UI lives on another origin, name it:

  ```toml
  [server]
  cors_allowed_origins = ["https://ui.example.com"]
  ```

  `cors_allowed_origins = ["*"]` restores the old behaviour verbatim and is now
  the explicit opt-out. It raises a `cors.any-origin` config warning, surfaced at
  `GET /api/v1/admin/config/warnings` and on the Config Reload page, so a
  wildcard copied forward from an old config does not stay invisible.

### Added

- **Host-based (subdomain) registry routing.** A registry can now be bound to one
  or more hostnames whose root serves it, in addition to `/proxy/{name}/…`:
  `https://npm.acme.io/lodash` means exactly what
  `https://hub.example.com/proxy/npm1/lodash` means. Configure a wildcard with
  `[subdomain_routing]` (`enabled` + `base_domain`), vanity hosts with a
  registry's `hosts = […]`, or both. Every self-referencing URL the server
  generates — npm `dist.tarball`, the NuGet service index and registration
  `@id`s, the PyPI simple index, Composer `metadata-url`/`dist`, the Terraform
  provider `download_url`, the cargo index `dl`/`api` — now reflects the ingress
  the client actually used. Off by default; with no hosts configured every
  generated URL is byte-identical to before. See the
  [Host-based routing guide](https://batleforc.git.batleforc.fr/batlehub/guide/host-routing).
- `registries[].path_routing = false` makes a registry reachable **only** through
  its host(s); `/proxy/{name}/…` then returns 404 (not 403 — a disabled ingress
  should look absent). A registry with no reachable ingress is a config error.
- `GET /api/v1/registries` gained `public_url`, the registry's hostname-rooted
  URL when it has one. The Setup Guide and namespace upload snippets use it.
- **`[server].trusted_proxies`** — one server-level CIDR list governing which
  peers may set `Forwarded` / `X-Forwarded-Host` / `X-Forwarded-Proto` /
  `X-Forwarded-For`. Previously the forwarded host and scheme were trusted
  unconditionally while only the client IP had a rule; now all three follow one
  verdict computed once per request. Bare IPs are accepted as `/32` (`/128`).
- **Config warnings** — `AppConfig::warnings()`, surfaced at
  `GET /api/v1/admin/config/warnings`, inline in the responses of
  `/config/validate` and `/config/from-content`, and rendered on the Config
  Reload admin page. First users: an unstated proxy-trust policy, a shadowed
  deprecated key, and a registry name that cannot become a DNS label.
- Helm: `ingress.extraHosts` for the additional hostnames, and a documented
  `config.server.trusted_proxies`.
- `pending_created` on the config-reload responses. `POST /config/from-content`
  answers `200` with an empty diff both when it stages a pending reload and when
  the submitted content is byte-identical to the last load attempt, in which case
  there is nothing to stage; the flag tells the two apart instead of leaving the
  caller to find out from a `404 No pending reload` at apply time. `false` for
  `/config/validate` (a dry run) and for `/config/reload` and
  `/config/pending/apply` (which consume a pending rather than leave one).
- **`GET /livez`** — unauthenticated liveness probe. Answers `200` with the running
  version as long as the process is up, and performs no I/O. `/healthz` remains the
  readiness endpoint: it checks database and storage and answers `503` when either
  is unreachable, which removes the pod from the Service without restarting it.
  Splitting the two matters because restarting a container reaches neither an
  unavailable database nor an unavailable object store — a dependency check on the
  liveness path turns a brief Postgres outage into a CrashLoopBackOff across every
  replica simultaneously.
- Helm: `podDisruptionBudget` (rendered only when `replicaCount > 1`, since a PDB
  over a single replica blocks node drains without buying availability) and an
  opt-in `networkPolicy`. The network policy is off by default because the correct
  egress set depends on which upstreams you proxy; when enabled it always emits a
  DNS egress rule first, as a default-deny policy without one breaks every upstream
  lookup. `podDisruptionBudget.maxUnavailable` takes precedence over `minAvailable`
  when set, including an explicit `0`.
- Helm: filesystem storage with `persistence.enabled: false` now mounts an
  `emptyDir` at the cache path, sized by `persistence.ephemeralSizeLimit`
  (default `1Gi`). The cache stays ephemeral as before, but `readOnlyRootFilesystem:
  true` means the container filesystem can no longer supply a writable path of its
  own, and without a mount every artifact write would fail.

### Changed

- `[server].trusted_proxies` is now hot-reloadable, and is swapped just before
  the host-routing table it guards. A reload that turns host routing on used to
  keep the startup trust policy until the process restarted, which left routing
  driven by `X-Forwarded-Host` from any peer — the state config validation exists
  to make unreachable.
- The rate limiter buckets anonymous clients on the same client IP the IP-block
  middleware bans, instead of the raw TCP peer. Behind a trusted proxy the two
  previously disagreed: one abusive client could exhaust a bucket shared by every
  anonymous user, and the resulting `429`s then counted as violations against
  each innocent client's own IP.
- Registry names are matched on the path actix routes on rather than the raw
  URI. Percent-encoding a character of the name (`/proxy/npm%32/…`) reached the
  registry's handler while slipping past both the `path_routing = false` 404 and
  the registry's rate limit.
- `ui/openapi.json` is tracked in git, so an API change shows up in review as a
  diff of the contract. Refresh it with `task dump-spec`, which needs no database.
  The generated TypeScript client under `ui/src/client/` stays untracked.

### Fixed

- **`X-Forwarded-For` is now read right to left**, skipping hops that fall inside
  `trusted_proxies`, instead of taking the left-most entry. Each hop appends the
  address it observed, so everything left of the entry our own proxy wrote is
  client-supplied: behind a trusted proxy, any client could name the IP that
  `[ip_blocking]` bans and the anonymous rate-limit bucket is keyed on — evading
  its own ban, or getting a third party blocked. Entries are parsed as IP
  addresses (the `ip:port` and `[ipv6]:port` forms included) and the walk stops
  at anything that does not parse, falling back to the TCP peer address rather
  than stepping over a hop it cannot classify. Deployments with no
  `trusted_proxies` list are unaffected — they still ignore the header entirely.
- The Setup Guide's `.netrc` block lists every host a client may authenticate
  against. `.netrc` entries are matched by hostname, so a guide naming only the
  main host meant no credentials were sent to a host-routed registry and every
  authenticated install failed with `401`.
- The Terraform `source` snippet drops the registry segment for a host-routed
  registry, where provider endpoints live at the root, and keeps the port, which
  `terraform init` needs on any deployment not served on 443.
- `POST /config/from-content` reports the warnings of the config submitted rather
  than of the one still in force when the content matches the last load attempt.
  An admin staging a config with warnings could see an empty warning panel.
- **The Helm chart's liveness and readiness probes no longer target an
  authenticated endpoint.** Both pointed at `GET /api/v1/admin/health`, which is
  `require_admin`-gated; the kubelet sends no credentials, so every probe was
  answered `403`. A fresh install never became Ready and liveness restarted the
  container on a loop. Readiness now uses `/healthz` and liveness the new
  `/livez`. A rendered-manifest check in CI (`.github/workflows/helm-lint.yaml`)
  fails the build if a probe path moves back under `/api/`.
- **The package explorer no longer lists packages the caller cannot download.**
  `GET /api/v1/explore/packages` and `/api/v1/explore/packages/{registry}/{name}`
  gated on *registry*-level access only, so the name, version count and download
  total of an `internal` or `team` package were visible to anyone who could explore
  that registry — even though the same caller got a `403` from the download path,
  where `check_visibility` has always been enforced. Artifact contents were never
  exposed; the leak was metadata, which for a private registry is often the
  sensitive part.

  The listing now applies the same three rules in SQL (`public` → everyone,
  `internal` → authenticated, `team` → member of the longest-prefix namespace
  claim, admins bypass), and the detail endpoint answers **404** rather than 403
  so a denial does not confirm the package exists. The paginated count query
  applies the identical predicate, so totals match the rows returned.

  The explore result cache is keyed on the viewer as well — without that, the
  first caller to populate an entry would have their filtered view served to
  everyone who followed.
- **Baseline security headers on every response** — `X-Content-Type-Options:
  nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`. Applied
  outside the IP-block and rate-limit layers so their `403`/`429` responses carry
  them too. A handler that sets its own value keeps it.
- **Streamed artifacts always declare a `Content-Type`.** Eight routes — raw
  repository files from GitHub, GitLab and Forgejo, npm tarballs, `.vsix` bundles,
  JetBrains plugin archives — passed `None` to `proxy_stream`, which sent no
  `Content-Type` at all, leaving the browser to MIME-sniff. Artifacts are served
  from the same origin as the admin SPA, which holds bearer tokens in
  `localStorage`, so a "raw file" containing HTML could execute as a document on
  that origin. `proxy_stream` now falls back to `application/octet-stream`, and
  `nosniff` removes the sniffing step entirely; the two together close the path.
- The SPA declares a `Content-Security-Policy` (`script-src 'self'`,
  `object-src 'none'`) in `ui/index.html`. It lives in the document rather than a
  response header because it must not apply to `/scalar`, whose bundle is loaded
  from a CDN. `frame-ancestors` is ignored in meta form, which is why
  `X-Frame-Options` is sent for every response instead.
- **Both container images now run as a non-root user** (`USER 65532:65532`). Neither
  `Containerfile` nor `Containerfile.hardened` declared a `USER`, so the server ran
  as root with a writable root filesystem and the full default capability set. The
  artifact-cache directory is copied in already owned by that UID; the binaries and
  the SPA bundle stay root-owned and read-only to the runtime user.
- **The Helm chart ships a real security context.** `podSecurityContext` and
  `securityContext` were both `{}`, so nothing stopped the workload from running as
  root even on a cluster that could have enforced otherwise. Defaults are now
  `runAsNonRoot` / `runAsUser: 65532` / `fsGroup: 65532` (so the cache PVC is
  writable) / `seccompProfile: RuntimeDefault`, plus `allowPrivilegeEscalation:
  false`, `readOnlyRootFilesystem: true` and `capabilities.drop: [ALL]`. The pod is
  admissible in a Pod Security Admission `restricted` namespace unmodified.
- Pinned `js-yaml` to `^4.3.1` and `nanoid` to `^3.3.17` in `ui/`. The existing
  `js-yaml: ^4.3.0` pin covered GHSA-52cp-r559-cp3m but still resolved 4.3.0,
  leaving GHSA-5p4m-2wfm-xmqj open; `nanoid` (GHSA-2v37-7h3g-55p8) had no pin and
  arrives through postcss on 27 paths. Both are build-time only, but the `ui` leg
  of `dep-audit-frontend` audits the whole tree, so the job was failing. `website/`
  gets the same `nanoid` pin so its green result does not depend solely on the
  `--prod` filter.

### Deprecated

- `[ip_blocking].trusted_proxies` — use `[server].trusted_proxies`. The old key
  keeps working (and now governs the forwarded host and scheme too, so an
  existing deployment can adopt host routing without touching it), but raises a
  config warning. When both are set, `[server]` wins. An entry of the old key
  that is not an IP or CIDR range is still dropped — with a
  `proxy-trust.invalid-deprecated-entry` warning — rather than failing the boot,
  since that key never validated its entries before.

## [1.0.0] - 2026-07-17

First stable release.

### Security

- **SSRF hardening** across registry adapters, including OpenVSX upstream requests
- **Signed-release enforcement** (`RequireSignedReleaseRule`) — optionally require GitHub/OpenVSX/VS Code Marketplace releases to carry verifiable signatures before they're served, with a role-based bypass
- Open-source release housekeeping: `LICENSE` (Apache-2.0), `SECURITY.md`, `CONTRIBUTING.md`

### Reliability

- Fixed the config hot-reload watcher retriggering without a real change; a reload loop that fires more than a few times within 30s without settling now stops and surfaces a warning instead of looping forever
- Large hardening/bug-fix pass across handlers and services following an in-depth code review

### Developer experience

- Frontend lint job added to CI (`front-test.yaml`)
- Dependency upgrades across the Rust workspace (including `sqlx`) and the UI toolchain
- Continued UI rework (routing, navigation) and codebase health cleanup (dead code, duplication)

## [0.5.0] - 2026-06-29

### Registry adapters

- **Arch Linux / Pacman** (`type = "pacman"`) — proxy upstream Arch mirrors **and** private hosting in `local`/`hybrid` mode: `.pkg.tar.{zst,xz,gz}` publish (metadata read from `.PKGINFO`), per-arch `<repo>.db`/`<repo>.files` database regeneration, Ed25519 OpenPGP-signed database (`<repo>.db.sig`) and packages (`.sig` + embedded `%PGPSIG%`) so `SigLevel = Required` works. Signing reuses the hand-rolled Ed25519 signer (the `rsa` crate is banned)

### Vulnerability management

- **OSV vulnerability scanning** — per-registry `cve_gate` rule (`min_severity`, `block`/warn-only, `bypass_roles`); periodic background re-scan via `[vulnerability_scan]` task; findings stored in `artifact_vulnerabilities` DB table; per-version CVE status surfaced in the Package Explorer and admin views
- **Go module vulnerability database proxy** — GOPROXY vuln endpoint (`/proxy/{reg}/goproxy/vuln/`) proxied so `govulncheck` and related tooling can query BatleHub directly without reaching the public database
- **NuGet vulnerability endpoint proxy** — NuGet v3 vulnerability endpoint wired into the service index so `dotnet restore` vulnerability checks flow through the proxy cache
- **Vulnerability scanner extension point** — documented API for adding custom vulnerability scanners (`docs/adding-a-vulnerability-scanner.md`); `docs/vulnerability-proxy.md` covers the proxy-side configuration

### Admin & security

- **User block management** — DB-backed user block list (`028_user_blocks` migration); `UserBlockMiddleware` evaluates the block list before any request handler and returns 403; admin API (`GET/POST/DELETE /api/v1/admin/users/blocks`); Admin Users page in the UI lists OIDC, Kubernetes, and static-token identities with block/unblock actions; fails open on DB errors to avoid locking out admins

### Developer experience

- **Eclipse Che workspace login** — login page detects Eclipse Che environment variables and displays pre-configured connection instructions for workspace-hosted instances
- **CLI download command** (`batlehub-cli download`) — downloads an artifact from any configured registry to a local file; auto-detects registry type and constructs the correct download URL
- **SonarCloud integration** — `.github/workflows/sonar.yaml` runs frontend (Vitest LCOV) and backend (cargo-llvm-cov LCOV, with Postgres/MinIO/Redis services) coverage and uploads both reports to SonarCloud on every push to `main`

### Bug fixes

- JetBrains artifact post-copy path handling corrected; improved `docs/path-mapper.md` to clarify URL routing for large IDE archives
- TOCTOU race condition fixes and general code-review hardening across several handler paths
- Correct handling of unreachable match arms and unused assigned values flagged by the compiler

### Code quality

- Code duplication reduced below 5% (tracked via SonarCloud)
- Container image updated to TiKV-based build; `Containerfile` and `Containerfile.hardened` both updated

---

## [0.2.0] - 2026-06-14

### Registry adapters

- **npm** — proxy with scoped package support; local/hybrid publish
- **Cargo** — sparse index proxy compatible with `cargo` sparse protocol; local/hybrid publish
- **GitHub Releases** — artifact download proxy for GitHub release assets
- **OpenVSX** — VS Code extension proxy for the open-source marketplace
- **VS Code Marketplace** — VSIX download proxy for the official marketplace
- **Go modules (GOPROXY)** — Go module proxy protocol (`$GOPROXY`); multi-segment module path routing via `{module:[^@]+}` pattern
- **Maven / Gradle** — Maven Central-compatible metadata XML + JAR / POM downloads; private publishing via `mvn deploy` (three-phase POM + JAR + checksum upload); dynamically generated `maven-metadata.xml` from DB; local/hybrid mode
- **Terraform** — provider and module proxy protocol; private module (tar.gz + `X-Terraform-Get` redirect) and provider (version manifest + per-platform binary) publishing; local/hybrid mode
- **RubyGems** — gem download and version listing; local/hybrid mode with yank / unyank
- **Composer** — Packagist v2 protocol (`packages.json`, p2 metadata, dist downloads); private package ZIP upload; local/hybrid mode
- **PyPI** — Simple API proxy with URL rewriting; private wheel / sdist publishing via `twine`; Simple API served from DB; local/hybrid mode
- **Conda / Anaconda** — `repodata.json` proxy and channel merging; `.tar.bz2` and `.conda` package parsing; private channel publishing; local/hybrid mode
- **NuGet** — NuGet v3 service index + flat container proxy; `.nupkg` and `.nuspec` downloads; private publishing via `dotnet nuget push`; `X-NuGet-ApiKey` normalised to `Authorization: Bearer`; local/hybrid mode

### Authentication

- **Static tokens** — plain-text Bearer tokens and Argon2id PHC hashes in `config.toml`; `batlehub hash-token <token>` CLI helper
- **OIDC** — JWT validation via OIDC discovery + JWKS; browser SSO (Authorization Code flow); role and group mapping from claims; namespaced group prefixes for multi-provider setups
- **Kubernetes service accounts** — TokenReview API validation; role and group mapping; in-cluster defaults
- **GitHub / Forgejo Actions OIDC** (`type = "actions-oidc"`) — short-lived JWT validation for workflow jobs; claim-to-group mapping (`repository`, `ref`, `environment`, `actor`, …) with static and dynamic templates; glob and regex pattern matching; AND / OR condition logic per rule

### Access control & policy

- **RBAC engine** — role/group rules per registry with `pull` / `push` / `admin` actions; evaluated by `RbacRule`
- **Built-in policy rules** — `DenyLatestRule` (block floating `latest` tags), `BlockListRule` (explicit package/version deny list), `ReleaseAgeGateRule` (reject versions younger than a configured age)
- **Rate limiting** — per-user and per-registry token-bucket rate limits; per-group shared pools; hard block or soft warn enforcement; `Retry-After` and `X-RateLimit-*` response headers; state resets on restart
- **IP blocking** — fail2ban-style blocking via `IpBlockStore`; configurable block duration and thresholds; outermost `actix_web::middleware::Condition` middleware
- **Publish quota** — per-user, per-group, and per-registry quotas on storage usage and package count; `X-Quota-*` response headers; admin API for viewing and resetting quotas; enforcement policies: block or warn

### Cache

- **Cache-Control honouring** — respects `no-cache`, `max-age`, and `no-store` from upstream responses
- **Eviction policies** — TTL-based expiry, "not accessed for N days", garbage-collect all versions except the latest N, storage-size cap with LRU eviction
- **Content-addressable deduplication** — identical artifact bytes stored once; ref-counted via `artifact_dedup_index` / `artifact_dedup_refs`; backwards-compatible with pre-dedup artifacts
- **Proactive cache warming** — pre-fetch known versions on startup and on demand via `POST /api/v1/admin/registries/{registry}/warm`; configurable `warm_packages`, `warm_latest_n`, `warm_concurrency`
- **Explore cache** — 10-minute in-memory cache for the explore list and stats; stale-on-DB-error fallback; admin invalidation via `POST /api/v1/admin/explore/invalidate`; auto-invalidated on local publish

### Private registry features

- Local and hybrid operating modes for all supported registry types
- **Ownership management** — per-package owner table (user/group; admin/maintainer roles); `initialize_owner` on first publish; `can_publish` check on subsequent publishes; admin API to list / add / remove owners
- **Versioning policies** — `enforce_semver`, `allow_prerelease`, `version_pattern` (regex) per registry; enforced at publish time with HTTP 422
- **Beta / pre-release channel** — per-registry allow-list of users or groups who may access unpublished versions (`BetaChannelPort`, DB-backed)
- **Artifact signing** — `X-Artifact-Signature` / `X-Signature-Type` headers at publish; signature stored in DB and returned on download; optional `signing.required` enforcement
- **Bulk operations** — `POST /api/v1/admin/registries/{registry}/bulk-yank|bulk-unyank|bulk-delete`

### SBOM

- Per-artifact SPDX 2.3 and CycloneDX 1.4 generation at proxy time and at publish time; archive manifest extraction (Cargo.toml, package.json, pom.xml, go.mod, requirements.txt, …)
- Upstream SBOM fetch from GitHub dependency graph API and npm `bom.json`
- Org-level SBOM export — all artifacts served in a time range as a single merged document (`GET /api/v1/sbom/export?from=…&to=…&format=spdx|cyclonedx`); admin UI at `/admin/sbom`
- `required = true` policy option in `[registries.sbom]` — deny publishing a private package when no manifest is found in the archive
- Per-artifact SBOM download buttons (SPDX and CycloneDX) in the Package Explorer version detail view

### Hot reload & dynamic config

- `HotConfig` behind `Arc<RwLock<HotConfig>>` — in-flight requests finish with the old snapshot; config swap is atomic
- File watcher (`notify` crate) — loads a pending reload; admin confirms via `POST /api/v1/admin/config/pending/apply` or discards with `DELETE /api/v1/admin/config/pending`
- Schema validation and upstream connectivity probes before storing a pending reload
- Config audit trail — every reload is recorded in `config_changes` table; retrievable via `GET /api/v1/admin/config/changes`
- **Global admin banner** — broadcast info / warning / error to all visitors; backed by in-memory, Redis, or PostgreSQL; `PUT/DELETE /api/v1/admin/banner`
- `BATLEHUB_DISABLE_HOT_RELOAD=1` env var — disables the file watcher and all reload endpoints (for read-only Kubernetes ConfigMap mounts)

### Webhooks & notifications

- Outbound notification channels: email (via `lettre`), Slack, Microsoft Teams, HTTP webhooks
- DB-backed subscriptions — subscribe to events per package, version, or registry (new version published, version deprecated, package removed)
- Fire-and-forget dispatch integrated into all publish and yank handlers
- Inbound webhook receiver — external systems (CI pipelines, security scanners) can push events into BatleHub

### Observability

- Prometheus metrics endpoint (`/metrics`) — request counts, cache hit/miss rates, latency percentiles, error rates per registry
- Health check endpoint (`/healthz`) — verifies connectivity to the database and all configured storage backends
- Stats dashboard on the admin home screen — hits/misses, bandwidth saved, per-registry and aggregate

### CLI (`batlehub-cli`)

- Full command tree: `registry list|info`, `package list|versions`, `version yank|unyank|delete`, `owners list|add|remove`, `publish`, `auth whoami|login|refresh`, `token list|create|revoke`, `admin`, `config init|show|set`, `completion`, `hash-token`
- `batlehub-cli publish <file>` — auto-detects registry type, package name, and version from the artifact (`detect_meta`); supports all local/hybrid registry types
- `batlehub-cli auth login` — OIDC Authorization Code browser flow with token caching; Kubernetes token path support; auto-refresh on startup
- Shell completion for bash, zsh, fish, and others via `batlehub-cli completion`
- Named profile config at `~/.config/batlehub/config.toml`; global flags `--profile`, `--server`, `--token`, `--registry`, `--json` and `BATLEHUB_*` env-var equivalents
- **TUI mode** (`batlehub-cli tui`) — ratatui / crossterm terminal UI with: registry list, package explorer with live search/filter, package detail (yank / unyank keybindings), publish form, setup wizard (scans local manifests and shows per-type config snippets + publish commands), login screen (OIDC / Kubernetes / static token)

### UI (Vue 3 SPA)

- **Package Explorer** (`/explore`) — collapsible registry catalog sidebar; search and sort across cached and upstream packages; per-package detail page with version history and gate/firewall status per version; independent search permissions via `[registries.rbac.explore]`
- **Setup Guide** — API-driven; tabs appear only for registry types configured on the server; per-type config snippets and client commands defined in `ui/src/config/registryTypes.ts`
- **Monofolio design system** — OKLCH colour tokens, 2 px sharp corners, crimson + copper palette, JetBrains Mono font, cyber-grid background, `text-copper` utility class
- **Admin pages** — config reload (pending/apply flow, audit log), global banner editor, SBOM org export, webhook / notification subscription management

### Infrastructure

- Helm chart for Kubernetes deployment (`helm/`)
- Hardened OCI container image (`Containerfile.hardened`) with minimal attack surface
- Forgejo CI/CD workflows — lint (`cargo clippy -D warnings`), format check, tests, ≥ 80% line coverage, container build
- `sqlx-macros` and `sqlx-mysql` patched to empty stubs in `[patch.crates-io]` to remove the `rsa` crate (RUSTSEC-2023-0071)
- `aws-sdk-s3` and `aws-config` with `default-features = false` to avoid `legacy-rustls-ring` (RUSTSEC-2026-0098 / 0099 / 0104)
- Fuzz targets for RBAC evaluation, cache key generation, deny-latest rule, and release age gate (`task fuzz`)

---

[Unreleased]: https://git.batleforc.fr/batleforc/batlehub/compare/v1.3.0...HEAD
[1.3.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v1.2.0...v1.3.0
[1.2.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v1.1.0...v1.2.0
[1.1.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v1.0.0...v1.1.0
[1.0.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v0.5.0...v1.0.0
[0.5.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v0.2.0...v0.5.0
[0.2.0]: https://git.batleforc.fr/batleforc/batlehub/releases/tag/v0.2.0
