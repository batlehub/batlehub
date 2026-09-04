# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

All task runner commands use `task` (Taskfile). `Taskfile.yml` is only the
includes and the shared vars; the tasks live in `.tasks/<domain>.yaml`
(`rust`, `test`, `coverage`, `security`, `ui`, `docs`, `compose`, `browser`,
`dex`, `perf`) and are included with `flatten: true`, so every task keeps its
plain name — `task ui:dev`, not `task ui:ui:dev`. Add a task to the file whose
prefix it shares.

The key ones:

```bash
# Build / check
cargo build --workspace
cargo check --workspace

# Tests
cargo test --workspace                              # all unit + in-process integration tests
cargo test --package batlehub-web nuget            # single package, filter by name
cargo test --package batlehub-adapters --lib rbac  # lib-only tests (no integration)
cargo test -p batlehub-cli --test integration      # CLI integration tests (subprocess binary)

# Housekeeping — cargo never garbage-collects target/
task clean:stale     # cargo sweep --time 1: drop artefacts not touched since yesterday

# Linting (CI fails on any warning)
cargo clippy --workspace -- -D warnings
cargo fmt --all --check

# Format
cargo fmt --all

# Coverage (requires Podman — starts Postgres + MinIO)
task coverage        # HTML report in coverage/html/
task coverage-check  # fails if line coverage < 80%

# Integration tests that need real Postgres (via Podman)
task test:pg-cache
task test:pg-local-registry

# Integration test that needs real S3/MinIO
task test:s3

# Run server (requires Postgres)
cargo run -p batlehub-server -- --config config.example.toml

# Frontend
# Two dev servers; there is no /api proxy, each calls VITE_API_BASE_URL directly.
task ui:dev                   # 5173 → API via the workspace FQDN (`url back`), for a real browser
task ui:dev:local             # 5174 → API on localhost:8080, for in-pod tools and the rendered gates
# Both set VITE_API_BASE_URL explicitly: this Taskfile has `dotenv: [ .env ]`, and
# Vite prioritises process.env over its own .env files, so an inherited value in
# the repo-root .env otherwise wins and both fronts land on the same API.
cd ui && pnpm run generate    # regenerate TypeScript client from ui/openapi.json
task dump-spec                # refresh ui/openapi.json from running server

# Fuzz
task fuzz:check                                 # every target compiles — the per-PR CI gate
task fuzz TARGET=fuzz_rbac_evaluate MAX_TIME=30 # actually fuzz one (nightly)
```

## Architecture

### Crate layout

```
crates/core      — domain: entities, ports (traits), rules, services (no I/O)
crates/config    — TOML schema (AppConfig, RegistryConfig, …) + loader
crates/adapters  — infrastructure: HTTP registry clients, Postgres, Redis, S3, in-memory impls
crates/web       — actix-web handlers, middleware, extractors
crates/examples  — integration test helpers and smoke/real-proxy test binaries
server/          — binary: wires everything together, no domain logic
cli/             — batlehub-cli binary: clap commands + reqwest API client + ratatui TUI
ui/              — Vue 3 + Vite SPA (TypeScript, Tailwind, shadcn-inspired components)
docs/            — the documentation: a VitePress site and the only documentation
                   tree (its own pnpm project; covered by `task docs:*`)
```

Registry adapters are gated behind `registry-*` cargo features (all on by default) declared in `crates/adapters/src/registry/mod.rs` and the `[features]` block of `crates/adapters/Cargo.toml`.

The dependency direction is strict: `core` ← `adapters` ← `web` ← `server`. The `config` crate is read only by `server` and `web`.

### Request lifecycle

1. **Auth middleware** (`crates/web/src/middleware/auth.rs`) — iterates `Vec<Arc<dyn AuthProvider>>`, resolves to an `Identity`, stores it in request extensions. `X-NuGet-ApiKey` is normalised to `Authorization: Bearer` in `extractors::raw_auth_from_request` before providers see it.

2. **Handler** (`crates/web/src/handlers/proxy/<registry>.rs`) — calls `require_registry_type` + `require_local_mode` guards, then either:
   - **Local/hybrid mode**: reads from `LocalRegistryService` (database-backed)
   - **Proxy/hybrid fallthrough**: delegates to `ProxyService::handle`

