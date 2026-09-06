---
# The contributor's reference: it sat just under the line until the workspace
# grew a second sidecar (§11), and the material it covers — crate layout, test
# suites, design gates, the dev-time identity provider — is a subject that is
# long rather than a page that has sprawled. `docs:structure` asks for this
# declaration above 4 000 words (RFC 0005-bis §4.5).
reference: true
---

# Contributing to BatleHub

This guide is the starting point for developers working on the BatleHub codebase. It covers the project layout, key architectural patterns, how to run the tests, and known design limitations you need to be aware of before touching specific areas.

## 1. Prerequisites

| Tool | Minimum version | Notes |
|------|----------------|-------|
| Rust toolchain | stable (see `rust-toolchain.toml`) | `rustup` is the recommended installer |
| PostgreSQL | 14 | Integration tests expect `DATABASE_URL` in the environment |
| Node.js + pnpm | Node 24 / pnpm 11 (via `mise install`) | UI (`ui/`) and docs site (`website/`) — not needed for Rust-only work |

```bash
# Clone and build
git clone https://git.batleforc.fr/batleforc/batlehub
cd batlehub
cargo build
```

### Disk, and what the dev profile gives up for it

A full `cargo build --workspace --all-targets` used to leave a **46 GB**
`target/`. The cause is structural rather than accidental: 102 test executables,
each statically linking the whole dependency graph, each carrying its own copy of
the DWARF for it — 81 % of every binary was `.debug_*`.

The root `Cargo.toml` sizes that down to about **15 GB** with two settings, and
they are a trade you should know about before reaching for a debugger:

| Setting | Effect |
| --- | --- |
| `[profile.dev] debug = "line-tables-only"` | our crates keep the file:line a panic backtrace prints, and lose the variable/type information a debugger would show |
| `[profile.dev.package."*"] debug = false` | dependencies keep none at all; symbol names still come from the symbol table, so a backtrace frame in `tokio` still names the function |

`RUST_BACKTRACE=1` is unaffected in the way that matters — every frame still
reports `file.rs:line:col`. If you need to step through code in lldb, delete both
blocks for the session; a rebuild is the whole cost and nothing else in the repo
depends on them.

`target/debug/incremental` (another few GB) is pure cache and can be deleted at
any time.

### Keeping it from growing back

The profile settings fix the size of each artefact; they do nothing about how
many there are. **Cargo never removes anything from `target/`**: when a metadata
hash changes — a dependency bump, a feature flip, a new toolchain — the new
artefact is written *beside* the old one, and the old one stays forever.

Measured on this workspace, which is the useful part: editing a source file and
rebuilding the same way adds **nothing** to `target/debug/deps` (the artefact is
overwritten in place; only the incremental cache grows, ~60 MB a cycle). One
change of *build shape* added 152 files and 167 MB that nothing would ever
reclaim. A dependency bump invalidates the 102 test binaries at once, so it
leaves a whole generation — on the order of 10 GB — behind.

```bash
task clean:stale     # cargo sweep --time 1
```

Everything today's work has touched is kept; everything left over from
yesterday's is not. It is safe by construction — every artefact is reproducible
from source — and the only cost of sweeping something still wanted is rebuilding
it. Expect a partial rebuild on the next `cargo build` even when the sweep
reports a small number: removing one `.rmeta` invalidates its dependents.

Run it after a dependency bump, which is the moment the biggest stale generation
appears, or on a schedule.

Le repo Git est disponible dans deux provider Git:

- https://git.batleforc.fr/batleforc/batlehub : Instance SelfHosted de Forgejo
- https://github.com/batlehub/batlehub : Miroir GitHub (Principalement en lecture seule, les contributions se font via des pull requests sur la Forgejo)

---

## 2. Workspace layout

```
batlehub/
├── crates/
│   ├── config/        Config schema (TOML → typed structs) and validation
│   ├── core/          Domain types, port traits, pure business logic — no I/O
│   ├── adapters/      Concrete I/O implementations: Postgres, S3, HTTP clients
│   ├── web/           actix-web handlers, middleware, OpenAPI wiring
│   └── examples/      Integration test helpers (smoke, local_registry, real_proxy tests)
├── server/            Binary entry point: wires everything together
├── cli/               batlehub-cli binary: clap commands, reqwest API client, ratatui TUI
│   └── tests/         CLI integration tests — subprocess binary against in-memory server
├── docs/              Guides (you are here)
├── ui/                Vue 3 front-end
└── patches/           sqlx-macros stub (see sqlx note in Cargo.toml)
```

### Dependency direction

```
config  ──►  core  ──►  adapters  ──►  web  ──►  server
```

`core` has no I/O dependencies. `adapters` implements `core`'s port traits.
`web` depends on `core` types but calls into adapters only through traits —
never by name. `server` is the only crate that knows both sides and wires them.

---

## 3. Architecture: ports and adapters

BatleHub uses the *hexagonal* (ports-and-adapters) pattern. Every external
dependency is hidden behind a trait defined in `crates/core/src/ports/`.

