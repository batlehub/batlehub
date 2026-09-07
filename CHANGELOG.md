# Changelog

All notable changes to BatleHub will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

---

## [Unreleased]

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

### Fixed

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

[Unreleased]: https://git.batleforc.fr/batleforc/batlehub/compare/v1.1.0...HEAD
[1.1.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v1.0.0...v1.1.0
[1.0.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v0.5.0...v1.0.0
[0.5.0]: https://git.batleforc.fr/batleforc/batlehub/compare/v0.2.0...v0.5.0
[0.2.0]: https://git.batleforc.fr/batleforc/batlehub/releases/tag/v0.2.0