3. **ProxyService** (`crates/core/src/services/proxy.rs`) — acquires a short-lived read lock on `HotConfig` to clone the `Arc<RegistryClient>` and `Arc<RegistryPolicy>`, then:
   - Resolves metadata (cache-first, stale-on-error optional)
   - Evaluates rules (`RbacRule`, `DenyLatestRule`, `BlockListRule`, `ReleaseAgeGateRule`)
   - Streams artifact from upstream (or serves from storage cache)

### Hot reload

`HotConfig` is wrapped in `Arc<RwLock<HotConfig>>` (`HotConfigLock`). Handlers snapshot the parts they need by cloning `Arc<>` before any `await`. Config reload replaces the entire inner value atomically. In-flight requests finish with the old snapshot.

### Adding a new registry adapter

1. **`crates/adapters/src/registry/<name>.rs`** (or `<name>/client.rs` — see below) — implement `RegistryClient` (`registry_type`, `resolve_metadata`, `fetch_artifact`; optionally `list_versions`, `search_packages`).
   - **Start as a single flat `<name>.rs` file** when the upstream's JSON responses are small enough to deserialize with a couple of private structs defined inline (see `NpmRegistryClient` in `npm.rs`, or `cargo.rs`, `openvsx.rs`).
   - **Split into a `<name>/` directory** (`client.rs` + a dedicated `models.rs` for the upstream response DTOs, plus `tests.rs` once the test module gets long) once those DTOs would otherwise crowd out the request/auth logic in the client file, or the registry needs more than one protocol facet (e.g. `terraform/` has separate `modules.rs`/`providers.rs`; `composer/` splits `client.rs`/`local.rs`/`impl_registry.rs`). Use `NugetRegistryClient` (`nuget/client.rs` + `nuget/models.rs`) as the reference for the directory layout.
2. **`crates/adapters/src/registry/mod.rs`** — add `#[cfg(feature = "registry-<name>")] pub mod <name>` entry, **and** declare `registry-<name>` in the `[features]` block of `crates/adapters/Cargo.toml` (add it to `default` so it ships in the standard build). Without the feature declared, the `cfg` never activates.
3. **`crates/web/src/handlers/proxy/<name>.rs`** — actix-web handlers; use `proxy_stream`, `require_registry_type`, `require_local_mode`, `content_type_for` helpers from `common.rs`. **Validate any package name/version taken from the request** with `batlehub_core::services::validate_package_name` (and reject `..`/separators in version) before it reaches a storage key — see the PyPI/Composer handlers. Two funnels enforce this in depth — `ProxyService::handle` (every proxy read) and `LocalRegistryService::get_artifact` (every local read) call `validate_coordinate` on the `PackageId`, and `ensure_safe_key` guards the storage backends as the last line of defense — but a handler that builds a storage key directly (e.g. the Maven, NuGet flat, and Terraform-provider paths) must still validate at the edge for a clean `400`, not rely on the deeper guards.
4. **`crates/web/src/lib.rs`** — register routes with `cfg.service(...)` and add the `utoipa` tag. **Every `200`/`201` in a `utoipa::path` must declare `body = T`** — `crates/web/tests/openapi_contract.rs` fails on any that does not, because a schema-less success response makes the generated TypeScript client emit `unknown` and leaves the docs site's API reference blank. Use a real DTO where one exists, otherwise the shared markers in `crates/web/src/handlers/schemas.rs` (`ArtifactBytes`, `UpstreamDocument`, `ProtocolDocument`, `OkResponse`, `MessageResponse`); an ad-hoc `json!` of the handler's own invention gets a named `ToSchema` struct in its module and is serialised from that. See `docs/contributing/adding-a-registry.md` §9.
5. **`crates/core/src/entities/registry_kind.rs`** — add the new variant to the `RegistryKind` enum and its `ALL` slice; `as_str`/`FromStr` and `crates/config/src/schema/mod.rs`'s `validate()` (which just calls `registry_type.parse::<RegistryKind>()`) pick it up automatically. `server/src/builders.rs`'s exhaustive match on `RegistryKind` will then force you to wire up client construction for the new variant.
6. **`server/src/main.rs`** — instantiate the client and wire it into `HotConfig`.
7. **`ui/src/config/registryTypes.ts`** — add a `RegistryTypeDef` entry with setup snippets.
8. **Add a regression test** in the relevant `crates/web/tests/*.rs` file (e.g. `local_npm_registry.rs`, `local_composer_registry.rs`; see "Integration tests" below for how the suite is split) — at minimum a `<name>_publish_traversal_version_returns_400` test that publishes with `version = "../../etc/x"` and asserts `400`. Follow the `nuget_publish_traversal_version_returns_400` / `npm_publish_traversal_version_returns_400` pattern.