| Trait | Description | Primary implementation |
|-------|-------------|----------------------|
| `RegistryClient` | Upstream registry HTTP protocol | `crates/adapters/src/registry/*.rs` |
| `StorageBackend` | Artifact blob store (read/write/delete) | `crates/adapters/src/storage/` |
| `LocalRegistryBackend` | Index for privately published packages | `crates/adapters/src/local_registry/postgres.rs` |
| `PackageRepository` | Audit log and proxy metadata (Postgres) | `crates/adapters/src/db/postgres.rs` |
| `QuotaRepository` | Publish quota tracking | `crates/adapters/src/db/quota.rs` |
| `ArtifactMetaRepository` | Cache TTL / access-time tracking | `crates/adapters/src/db/artifact_meta.rs` |
| `CacheStore` | Metadata cache (memory / Postgres / Redis) | `crates/adapters/src/cache/` |
| `AuthProvider` | Token / OIDC / Kubernetes / Actions-OIDC validation | `crates/adapters/src/auth/` |

**Rule**: `crates/core` must never import from `crates/adapters` or `crates/web`.
Tests inside `core` use in-memory mocks, not the Postgres implementations.

---

## 4. Request lifecycle

```
HTTP request
  │
  ▼
actix-web middleware  (AuthMiddlewareFactory → AuthIdentity extractor)
  │
  ▼
Handler  (crates/web/src/handlers/proxy/<registry>.rs)
  │  extracts: AuthIdentity, RegistryMap, web::Data<Arc<ProxyService>>
  ▼
ProxyService::proxy(PackageId)
  │  checks: RegistryPolicy (RBAC rules, firewall_only, release-age-gate, …)
  │  checks: cache (CacheStore + StorageBackend)
  ▼
RegistryClient::fetch_artifact(PackageId)   ← upstream HTTP call
  │
  ▼
StorageBackend::store()                     ← persist to filesystem / S3
  │
  ▼
HTTP response streamed back to client
```

For **local/hybrid registries**, the publish path goes through
`LocalRegistryService::publish()` → `LocalRegistryBackend::publish()` →
`StorageBackend::store()`. Quota is checked and recorded between the two.

### PackageId conventions

`PackageId { registry, name, version, artifact }` is the cache key and data
carrier between the web layer and adapters. Conventions vary by ecosystem —
see `docs/contributing/adding-a-registry.md` for the full mapping table.

---

## 5. Database and migrations

Migrations live in `crates/adapters/migrations/` as numbered SQL files and are
registered in `crates/adapters/src/migrations.rs` using the `mig!()` macro.
They run automatically on startup via `sqlx::Migrate`.

**Important**: `sqlx::query!()` macros are disabled. The project patches
`sqlx-macros` with a no-op stub to avoid pulling in `sqlx-mysql` which carries
RUSTSEC-2023-0071 (an unfixed RSA vulnerability). All database queries use the
runtime API instead:

```rust
// Correct
sqlx::query("SELECT ... WHERE id = $1")
    .bind(id)
    .fetch_one(&pool)
    .await?;

// Will not compile — do not use
sqlx::query!("SELECT ...", id).fetch_one(&pool).await?;
```

When adding a new migration:
1. Create `crates/adapters/migrations/00N_description.sql`.
2. Add `mig!(N, "description", "../migrations/00N_description.sql")` to
   `crates/adapters/src/migrations.rs` (keep them in order).

---

## 6. Adding a new feature — checklist

### New registry type (proxy-only)

See `docs/contributing/adding-a-registry.md` for the full step-by-step walkthrough.
Short version:

- [ ] `crates/adapters/src/registry/<name>.rs` — implement `RegistryClient`
- [ ] `crates/adapters/Cargo.toml` — add `registry-<name> = []` feature, include in `default`
- [ ] `crates/adapters/src/registry/mod.rs` — export under `#[cfg(feature = "registry-<name>")]`
- [ ] `crates/config/src/schema.rs` — add `"<name>"` to the `validate()` match arm
- [ ] `server/src/main.rs` — add arm to `build_registry_client()`
- [ ] `crates/web/src/handlers/proxy/<name>.rs` — HTTP handler(s)
- [ ] `crates/web/src/lib.rs` — register routes in `collect_routes()`

### New DB-backed feature

- [ ] `crates/adapters/migrations/00N_<feature>.sql` — migration
- [ ] `crates/adapters/src/migrations.rs` — register it
- [ ] `crates/core/src/ports/<feature>.rs` — port trait
- [ ] `crates/core/src/ports/mod.rs` — re-export
- [ ] `crates/adapters/src/db/<feature>.rs` — Postgres implementation
- [ ] `crates/adapters/src/db/mod.rs` — export
- [ ] Wire the repository into `server/src/main.rs`

### New admin API endpoint

- [ ] `crates/web/src/handlers/back_office/<feature>.rs`
- [ ] Call `require_admin(&identity)?` at the start of every handler
- [ ] Register routes in `collect_routes()` — most-specific paths first
  (actix-web matches in registration order; `DELETE /quota/{reg}/{user}` must
  appear **before** `DELETE /quota/{reg}`)
