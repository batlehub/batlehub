---
title: Getting started
description: Set up, run, test and change BatleHub
---
<!-- techdoc:begin getting-started#notice -->
> Sections between `techdoc` markers are regenerated; write your own notes outside them.
<!-- techdoc:end -->

<!-- techdoc:begin getting-started#prerequisites -->
## Prerequisites

Tools are pinned in `mise.toml`; install them with `mise install`. Main ones: `rust` (latest), `node` 26, `pnpm` 12, `task`, `helm`, `cocogitto`, `k6`, `go` 1.27.2, and the security tools `cargo-deny`, `cargo-audit`, `trivy`, `syft`, `gitleaks` 8.30.1, `semgrep`, `postmortem` 2.5.0. A PostgreSQL instance is required to run the server; Podman is used by the integration-test tasks. A Che/DevWorkspace setup is described in `devfile.yaml`.
<!-- techdoc:end -->

<!-- techdoc:begin getting-started#tasks -->
## Tasks

`Taskfile.yml` only includes `.tasks/<domain>.yaml` files with `flatten: true`.

| Task | What it does |
| --- | --- |
| `build` / `check` | Build / type-check the workspace |
| `lint` | `cargo clippy --workspace -- -D warnings` and `cargo fmt --all --check` |
| `fmt` | Format all Rust sources |
| `test` / `test:all` | Workspace tests / backend + frontend suites in parallel |
| `test:pg-*`, `test:s3` | Integration tests against a temporary Postgres / RustFS (Podman) |
| `test:*-heavy` | Real-client suites (`tests/heavy/`) |
| `coverage` / `coverage-check` | HTML + lcov report / fail under 80 % lines |
| `run` | Start the server with the example config (needs Postgres) |
| `dev` | Backend and Vite frontend in parallel |
| `ui:dev` / `ui:dev:local` | Vite on 5173 (workspace FQDN) / 5174 (localhost:8080) |
| `dump-spec` | Dump the OpenAPI spec to `ui/openapi.json` |
| `compose:up` / `compose:db` | Postgres + server via Podman Compose / Postgres only |
| `audit` / `deny` / `security` | `cargo audit` / `cargo deny check` / full local security gate |
| `fuzz:check` / `fuzz` | Fuzz targets compile / run one target |
| `helm:lint` / `helm:docs` | Lint the chart / regenerate its README |
| `cli` / `cli:tui` | Run `batlehub-cli` / its TUI |
| `config:explain` | Print what a config grants after wildcard expansion |
| `docs:dev` / `docs:build` | VitePress docs site |
<!-- techdoc:end -->

<!-- techdoc:begin getting-started#run -->
## Run and test locally

```bash
mise install
task compose:db                                        # Postgres only
cargo run -p batlehub-server -- --config config.example.toml
task ui:dev:local                                      # console on :5174
cargo test --workspace
cargo clippy --workspace -- -D warnings && cargo fmt --all --check
cd ui && pnpm install && pnpm test && pnpm run lint
```

After changing an API route: `task dump-spec` then `cd ui && pnpm run generate` to refresh `ui/openapi.json` and `ui/src/client`.
<!-- techdoc:end -->

<!-- techdoc:begin getting-started#config -->
## Configuration

The server reads TOML (`config.example.toml`, `config.example-s3.toml`, `config.example-space.toml`). Top-level sections are the fields of `crates/config/src/schema/mod.rs` `struct AppConfig`: `server`, `database`, `auth`, `storage`, `cache`, `registries`, `grants`, `otel`, `logging`, `audit`, `limits`, `ip_blocking`, `notifications`, `flag_sources`, `release_imports`, `air_gap`, `proxy`, `vulnerability_scan`, `cache_coherence`, `upstream_audit`, `subdomain_routing`, `stats`, `search`, `scanners`, `worker`.

| Env var / flag | Read by | Default | Required |
| --- | --- | --- | --- |
| `--config` (repeatable) | `server/src/main.rs` `fn config_paths` | — | no |
| `BATLEHUB_CONFIG` (`:`-separated layers) | `server/src/main.rs` `fn config_paths` | `config.toml` | no |
| `--roles` (`proxy`, `worker`) | `server/src/main.rs` `fn parse_roles` | `[server].roles`, both | no |
| `PROXY_CACHE__<SECTION>__<FIELD>` (server host/port/static_dir, database url/pool, single storage path/bucket/region/endpoint, otel endpoint/service_name) | `crates/config/src/schema/mod.rs` `fn apply_env_overrides_from` | values from TOML | no |
| `BATLEHUB_DISABLE_HOT_RELOAD` | `server/src/main.rs` `fn main` | unset | no |
| `KUBERNETES_SERVICE_HOST` / `_PORT` | default API URL for `[[auth]] type = "kubernetes"` (`crates/core/src/ports/auth/auth_config.rs`) | — | for `type = "kubernetes"` auth |
| `VITE_API_BASE_URL` | `ui/` (Vite) | set by `ui:dev*` tasks | yes for the dev server |
<!-- techdoc:end -->

<!-- techdoc:begin getting-started#where-to-change -->
## Where to change things

- New registry kind: follow `CLAUDE.md` § "Adding a new registry adapter" and `docs/contributing/adding-a-registry.md` — client in `crates/adapters/src/registry/<name>`, handlers in `crates/web/src/handlers/proxy/<name>`, routes in `crates/web/src/lib.rs` `fn collect_routes`, variant in `crates/core/src/entities/registry_kind.rs` `enum RegistryKind`, wiring in `server/src/builders.rs`, UI entry in `ui/src/config/registryTypes.ts`.
- New admin/API route: handler under `crates/web/src/handlers/back_office/` or `front_office/`, registered in `fn collect_routes`; every `200`/`201` needs `body = T` (`crates/web/tests/openapi_contract.rs`).
- New policy rule: `crates/core/src/rules/`.
- New config key: `crates/config/src/schema/`; hot vs restart classification in `crates/config/src/reload_keys.rs`.
- New migration: `crates/adapters/migrations/NNN_*.sql` and a `mig!` line in `crates/adapters/src/migrations.rs`.
- New CLI command: `cli/src/cli/mod.rs` and a module in `cli/src/cli/`.
- New console page: `ui/src/pages/` and `ui/src/router/index.ts`.
<!-- techdoc:end -->