For **local/hybrid mode**, additionally implement `get_<name>_versions` (and related helpers) in `crates/core/src/services/local_registry.rs`, following the existing `get_nuget_versions` / `get_maven_versions` patterns.

### Test patterns

- **Unit tests**: `#[cfg(test)] mod tests` inside the same file by default. Registry adapter tests use `mockito::Server` to mock HTTP upstreams.
  - **Exception — standalone `<name>/tests.rs`**: use a sibling `tests.rs` (declared as `#[cfg(test)] mod tests;` in `mod.rs`) when the adapter's test suite exercises behavior that spans more than one sibling file — e.g. the `RegistryClient` impl in `client.rs` plus parsing helpers that also live in `client.rs` or `models.rs` (composer, conda, pypi, rubygems, terraform). A single `impl`'s own tests still belong inline next to that `impl` (forgejo, github, gitlab, maven, nuget, vscode_marketplace).
- **Integration tests** (in-process): `crates/web/tests/*.rs` — one file per feature/registry area (e.g. `local_npm_registry.rs`, `terraform.rs`, `namespaces_and_visibility.rs`, `vuln_proxy_endpoints.rs`), each spinning up a full actix-web app with `InMemoryPackageRepository`, `InMemoryStorageBackend`, `InMemoryCacheStore`, and `FixedRegistry`. Shared app-factory infrastructure (`make_app`, `make_local_svc`, `access_config*`, `LocalRegistryAppParts`/`build_local_registry_app`, etc.) lives in `crates/web/tests/common/mod.rs`; every other file starts with `mod common; use common::*;`. Add a new registry type's tests to a new `local_<type>_registry.rs` file (or an existing one that already covers a closely related area) rather than growing one of the existing files indefinitely. Each registry type has a `make_local_<type>_app(mode: RegistryMode)` factory (in `common/mod.rs` if used by more than one file, otherwise local to its own file) and a helper to build publish payloads.
- **CLI integration tests**: `cli/tests/integration.rs` — builds the CLI binary then invokes it as a subprocess against an in-memory actix-web server (same pattern as the web tests). Uses `env!("CARGO_BIN_EXE_batlehub-cli")` so cargo builds the binary automatically before running. See architecture note below about in-memory store separation.
- **External integration tests**: `crates/adapters/tests/pg_*.rs`, `s3_storage.rs` — require real Postgres/MinIO (run via `task test:pg-*` / `task test:s3`).
- **Fuzz targets**: `fuzz/fuzz_targets/` — run with nightly via `task fuzz`.
  `fuzz/` is a **separate workspace**, so `cargo check/clippy/test --workspace`
  never compiles it: after changing a type a fuzz target constructs, run
  `task fuzz:check` (no nightly needed) or CI's `Fuzz targets` job will.

#### CLI test architecture — in-memory store separation

`InMemoryLocalRegistry` (used by `LocalRegistryService` — publish/yank/delete) and `InMemoryPackageRepository` (used by `AdminService` — package list/block) are **separate** in-memory stores. In Postgres they share the same tables, so this separation only matters in tests.

Consequence: packages published via the local-registry HTTP endpoint do **not** appear in `GET /api/v1/packages` (which queries `AdminService`). Use `TestServer::seed_package()` to inject entries directly into `InMemoryPackageRepository` when testing commands like `package list`. To verify yank/unyank/delete state, query the registry-specific endpoints (e.g. the NuGet flat-index at `/proxy/{reg}/nuget/v3/flat/{id}/index.json`) rather than `package list`.

Coverage is enforced at 80% lines. Excluded paths (DB adapters, some registry clients, auth/OIDC handlers, server main) are listed in the `COVERAGE_EXCLUDE` variable in `.tasks/coverage.yaml`.

### Storage keys

Artifacts are stored with `artifact_storage_key(registry, name, version)` → `"{registry}/{name}/{version}"`. Maven uses `maven_artifact_storage_key` for multi-file artifacts. These keys are stable and shared between `ProxyService` (cache writes) and `LocalRegistryService` (local reads).

### Database migrations

SQL migrations live in `crates/adapters/migrations/`. They are embedded via `crates/adapters/src/migrations.rs` using a `mig!` macro (avoids the `sqlx::migrate!` macro which pulls in `sqlx-mysql` → `rsa` RUSTSEC advisory). When adding a migration, increment the sequence number and add a `mig!` entry to `embedded_migrator()`.

### Security constraints