- [ ] Add `pub mod <feature>;` to `crates/web/src/handlers/back_office/mod.rs`

---

## 7. Running tests

BatleHub has four layers of tests, each trading breadth for depth. Run them in order when you want full confidence; run just the first two for fast feedback.

### Layer 0 — unit tests (no external dependencies)

```bash
cargo test                      # all crates
cargo test -p batlehub-core     # domain logic only
cargo test -p batlehub-adapters # I/O adapter implementations
```

Every module keeps its unit tests at the bottom of the same source file under `#[cfg(test)]`. These tests use in-process mocks and stubs only — no database, no HTTP server, no network.

**What they validate:** Pure business logic — publish rules, quota checks, RBAC decisions, cache-key formatting, wire-format parsing, JWT claim evaluation. They run in milliseconds and must always pass on any developer machine.

#### Auth provider unit tests

Auth providers expose a `for_testing` constructor that injects a pre-loaded JWKS so tests never hit the network. Each module's `#[cfg(test)]` block covers:

- Token parsing and claim extraction
- Role elevation and group assignment via rule evaluation
- Condition matching (glob and regex patterns, auto-detection)
- Group template rendering
- Error paths: expired tokens, unknown signing keys, malformed headers

The `actions-oidc` provider additionally exposes `for_testing_stale`, which backdates the JWKS cache by more than `JWKS_MIN_REFRESH` so the cache-refresh path can be exercised without sleeping.

---

### Layer 0.5 — adapter integration tests (mockito HTTP, no external services)

```bash
cargo test -p batlehub-adapters --test actions_oidc
cargo test -p batlehub-adapters --test selfhosted
```

`crates/adapters/tests/` contains integration tests for adapters that need an HTTP server but no database or object storage. Tests use **mockito** (`mockito::Server::new_async()`) to spin up in-process HTTP servers.

**What they validate:** The full bootstrap and request cycle for network-facing adapters — OIDC discovery fetch, JWKS retrieval, provider construction failure paths, and end-to-end JWT authentication. Each test file covers one adapter family:

| File | What it covers |
|------|---------------|
| `actions_oidc.rs` | GitHub/Forgejo Actions OIDC: discovery → JWKS → JWT auth round-trip, error paths (5xx, malformed JSON), TOML config round-trip |
| `selfhosted.rs` | Self-hosted registry HTTP options: bearer forwarding, basic auth, TLS |

No external services are needed. These tests run offline and are included in `task coverage` automatically.

---

### Layer 1 — web integration tests (in-memory backends, no PostgreSQL)

```bash
cargo test -p batlehub-web
```

`crates/web/tests/*.rs` — one file per feature/registry area, sharing app-factory infrastructure from `crates/web/tests/common/mod.rs` — spin up a real actix-web application using `actix_web::test::init_service` and in-memory backends (no Postgres, no S3). They send actual HTTP requests and assert on status codes, headers, and JSON bodies.

**What they validate:**
- All proxy handlers across every registry type — correct URL routing, wire-format parsing, upstream passthrough, cache behaviour, and error mapping.
- Auth middleware — anonymous fallback, bearer token resolution, role mapping.
- Rate-limit middleware — per-user buckets, per-group buckets, warn/block modes, 429 responses.
- Back-office admin API — package listing, block/unblock, audit log, quota management, yank/unyank.
- Local registry publish/pull cycle — three-phase commit, quota enforcement, ownership checks.

These tests cover the largest surface area and run without any external services. They are the primary regression net for handler changes.

---

### Layer 1.5 — CLI integration tests (no external dependencies)

```bash
cargo test -p batlehub-cli --test integration
```

`cli/tests/integration.rs` compiles `batlehub-cli` as a real binary and invokes it as a subprocess against a genuine in-memory batlehub server started in the same process. The binary path is resolved at compile-time via `env!("CARGO_BIN_EXE_batlehub-cli")`, so cargo automatically rebuilds the binary before running the tests.

**What they validate:**
- All CLI commands round-trip correctly through the HTTP API: `registry list/info`, `auth whoami`, `package list/versions`, `version yank/unyank/delete`, `publish`.
- JSON output (`--json`) is valid and contains the expected fields — asserted with `serde_json::Value` rather than string matching.
- Auth: authenticated (admin token) vs. anonymous identity.
- Error paths: unknown registry returns non-zero exit with a "not found" message in stderr.

#### Important: in-memory store separation

`InMemoryLocalRegistry` (backing `LocalRegistryService`, used for publish/yank/delete) and `InMemoryPackageRepository` (backing `AdminService`, used for `package list`) are **two separate in-memory stores**. In PostgreSQL they share the same tables, so this only matters in tests.

As a result:
- Packages published via the NuGet local endpoint do **not** appear in `package list` (which queries `AdminService`). The tests use `TestServer::seed_package()` to inject records directly into `InMemoryPackageRepository` when they need a package to appear in the admin list.
- Yank/unyank/delete state is verified via the NuGet flat-index endpoint (`GET /proxy/{reg}/nuget/v3/flat/{id}/index.json`), which queries `LocalRegistryService` — the same store the operations write to.

---

### Layer 2 — example structure tests (no network)

```bash
cargo test -p batlehub-examples --test structure
```

`crates/examples/tests/structure.rs` is a single static-analysis test (`all_examples_are_complete`) that iterates over all 12 example directories and asserts:
- Required files are present (`mise.toml`, `README.md`, a start script, a config file).
- TOML and JSON files parse without error.
- Config files reference the expected proxy URL placeholder.
- Shell scripts carry a proper shebang line.

**What they validate:** That every shipped example is complete and well-formed before anyone runs it. Catches copy-paste omissions and accidental deletions immediately, without touching the network or running any package manager.

---

### Layer 3 — local registry upload/pull cycle (no network, curl only)

```bash
cargo test -p batlehub-examples --test local_registry
```

`crates/examples/tests/local_registry.rs` starts a genuine actix-web batlehub proxy in `RegistryMode::Local` with fully in-memory backends (no PostgreSQL, no upstream registries) and runs an end-to-end publish → download cycle for every publish-capable registry type.

| Test | Publish endpoint | Download check |
|------|-----------------|---------------|
| `local_npm_publish_pull` | `PUT /proxy/{reg}/{name}` | packument + tarball |
| `local_cargo_publish_pull` | `PUT /proxy/{reg}/api/v1/crates/new` | `.crate` download |
| `local_go_publish_pull` | `PUT /proxy/{reg}/{module}@v/{ver}.zip` | list, `.mod`, `.zip` |
| `local_rubygems_publish_pull` | `POST /proxy/{reg}/api/v1/gems` | `.gem` download |
| `local_composer_publish_pull` | `POST /proxy/{reg}/api/upload` | p2 metadata + dist |
| `local_maven_publish_pull` | `PUT /proxy/{reg}/maven2/{path}` | artifact download |
| `local_openvsx_publish_pull` | `PUT /proxy/{reg}/{pub}.{name}/{ver}/vsix` | vsix download |
| `local_terraform_module_publish_pull` | `POST /proxy/{reg}/v1/modules/{ns}/{name}/{prov}/{ver}` | versions + artifact |

**What they validate:** That the full publish → store → serve pipeline works end-to-end for each ecosystem's wire format (binary framing for cargo, TAR+gzip for rubygems, ZIP for composer and goproxy, etc.) without any network dependency or package-manager tooling. These tests are the first line of defence when touching `LocalRegistryService`, storage backends, or registry-specific handlers.

---

### Layer 4 — smoke tests against example apps (requires mise + language runtimes)

```bash
cargo test -p batlehub-examples --test smoke
```

`crates/examples/tests/smoke.rs` copies each example into a temp directory, runs `mise install` to pull language runtimes, starts the example application, and curls it. Tests that hit the network skip gracefully when the required tool is not available.

**What they validate:**

| Group | Tests | What is verified |
|-------|-------|-----------------|
| MockProxy routing | `proxy_curl_endpoints` | curl hits a hand-rolled TCP proxy; `X-Served-By: mock-proxy` header is returned |
| Downstream tool routing | `vsix_downloads_via_proxy`, `github_asset_download_via_proxy` | curl downloads pass through the mock proxy log |
| Real app startup | `api_npm`, `api_go`, `api_python`, `api_ruby`, `api_composer_console`, `api_maven_spring`, `api_maven_quarkus` | example app starts, HTTP `/` returns "hello" |
| mise proxy routing | `mise_install_tasks_route_through_proxy` | package-manager requests are logged in the mock proxy |

These are the highest-cost tests and are intended for CI environments with full language tooling. They confirm that the shipped examples actually work.

---

### Layer 5 — real proxy against live upstreams (requires network + language runtimes)

```bash
cargo test -p batlehub-examples --test real_proxy
```

`crates/examples/tests/real_proxy.rs` starts a genuine batlehub actix-web proxy with in-memory backends and **real upstream registry HTTP clients**, then uses actual package-manager tools to fetch packages through it.

| Test | Tool / ecosystem | What is verified |
|------|-----------------|-----------------|
| `real_proxy_npm_api` | Node / npm | npm example installs deps via proxy, app starts, GET `/` → "hello" |
| `real_proxy_cargo_fetch` | cargo | `cargo fetch` resolves a crate through the proxy |
| `real_proxy_go_api` | Go | Go example builds + runs with `GOPROXY` pointing at the proxy |
| `real_proxy_pypi_api` | Python / pip | Python example installs via proxy, app starts |
| `real_proxy_rubygems_api` | Ruby / bundler | Ruby example installs via proxy, app starts |
| `real_proxy_composer_console` | PHP / composer | composer install routes through proxy |
| `real_proxy_maven_spring_api` | Java / Maven | Spring Boot example builds via proxy, starts, GET `/` → "hello" |
| `real_proxy_maven_quarkus_api` | Java / Maven | Quarkus example builds via proxy, starts, GET `/` → "hello" |
| `real_proxy_terraform_init` | Terraform | `terraform init` downloads provider through proxy |
| `real_proxy_github_releases` | GitHub Releases | asset download resolves through proxy |
| `real_proxy_openvsx_download` | Open VSX | extension download resolves through proxy |
| `real_proxy_vscode_marketplace_download` | VS Code Marketplace | extension download resolves through proxy |