`sqlx-macros` and `sqlx-mysql` are patched to empty stubs in `[patch.crates-io]` (Cargo.toml) to remove the `rsa` crate (RUSTSEC-2023-0071) from the dependency tree. Do not add `features = ["macros"]` to the `sqlx` dependency or re-enable `sqlx-macros`/`sqlx-mysql`.

`aws-sdk-s3` and `aws-config` use `default-features = false` to avoid `legacy-rustls-ring` (RUSTSEC-2026-0098/0099/0104). Do not enable default features on these crates.

`actix-web` uses `default-features = false` with actix-web 4.14's default feature set **minus `http2`**, because that feature is the only thing pulling `h2 0.3` (RUSTSEC-2026-0258) into the tree — the fix is in h2 0.4.16, `actix-http` still requires the 0.3 line, and there is no 0.3 backport. Nothing is lost: this process never terminates TLS, so its HTTP/2 would only ever be h2c, and real deployments terminate HTTP/2 at the ingress. Do not enable default features on `actix-web` unless the server gains `bind_rustls`/`bind_openssl`, and re-open the advisory with actix if it does.

`lru` is held at `>= 0.18.2` (RUSTSEC-2026-0253 — `LruCache::pop()` is not panic-safe, so a panicking key `Drop` leaves dangling pointers for the next eviction to write through). `aws-sdk-s3` was the only holder of the vulnerable 0.16 line and moved to `^0.18.2` in 1.144.0, so this is a lockfile floor, not a manifest pin. Do not downgrade `aws-sdk-s3` below 1.144.0.

The four invariants above are now also enforced by `cargo-deny`: `rsa`, `sqlx-mysql`, `sqlx-macros-core`, the legacy `rustls 0.21` / `rustls-webpki 0.101` line, `h2 <0.4`, and `lru <0.18.2` are in the `[bans].deny` list in `deny.toml`. If a dependency bump silently drags one back into the tree, `cargo deny check` (and CI) fails.

### Vulnerability scanning

CVE detection runs continuously across every layer; see `docs/contributing/security-scanning.md` for the full matrix and the SBOM re-scan workflow. Reproduce the dependency/SBOM gate locally with `task security` (runs `cargo audit`, `cargo deny`, `pnpm audit` for `ui/` + `docs/`, and the Rust SBOM). Scanner tooling is provisioned by `mise install`.