**What they validate:** True end-to-end correctness of each `RegistryClient` implementation against the live upstream protocol — caching headers, redirect handling, tarball streaming, checksum verification. These are network-dependent and will skip or fail gracefully when the required toolchain or network is unavailable.

---

### Coverage

The project enforces a minimum of **80% line coverage** measured by `cargo-llvm-cov`. Both tasks require PostgreSQL and MinIO (started automatically from the `Taskfile`):

```bash
# Generate an HTML report (opens at target/llvm-cov/html/index.html) and an
# lcov.info at the repo root for SonarQube/SonarLint (sonar.rust.lcov.reportPaths)
task coverage

# Enforce the 80% threshold — fails the build if coverage drops below it
task coverage-check
```

To run coverage manually without the Task runner:

```bash
# Install the tool once
cargo install cargo-llvm-cov

# Base workspace coverage (unit tests)
cargo llvm-cov --no-report --workspace

# Add each integration test that needs separate invocation
cargo llvm-cov --no-report -p batlehub-adapters --test actions_oidc
cargo llvm-cov --no-report -p batlehub-adapters --test pg_cache     # needs DATABASE_URL
cargo llvm-cov --no-report -p batlehub-adapters --test local_registry  # needs DATABASE_URL
cargo llvm-cov --no-report -p batlehub-adapters --test storage_router  # needs DATABASE_URL
cargo llvm-cov --no-report -p batlehub-adapters --features storage-s3 --test s3_storage  # needs S3

# Generate the report
cargo llvm-cov report --html --output-dir coverage/html
```

The workspace-level `[workspace.metadata.llvm-cov]` config excludes `server/src/main.rs` (startup wiring only) from the report. Every other module is expected to have at least some exercised lines.

**Adding a new integration test to coverage**: integration tests that need no external service (mockito-only, like `actions_oidc`) must be listed explicitly in the `cov:collect` task in `.tasks/coverage.yaml` (which both `coverage` and `coverage-check` run) — `cargo llvm-cov --workspace` does not pick up `crates/adapters/tests/*.rs` files automatically.

---

### Security audits

Run dependency vulnerability scans before shipping or merging security-sensitive changes:

```bash
# Rust — checks crates against the RustSec advisory database
task audit

# Frontend — checks npm packages against the npm advisory database
task ui:audit
```

`task audit` suppresses advisories that have no actionable fix via `.cargo/audit.toml`. Add a new entry there (with a justification comment) when an advisory is known and accepted rather than silencing the whole tool. `task ui:audit` exits non-zero when high-severity vulnerabilities are found; use `pnpm audit --audit-level high` manually if you need to ignore lower-severity findings during development.

---

## 8. Code conventions

- **No wildcard imports** — write every imported name explicitly (`use foo::{Bar, Baz}`, not `use foo::*`). Wildcard imports hide where names come from, make unused-import warnings silent, and cause surprise breakage when an upstream crate adds a new symbol that clashes with a local one. The only accepted exception is `#[cfg(test)] use super::*` inside a same-file test module.
- **No `sqlx::query!()` macros** — use the runtime API (see §5).
- **No comments that describe what the code does** — only add one when the
  *why* is non-obvious (a hidden constraint, a workaround, a subtle invariant).
- **Error type per layer**: `CoreError` in `core`, `AppError` in `web`.
  Map at the boundary: `AppError::from(CoreError)` in `crates/web/src/error.rs`.
- **HTTP status for infrastructure errors**: storage and DB errors map to
  `503 Service Unavailable` so load-balancers can retry on another instance.
  Logic errors (not-found, conflict) map to the appropriate 4xx.
- **Quota rollback is best-effort** (`tokio::spawn`). Errors are logged via
  `tracing::error!` but do not propagate to the caller. See §9 for the
  accepted race condition.
- **Route registration order matters**. In `collect_routes()`, register more
  specific paths (longer or with more literal segments) before catch-alls.

---

## 9. Known limitations and accepted trade-offs

### Quota enforcement has a TOCTOU race (accepted)

`QuotaService::check_and_record_publish()` reads the current usage with
`repo.get_usage()` and writes the new total with `repo.record_publish()` in
two separate SQL statements. There is no `SELECT FOR UPDATE` or advisory lock.