- **Rust deps**: `cargo audit` (RUSTSEC) + `cargo deny` (advisories/bans/licenses/sources) — `.github/workflows/back-dep-audit.yaml`.
- **JS deps**: `pnpm audit --audit-level high` — `.github/workflows/dep-audit-frontend.yaml`.
- **Dependency supply chain**: [postmortem](https://github.com/mlab-sh/postmortem) (source-repo reputation + vulns from lockfiles) — `.github/workflows/postmortem.yaml`, one job per dependency root (Rust `.`, `ui/`, `docs/`), SARIF to Code Scanning.
- **Container/OS**: Trivy on the built images — the proxy image (`Containerfile`) and the scan-worker image (`Containerfile.worker`, RFC 0018) — blocking on fixable HIGH/CRITICAL — `.github/workflows/image-scan.yaml` (GitHub, daily rebuild+rescan) and `.forgejo/workflows/build.yaml` (both images).
- **SBOM**: CycloneDX for the Rust workspace and the image, attached/attested on release (`.github/workflows/build.yaml`).
- **SAST / secrets / lint**: CodeQL, Semgrep (`semgrep.yaml`), gitleaks (`secret-scan.yaml`, config `gitleaks.toml`), and the clippy/fmt `lint` job in `test.yaml`.

Stance is **no suppressions**: keep `advisories.ignore = []` in `deny.toml` and `.cargo/audit.toml` empty; fix or patch rather than ignore.

### Frontend

**Package manager: pnpm.** Both `ui/` and `docs/` are pnpm projects — `pnpm-lock.yaml` is the only lockfile, there is no `package-lock.json`. The version is pinned by the `packageManager` field in each `package.json`, and provisioned by `pnpm/action-setup` (GitHub/Forgejo CI), an explicit `npm install -g pnpm@<version>` (the `Containerfile`s — Node stopped distributing corepack in v25), or `mise install` (local). Use `pnpm install --frozen-lockfile` in scripts/CI (the `npm ci` equivalent).

**pnpm settings live in `pnpm-workspace.yaml`, not `package.json`.** Since pnpm 10 the `pnpm` field in `package.json` is no longer read: pnpm only *warns* and carries on, so an override left there is silently dropped. Pin a transitive dependency under `overrides:` in `pnpm-workspace.yaml` (that is where the `js-yaml` CVE pin lives). Install-time lifecycle scripts are denied by default; opt a package in under `allowBuilds:` in the same file, with a reason.

The Vue SPA lives in `ui/`. The TypeScript API client (`ui/src/client/`) is auto-generated from `ui/openapi.json` via `pnpm run generate` — do not edit it manually. Setup snippets for the Setup Guide are defined in `ui/src/config/registryTypes.ts` as `REGISTRY_TYPE_DEFS`.

#### Sync SDK

When the backend api is updated, the openapi spec/sdk must be resynced:

1. Generate the swagger spec : `task dump-spec` (copies to `ui/openapi.json`)
2. Generate the Typescript client: `task ui:generate` (reads from `ui/openapi.json`, outputs to `ui/src/client/`)

The generated use fetch and include the full model spec, so any changes to the API will be reflected in the generated client. The client is used in the frontend and can also be imported by external users who want a typed API client for the server.

## Docs

**There is one documentation tree, `docs/`, and it is the published site**
(batleforc.git.batleforc.fr/batlehub). It used to be two — `website/` published
and `docs/` unpublished — with four documents maintained in both and drifted by
up to 296 lines. RFC 0005 merged them. A document has exactly one home, and the
home is chosen by **who reads it**:

| Space | "I am here because…" |
| --- | --- |
| `docs/guide/` | I run this server — install, configure, administer |
| `docs/use/` | my package manager talks to it — tokens, publishing, the CLI |
| `docs/registries/` | I need the snippet for *my* package manager |
| `docs/operations/` | something is broken, or an auditor is asking |
| `docs/contributing/` | I am changing the code |
| `docs/rfc/` | I want to know why it works this way |
| `docs/internal/` | not published — generated artifacts, dated security findings, the RFC template (`srcExclude`d) |

A page belongs to exactly one space and appears in exactly one sidebar
(`task docs:audience`), a page over 4 000 words declares `reference: true`
(`task docs:structure`), and an instruction has one home — the others link to it.
Publishing lives on the registry page, not in a per-ecosystem walkthrough.

Two files in the tree are **generated and must not be hand-edited**:
`docs/.vitepress/theme/tokens.css` (`task ui:tokens`, from
`ui/src/design/tokens.css`) and `docs/guide/roadmap.md` (`task docs:roadmap`,
from `ROADMAP.md`). Both have a drift check that fails the build.

### RFCs (`docs/rfc/`)

Every fact a listing quotes about an RFC lives in that RFC's own header table —
`Status`, the `Short` name it is listed under, and the one line it `Settles`.
The status banner on the page, the **table between the `rfc-index` markers in
`docs/rfc/index.md`**, and the **`/rfc/` sidebar between the `rfc-sidebar`
markers in `docs/.vitepress/config.ts`** are all generated from those rows; edit
the RFC, not the listings.

```bash
task rfc:new TITLE="…" SETTLES="…" [SHORT=…] [SLUG=…] [BIS=NNNN]  # next free number, from the template
task rfc:index         # regenerate the /rfc/ table and sidebar
task rfc:index:check   # drift gate, part of task docs:design
task rfc:status        # status, open-question count and readiness per RFC
```

Implementation: `docs/build/rfc.mjs`, parsing in `docs/build/rfc-meta.mjs`
(imported by `docs/.vitepress/config.ts`, so the status vocabulary is defined
once). The template is `docs/internal/0000-rfc-template.md` and is not published.

- **Roadmap** (ROADMAP.md at the repo root) — canonical; the published page is generated from it.
- **Code comments** - doc directly in the codebase, especially for complex logic like the request lifecycle, hot reload, and registry client implementations. Use `///` for public items and `//` for internal comments.

<!-- rtk-instructions v2 -->
# RTK (Rust Token Killer) - Token-Optimized Commands

## Golden Rule

**Always prefix commands with `rtk`**. If RTK has a dedicated filter, it uses it. If not, it passes through unchanged. This means RTK is always safe to use.

**Important**: Even in command chains with `&&`, use `rtk`:
```bash
# ❌ Wrong
git add . && git commit -m "msg" && git push

# ✅ Correct
rtk git add . && rtk git commit -m "msg" && rtk git push
```

## RTK Commands by Workflow

### Build & Compile (80-90% savings)
```bash
rtk cargo build         # Cargo build output
rtk cargo check         # Cargo check output
rtk cargo clippy        # Clippy warnings grouped by file (80%)
rtk tsc                 # TypeScript errors grouped by file/code (83%)
rtk lint                # ESLint/Biome violations grouped (84%)
rtk prettier --check    # Files needing format only (70%)
rtk next build          # Next.js build with route metrics (87%)
```

### Test (60-99% savings)
```bash
rtk cargo test          # Cargo test failures only (90%)
rtk go test             # Go test failures only (90%)
rtk jest                # Jest failures only (99.5%)
rtk vitest              # Vitest failures only (99.5%)
rtk playwright test     # Playwright failures only (94%)
rtk pytest              # Python test failures only (90%)
rtk rake test           # Ruby test failures only (90%)
rtk rspec               # RSpec test failures only (60%)
rtk test <cmd>          # Generic test wrapper - failures only
```

### Git (59-80% savings)
```bash
rtk git status          # Compact status
rtk git log             # Compact log (works with all git flags)
rtk git diff            # Compact diff (80%)
rtk git show            # Compact show (80%)
rtk git add             # Ultra-compact confirmations (59%)
rtk git commit          # Ultra-compact confirmations (59%)
rtk git push            # Ultra-compact confirmations
rtk git pull            # Ultra-compact confirmations
rtk git branch          # Compact branch list
rtk git fetch           # Compact fetch
rtk git stash           # Compact stash
rtk git worktree        # Compact worktree
```

Note: Git passthrough works for ALL subcommands, even those not explicitly listed.

### GitHub (26-87% savings)
```bash
rtk gh pr view <num>    # Compact PR view (87%)
rtk gh pr checks        # Compact PR checks (79%)
rtk gh run list         # Compact workflow runs (82%)
rtk gh issue list       # Compact issue list (80%)
rtk gh api              # Compact API responses (26%)
```

### JavaScript/TypeScript Tooling (70-90% savings)
```bash
rtk pnpm list           # Compact dependency tree (70%)
rtk pnpm outdated       # Compact outdated packages (80%)
rtk pnpm install        # Compact install output (90%)
rtk npm run <script>    # Compact npm script output
rtk npx <cmd>           # Compact npx command output
rtk prisma              # Prisma without ASCII art (88%)
```

### Files & Search (60-75% savings)
```bash
rtk ls <path>           # Tree format, compact (65%)
rtk read <file>         # Code reading with filtering (60%)
rtk grep <pattern>      # Search grouped by file (75%). Format flags (-c, -l, -L, -o, -Z) run raw.
rtk find <pattern>      # Find grouped by directory (70%)
```

### Analysis & Debug (70-90% savings)
```bash
rtk err <cmd>           # Filter errors only from any command
rtk log <file>          # Deduplicated logs with counts
rtk json <file>         # JSON structure without values
rtk deps                # Dependency overview
rtk env                 # Environment variables compact
rtk summary <cmd>       # Smart summary of command output
rtk diff                # Ultra-compact diffs
```

### Infrastructure (85% savings)
```bash
rtk docker ps           # Compact container list
rtk docker images       # Compact image list
rtk docker logs <c>     # Deduplicated logs
rtk kubectl get         # Compact resource list
rtk kubectl logs        # Deduplicated pod logs
```

### Network (65-70% savings)
```bash
rtk curl <url>          # Compact HTTP responses (70%)
rtk wget <url>          # Compact download output (65%)
```

### Meta Commands
```bash
rtk gain                # View token savings statistics
rtk gain --history      # View command history with savings
rtk discover            # Analyze Claude Code sessions for missed RTK usage
rtk proxy <cmd>         # Run command without filtering (for debugging)
rtk init                # Add RTK instructions to CLAUDE.md
rtk init --global       # Add RTK to ~/.claude/CLAUDE.md
```

## Token Savings Overview

| Category | Commands | Typical Savings |
|----------|----------|-----------------|
| Tests | vitest, playwright, cargo test | 90-99% |
| Build | next, tsc, lint, prettier | 70-87% |
| Git | status, log, diff, add, commit | 59-80% |
| GitHub | gh pr, gh run, gh issue | 26-87% |
| Package Managers | pnpm, npm, npx | 70-90% |
| Files | ls, read, grep, find | 60-75% |
| Infrastructure | docker, kubectl | 85% |
| Network | curl, wget | 65-70% |

Overall average: **60-90% token reduction** on common development operations.
<!-- /rtk-instructions -->