**Consequence**: two concurrent publish requests from the same user can both
read the same stale counter, both pass the limit check, and both record —
ending up one package (or one upload's worth of bytes) over the configured
hard limit. The overshoot is bounded to the number of in-flight concurrent
publishes from the same user, which is typically one or two in practice.

**Why it is accepted**: adding database-level serialization (a `SELECT FOR
UPDATE` on the `quota_usage` row) would require restructuring
`PgQuotaRepository` and introducing explicit transactions across the check and
record steps, adding latency to every publish. The quota feature is intended as
a safeguard against accidental runaway usage, not a strict financial billing
boundary — a transient overshoot of one version is acceptable.

**If you need strict enforcement**: wrap `get_usage` and `record_publish` in a
single `BEGIN … SELECT … FOR UPDATE … UPDATE … COMMIT` transaction inside
`PgQuotaRepository`, and update `QuotaRepository::check_and_record_publish`
(or introduce a new method) to execute both steps atomically.

---

### In-memory adapters have separate package stores

`InMemoryLocalRegistry` and `InMemoryPackageRepository` are independent in-memory stores. In production (PostgreSQL), `PostgresLocalRegistry` and `PgPackageRepository` share the same database pool, so packages published via the local registry automatically appear in `GET /api/v1/packages`.

**Consequence for tests**: if you add a test that publishes via a local-registry HTTP endpoint and then queries `GET /api/v1/packages`, it will see an empty list. Seed the `InMemoryPackageRepository` explicitly via `record_access` if you need the package to appear in admin queries.

Additionally, `bulk_yank`, `bulk_unyank`, and `bulk_delete` in `crates/web/src/handlers/back_office/bulk.rs` normalize package names to lowercase before passing them to the backend. This matches the NuGet publish handler, which lowercases the package ID on store. Any test or script that sends mixed-case names to the bulk API must use the normalized (lowercase) form.

---

### `LocalRegistryBackend` uses a two-phase publish

`LocalRegistryService::publish()` uses a three-step protocol:

1. `backend.publish()` — inserts the row with `status = 'pending'` (invisible to readers).
2. `storage.store()` — persists the artifact bytes.
3. `backend.commit_publish()` — promotes the row to `status = 'published'`.

In-process errors at any step trigger a best-effort cleanup (`remove_version`,
`storage.delete`, quota rollback). A hard crash between steps 1 and 2 leaves an
orphaned *pending* row; a crash between steps 2 and 3 leaves a pending row plus
the artifact in storage.

Pending rows are safe: they are invisible to `get_versions` and `exists`, so
they do not cause 404s. They are cleaned up automatically by calling
`LocalRegistryBackend::cleanup_pending(older_than)`, which deletes pending rows
older than the given duration. Wire this up to a startup sweep or a
periodic maintenance task; a threshold of one hour is a safe default.

To recover manually: call `cleanup_pending` or run:

```sql
DELETE FROM local_packages WHERE status = 'pending';
```

---

## 10. Frontend design workflow (Impeccable)

The console's design work runs through [Impeccable](https://impeccable.style), a design skill for AI
coding agents. See `docs/rfc/0003-ui-rework.md` for what it is being used for.

**The skill is not in the tree.** It is ~150 vendored files that turn over on every upstream
release, so `.claude/skills/impeccable/` and `.impeccable/` are gitignored and installed on demand:

```bash
task impeccable:install   # npx impeccable install --providers=claude --scope=project
task impeccable:update    # refresh an existing install
task impeccable:detect    # deterministic anti-pattern scan (no LLM, no API key)
```

Reload your AI harness after installing or updating. `PRODUCT.md` (product truth) and `DESIGN.md`
(the visual system) hold the durable decisions and **are** tracked — they are what the detector and
future design work read.

### Live mode and the CSP

Impeccable's `live` mode serves a helper script from `http://localhost:<port>/live.js` so UI
elements can be iterated on in the browser. The SPA's `<meta http-equiv="Content-Security-Policy">`
(built by `buildCsp()` in `ui/build/csp.ts`) refuses that under `script-src 'self'`.

The relaxation is **dev-only and opt-in**. Set the port live mode reports, then start the dev
server:

```bash
VITE_IMPECCABLE_LIVE_PORT=4849 task ui:dev
```

`resolveLivePort()` requires both a non-production build *and* that variable, and `buildCsp()` takes
a port *number* — the widest policy the opt-in can express is one localhost origin on
`script-src`/`connect-src` plus `blob:` on `img-src`. A production build ignores the variable
entirely and emits a policy byte-identical to the one it emitted before live mode existed; both
halves are pinned by `ui/build/csp.test.ts`.

### Rendered design gates and the browser sidecar

Three of the four design gates are static and run anywhere (`task ui:design`).
The fourth needs a real browser, because contrast on painted pixels, focus-ring
visibility and reflow at 390 px cannot be measured by reading source.

`ui:design:routes` is that fourth gate, and it covers **every rendered route** —
the 15 admin pages and 4 account pages behind router guards *and* the seven
public ones, at four viewports, with axe plus the type ramp and the display
face. It
used to be two gates: `impeccable detect` + `@axe-core/cli` over the public
routes, and a separate authenticated harness. Neither URL-based scanner knows
what a type ramp is, so the ramp assertions ran on `/admin/*`, `/me/*` and `/`
and nothing else — and `/packages`, the one page with a checked-in specification
of its own appearance (`ui/design-proof/index.html`), was the significant page
no ramp check ran against. That is how its 104px display element became 24px
with every gate green (RFC 0004-bis §4.4).

The script's `EXPECTED_FAIL` is **empty**, and that is a result: `/packages` sat
in it until RFC 0004-bis O3 settled by moving the page. A pinned route that
starts *passing* fails the gate, so expect to unpin what you pin.

The viewports are 1440, 1024, 768 and 390. The middle two catch what the outer
pair cannot: a table whose fixed columns starve its flexible one is green at 390
(every fixed width released) and at 1440 (room for all).

The workspace `devfile.yaml` declares a **`browser` sidecar**
([che-browser](https://github.com/batleforc/WeeboDevImage/tree/main/che-browser)):
headed Chrome behind Xvfb, in a sidecar container of the same pod. Every
container in the pod shares one network namespace, so CDP (`9222`) and
chromedriver (`9515`) are plain `localhost` ports from the tools container, and
Chrome reaches the dev server on `localhost:5173` with no ingress.

```bash
task browser:check                  # is it up? prints the CDP version
task ui:dev:local                   # in another terminal (5174, the in-pod front)
task ui:design:rendered             # detector at 2 viewports + the route gate
BATLEHUB_ADMIN_TOKEN=… BATLEHUB_USER_TOKEN=… task ui:design:routes
task browser:open URL=http://localhost:5173/   # open a tab
task browser:tabs                   # list tabs (id, type, title, url)
task browser:close ID=<id>          # close one
task browser:vnc                    # noVNC password, to watch Chrome
```

Chrome itself is gated behind a flag file the sidecar's respawn loop watches, so
it can be parked without taking the VNC stack or chromedriver down with it —
useful when you want the ~1 GiB back between design passes:

```bash
task browser:status                 # running | starting | parked
task browser:stop                   # park Chrome
task browser:start                  # relaunch, waits for CDP to answer
task browser:restart                # both, for a clean window
task browser:logs                   # follow the sidecar log
task browser:exec CMD="ps -ef"      # run a command inside the sidecar
task browser:shell                  # interactive shell in the sidecar
```

The last four hop through `kubectl exec`/`logs` into the `browser` container of
this workspace pod; everything above them is a plain `localhost` port.

**A devfile change needs a workspace restart.** Adding the sidecar does not
affect the pod you are already in — Che recreates the pod from the devfile on
restart, and `task browser:check` fails with a connection error until then.

Chromium cannot run in the tools container itself: the image has no
`libglib-2.0`, and there is no root to install it. That is the gap the sidecar
fills, rather than a preference for sidecars.

## 11. An OIDC provider in the workspace (the dex sidecar)

`docker-compose.yml` runs **Authentik** for `[[auth]] type = "oidc"` work, and a
workspace has no compose to run it in — it is four containers and a Postgres of
its own. `devfile.yaml` declares a **`dex` sidecar** instead: one container,
in-memory storage, listening on **9000**, the same port Authentik uses. So
`localhost:9000` is the local identity provider either way, and only `issuer_url`
tells the two apart.

```bash
task dex:config                     # render the config + the [[auth]] blocks — live in ~2s
task dex:reload                     # re-read the config without changing it
task dex:check                      # discovery document — is it up, and as whom?
task run:space                      # the server, already pointed at dex
task dex:token -- admin@example.com password   # a JWT for curl, no browser
task dex:logs -f                    # follow the sidecar
```

Two accounts, both with the password `password`: `admin@example.com` → `admin`
and `dev@example.com` → `user`.

**A config change costs two seconds, not a workspace.** Dex has no reload: it
parses its config once, at startup, and there is no signal that makes it read the
file again. The obvious way to run it again — end the container and let the
kubelet bring it back — is the expensive one here, because a container that
terminates in this pod does not come back alone: the pod is replaced and every
sidecar goes down with it. That is a workspace restart with extra steps, and it
cost three of them before it was understood.

So dex is not PID 1 in its container. A small supervisor is, and it never exits;
dex is its child. The supervisor hashes `dev/dex/config.yaml` and `dev/dex/reload`
every two seconds, and on any change kills dex and starts it again in place. The
container stays `Running` throughout and its restart count stays `0` — nothing
outside that one process notices. `task dex:config` ends by calling
`task dex:reload`, so rendering a new config *is* applying it; `task dex:reload`
on its own bumps the `reload` file, for a config whose bytes did not change.

Three details worth knowing, because each is load-bearing:

- **It is checked before it is applied.** `dex:reload` first boots the candidate
  config with the same dex binary, in the same container, on ports 19000-19002,
  and looks for `server=http` in the output — logged only after parsing,
  validation and the bind have all succeeded. A config that fails leaves the
  running instance untouched.
- **The answer comes from the sidecar, not the port.** The supervisor writes
  `dev/dex/state` each time it starts dex, and the task waits for that value to
  change. Polling `:9000` alone cannot distinguish "restarted" from "the old dex
  is still answering with the config you just replaced".
- **A bad config makes it hold, not loop.** If dex exits on a config nobody has
  touched since, starting it again would fail identically and bury the one log
  line that explains why. The supervisor stops and says what would end the hold;
  the next `task dex:config` starts it again by itself.

**`task run:space` needs no pasting.** `config.example-space.toml` is the
workspace's config, and its two `[[auth]] type = "oidc"` blocks already describe
dex — email-mapped roles and all. What it does not contain is a workspace name:
the issuer, the callback host and the SPA origin are `${BATLEHUB_DEX_ISSUER}`,
`${BATLEHUB_BACK_URL}` and `${BATLEHUB_FRONT_URL}`, which the task fills from the
same `url` helper `task dex:config` uses, so the tracked file works in anyone's
workspace and the two cannot disagree about the FQDN. Pass `LOCAL=1` to both or
to neither — the issuer is one value, and a mismatch is every login failing on
the `iss` check. The generated `dev/dex/auth.toml` remains what you paste into a
config of your own.

**Why the config is rendered and not committed.** The issuer is one value that
every party has to agree on: the browser is redirected to it, the server fetches
`{issuer}/.well-known/openid-configuration` from it, and the `iss` that comes back
is checked against `issuer_url`. In a workspace that is the gateway FQDN, which
contains the workspace name — so `dev/dex/config.yaml` is generated from
`dev/dex/config.tmpl.yaml` (tracked) by `task dex:config` and gitignored, along
with the `dev/dex/auth.toml` it writes beside it: the two `[[auth]]` blocks with
issuer, redirect and frontend URLs already filled in, to paste over the Authentik
ones in your config. The pod can reach its own ingress, so the server is content
with the same URL the browser uses.

`task dex:config LOCAL=1` renders `http://localhost:9000` instead —
`is_secure_issuer_url` permits plain HTTP on loopback exactly so this works. The
server and the `browser` sidecar can both reach that (one pod, one network
namespace, so the rendered gates can drive a full SSO flow); your own browser
cannot.

**Roles map off `email`, not `groups`.** Dex's password DB carries no group
membership — groups only ever come from a real connector (LDAP, GitHub) — so
`role_claim = "groups"` would find no claim at all and `map_role` would fall
through to `Role::Anonymous` for every login. `email` is always present with the
`email` scope and is matched the same way, a string against the keys of
`[auth.role_mappings]`.

**The endpoint is public and not `secure`.** It has to be: the server fetches the
discovery document unauthenticated, and Che's auth in front of it would answer
that fetch with a login page. What is exposed is a throwaway provider holding two
accounts whose password is `password`. Put nothing in it you would mind a
passer-by reaching; for a sealed setup, use `LOCAL=1` and switch the endpoint to
`exposure: internal`.

As with the browser sidecar, **a devfile change needs a workspace restart** —
`task dex:check` fails with a connection error until then. That includes the
supervisor above: a pod started before it was added still runs the boot-once
container, and `task dex:reload` says so and changes nothing rather than pretending
otherwise. One restart adopts it, and it is the last one a config change asks for.
The container waits for `dev/dex/config.yaml` rather than exiting when it is
missing, so a workspace that has never run `task dex:config` still starts; it would
otherwise crash-loop and leave the pod short of Ready.


---

## 12. Writing an RFC

An RFC is for a change worth arguing about *before* it is written: new
user-facing surface, a cross-crate refactor, a security-relevant default, or
anything where the wrong choice is expensive to undo. A bug fix is not an RFC.
The published set is [Design history](/rfc/); the form itself is not published —
it is `docs/internal/0000-rfc-template.md`, something you copy rather than read.

```bash
task rfc:new TITLE="Artifact retention policies" \
             SETTLES="How long a cached artifact is kept, and who decides"
```

That takes the next free number (numbers are never reused, even by a rejected
RFC), fills in the author from `git config`, dates it today, sets the status to
`Draft`, and writes the page into both listings so it is not an orphan. A
follow-on to an existing RFC — the *bis* convention — is `BIS=0004`; `SHORT=`
overrides the sidebar label and `SLUG=` the filename.

**Three header rows are read back out of the document**, by
`docs/build/rfc-meta.mjs`:

| Row | Where it is quoted |
| --- | --- |
| `Status` | the banner at the top of the published page |
| `Short` | the `/rfc/` sidebar and the first column of the index table |
| `Settles` | the "What it settles" column of that table |

So the index table and the sidebar are **generated** — the blocks between the
`rfc-index` markers in `docs/rfc/index.md` and the `rfc-sidebar` markers in
`docs/.vitepress/config.ts`. Change the RFC and run `task rfc:index`; never edit
those two blocks by hand. `task rfc:index:check` fails `task docs:design` when
they have drifted, for the same reason every other generated table here has a
drift check: a page that mislabels the project's own design history is the
defect RFC 0005 exists to remove, one directory over.

The status vocabulary is `Draft`, `In review`, `Accepted`, `Implemented`,
`Rejected` and `Superseded by NNNN`; anything else fails the docs build rather
than rendering an unlabelled page. `task rfc:status` prints where each document
stands, including its open-question count — the template's own readiness test is
that `### Still open` is empty, and the report flags an RFC that has reached it
and not moved:

```text
RFC      Status              Open  Name
0010     Accepted            0     The toolchain layer                          ← no open questions
0011     Draft               7     Authenticated OpenVSX access

17 RFCs — 8 settled, 9 still proposals; 23 open question(s) across 5 document(s)
```
