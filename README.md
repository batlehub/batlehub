# BatleHub - Proxy Cache

A self-hosted smart proxy and cache for package registries. It sits between your build tools and the internet, caches artifacts after the first download, and enforces access-control rules before any package reaches a developer or CI pipeline.

## Supported registries

| Registry | Protocol | Default upstream |
|----------|----------|-----------------|
| **GitHub** | Releases, assets, tarballs, raw files, git refs | `api.github.com` |
| **Forgejo** | Releases, assets, tarballs, raw files, git refs | `codeberg.org` |
| **GitLab** | Releases, assets, tarballs, raw files, git refs | `gitlab.com` |
| **npm** | Full packument + tarball proxy | `registry.npmjs.org` |
| **Cargo** | Sparse index + `.crate` download | `crates.io` / `index.crates.io` |
| **NuGet** | V3 flat index + registration pages + `.nupkg` download | `api.nuget.org` |
| **OpenVSX** | VS Code extension VSIX download | `open-vsx.org` |
| **VS Code Marketplace** | VS Code extension VSIX download via Microsoft Gallery API | `marketplace.visualstudio.com` |
| **JetBrains Marketplace** | IDE plugin API (search, compatible updates, downloads) + `updatePlugins.xml` custom repo | `plugins.jetbrains.com` |
| **Go** | GOPROXY protocol (`.info`, `.mod`, `.zip`, `@latest`, `@v/list`) | `proxy.golang.org` |
| **Maven** | Maven Central-compatible metadata XML + JAR / POM downloads | `repo1.maven.org/maven2` |
| **Terraform** | Provider and module proxy protocol (v1 API) | `registry.terraform.io` |
| **RubyGems** | Gem downloads, version listing, REST info API | `rubygems.org` |
| **Composer** | Packagist v2 protocol (`packages.json`, p2 metadata, dist downloads) | `repo.packagist.org` |
| **PyPI** | Simple Repository API (PEP 503/691) + JSON API; URL rewriting for pip/uv/Poetry | `pypi.org` |
| **Conda** | repodata.json channel proxy; `.conda` and `.tar.bz2` package downloads | `conda.anaconda.org` |
| **Debian / APT** | Path-addressed repository mirror; BatleHub-signed indexes in local mode | `deb.debian.org` |
| **RPM / YUM** | Path-addressed repository mirror; BatleHub-signed `repomd.xml` in local mode | *(explicit upstream required)* |
| **Pacman** | Path-addressed Arch mirror (`$repo/os/$arch/…`) | `geo.mirror.pkgbuild.com` |
| **JetBrains IDE** | Path-addressed IDE archive mirror | `download.jetbrains.com` |
| **Node.js dist** | `index.tab` release listing + dist tarballs (nvm, fnm, Volta) | `nodejs.org/dist` |
| **SDKMAN** | Candidates API + broker download redirects | `api.sdkman.io/2` |
| **Generic** | Arbitrary path-addressed file tree mirror | *(explicit upstream required)* |

Multiple instances of the same registry type can run in parallel (e.g. a private npm registry and the public one as fallback).

### Feature matrix

Four capabilities are protocol-independent and hold for **every** registry type
above, so they are stated once here rather than repeated as an all-✓ row:
artifact caching, administrative version blocking, the deny-latest-tag rule, and
multi-upstream fanout with 404 failover.

The rest splits in two, because the two halves are addressed differently and do
not answer the same questions. **Package registries** are addressed by a package
coordinate (name and version) and can host versions themselves. **Forges,
mirrors and toolchain distributions** are addressed by repository ref or by
upstream file path.

#### Package registries

| Feature | npm | Cargo | NuGet | OpenVSX | VS Code Mkt | JetBrains Mkt | Go | Maven | Terraform | RubyGems | Composer | PyPI | Conda |
|---------|:---:|:-----:|:-----:|:-------:|:-----------:|:-------------:|:--:|:-----:|:---------:|:--------:|:--------:|:----:|:-----:|
| Version listing | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ ⁵ |
| Version metadata | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `latest` pseudo-version ¹ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | — | — | — | — | — | — |
| Upstream search | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — |
| Source archive download | ✓ | ✓ | — | — | — | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Binary / extension download | — | — | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | — | — | ✓ | ✓ |
| Sparse index proxy | — | ✓ | — | — | — | — | — | — | — | — | — | — | — |
| Module definition file | — | — | — | — | — | — | ✓ | — | — | — | — | — | — |
| Publish timestamp | ✓ | ✓ | ⚠ ⁶ | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠ ⁴ | ✓ | ✓ | ✓ | ⚠ ⁵ |
| Signed release detection | — | — | — | ✓ | ✓ | — | — | — | — | — | — | — | — |
| Release age gate rule | ✓ | ✓ | ⚠ ⁶ | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠ ⁴ | ✓ | ✓ | ✓ | ⚠ ⁵ |
| **Private publish** (`mode = local/hybrid`) | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ |

#### Forges, mirrors and toolchain distributions

| Feature | GitHub | Forgejo | GitLab | Debian | RPM | Pacman | JetBrains IDE | Node.js dist | SDKMAN | Generic |
|---------|:------:|:-------:|:------:|:------:|:---:|:------:|:-------------:|:------------:|:------:|:-------:|
| Release / version listing | ✓ | ✓ | ✓ | — | — | — | — | ✓ | ✓ | — |
| Version metadata | ✓ | ✓ | ✓ | — | — | — | — | ✓ | ✓ | — |
| Git refs (branches, tags, commits) | ✓ | ✓ | ✓ | — | — | — | — | — | — | — |
| Raw file access | ✓ | ✓ | ✓ | — | — | — | — | — | — | — |
| Path-addressed file fetch ⁸ | — | — | — | ✓ | ✓ | ✓ | ✓ | — | — | ✓ |
| Upstream artifact probe (`HEAD`) | — | — | — | ✓ | ✓ | ✓ | ✓ | — | — | ✓ |
| Publish timestamp | ⚠ ² | ⚠ ² | ⚠ ² | — | — | — | — | ✓ | — ⁷ | — |
| Signed release detection | ✓ ⁹ | ✓ ⁹ | ✓ ⁹ | — | — | — | — | — | — | — |
| Release age gate rule | ⚠ ² | ⚠ ² | ⚠ ² | — | — | — | — | ✓ | — ⁷ | — |
| **Private publish** (`mode = local/hybrid`) | — | — | — | ✓ ³ | ✓ ³ | ✓ ³ | — | — | — | — |

> ¹ **`latest` pseudo-version**: whether the adapter resolves the literal version string `latest` to a concrete version. Independent of the deny-latest-tag rule, which fires on that string for every registry type whether or not the adapter would have resolved it.
>
> ² **Forges (GitHub, Forgejo, GitLab)**: a tag coordinate takes its timestamp from the release, and a ref-addressed raw or archive coordinate from the resolved commit's date. The release-listing pseudo-version carries none, and a commit lookup the forge cannot answer serves the artifact undated — the age gate then does whatever `deny_missing_timestamp` says.
>
> ³ **Private publish**: set `mode = "local"` to use BatleHub as the authoritative registry (no upstream needed), or `mode = "hybrid"` to serve locally published packages first and fall through to an upstream for everything else. For Debian, RPM and Pacman, local mode also regenerates and signs the repository indexes and serves the public key. See the self-hosted / private registry example below, and [`docs/guide/configuration.md § Registry modes`](docs/guide/configuration.md#registry-modes) for the full reference.
>
> ⁴ **Terraform publish timestamp**: the module version detail endpoint (`/v1/modules/{ns}/{name}/{prov}/{ver}`) is part of the official Terraform Module Registry Protocol and always provides `published_at`. The provider version detail endpoint (`/v1/providers/{ns}/{type}/{ver}`) is supported by `registry.terraform.io` but is not in the official spec — other Terraform registries may omit `published_at`. When absent, the release age gate is skipped rather than blocking access.
>
> ⁵ **Conda**: conda has no dedicated per-package version listing API. BatleHub synthesises one by scanning `repodata.json` for `noarch`, `linux-64`, `osx-64`, `osx-arm64`, and `win-64` — the union of versions found across all available platforms is returned. Platforms that return 404 or a network error are silently skipped. The publish timestamp is read from the `timestamp` field in `repodata.json` (milliseconds since epoch); most packages carry it, but some older or third-party packages omit it — when absent the release age gate is skipped rather than blocking access.
>
> ⁶ **NuGet publish timestamp**: the V3 flat index carries no publication date, so BatleHub reads the `Last-Modified` header of a `HEAD` on the `.nupkg`. That is the date the file was written on the upstream CDN, which tracks the publication date closely but is not the protocol's own guarantee. When the upstream returns no such header the release age gate is skipped.
>
> ⁷ **SDKMAN**: the candidates API publishes no dates at all, so the age gate on this registry type decides every request with `deny_missing_timestamp`. Config validation makes the operator state that value explicitly rather than inherit a default.
>
> ⁸ **Path-addressed**: the whole upstream path travels in the coordinate and there is no per-package metadata API, so `path_allow` and `cache.warm_paths` are the controls that apply. These are also the only types whose upstream can be probed with a `HEAD` for the artifact-disappearance sweep.
>
> ⁹ **Forge signature detection**: applies to release assets. A ref-addressed raw or archive coordinate is served without a signature verdict.

## Key features

- **Artifact caching** — first download is fetched from upstream and stored; subsequent requests are served from local or S3 storage.
- **Private / local registry** — `npm`, `cargo`, `nuget`, `openvsx`, `vscode-marketplace`, `jetbrains-marketplace`, `goproxy`, `rubygems`, `maven`, `terraform`, `composer`, `pypi`, `conda`, `deb`, `rpm`, and `pacman` registries can be set to `mode = "local"` (fully private, no upstream) or `mode = "hybrid"` (local-first with upstream fallback). Teams publish packages directly to BatleHub using standard tools (`npm publish`, `cargo publish`, `gem push`, `mvn deploy`, `twine upload`, `dotnet nuget push`, raw VSIX / Go zip / Terraform provider upload / Composer ZIP / conda package upload / JetBrains plugin upload / `.deb`, `.rpm` and `.pkg.tar.zst` upload).
- **Ownership & team management** — per-package owner table (user or group, admin or maintainer role). The first publisher becomes the package admin; subsequent publishes require an owner record. Manage via the admin API or let it be set automatically.
- **Team namespaces & package visibility** — assign a package name prefix (e.g. `frontend/`) to an auth-provider group so only its members can publish there. Set per-package visibility to `public` (default), `internal` (any authenticated user), or `team` (group members only) to control who can download.
- **Versioning policies** — enforce semver, block pre-release versions, or restrict accepted version strings with a regex. Violations return HTTP 422 at publish time.
- **Artifact signing** — publish with `X-Artifact-Signature` (base64) and `X-Signature-Type` headers; signatures are stored alongside the artifact and returned on every download. Optionally require signatures (`signing.required = true`) and restrict accepted types.
- **Bulk operations** — bulk yank, unyank, and delete via the admin API; process hundreds of versions in a single request.
- **Publish quota** — per-user publish quotas (max storage bytes, max package count) with `block` or `warn` enforcement. `X-Quota-*` response headers on every publish.
- **Rate limiting** — per-user and per-group request rate limits with configurable windows. `X-RateLimit-*` headers; supports per-group pools (e.g. a shared CI-bot bucket).
- **RBAC** — per-registry permissions for `anonymous`, `user`, and `admin` roles, plus group-based access from OIDC or Kubernetes claims.
- **Release age gate** — block packages published less than N seconds ago (supply-chain delay window).
- **Deny latest tag** — reject requests that use `"latest"` as a version, forcing consumers to pin exact versions. Configurable bypass roles (e.g. admins may still use `latest`).
- **Fanout / failover** — list multiple upstreams per registry; 404 from one falls through to the next.
- **Self-hosted registry support** — upstream auth (Bearer token, Basic, or custom header) and custom CA certificates per registry, for air-gapped or corporate environments.
- **Auth providers** — static tokens (plain-text or Argon2id hashed), OIDC (Authentik, Keycloak, Dex, …), Kubernetes service account tokens, and GitHub/Forgejo **Actions OIDC** tokens with rule-based group mapping (map any JWT claim — repo, branch, environment — to named groups and roles).
- **Storage backends** — filesystem or S3-compatible (AWS S3, MinIO, RustFS). Different registries can use different backends.
- **Audit log** — every allow and deny decision is recorded in PostgreSQL.
- **OpenTelemetry** — optional distributed tracing via OTLP/gRPC.
- **Web UI** — a Vue 3 SPA for browsing packages via the Package Explorer, managing firewall blocks, and generating client config snippets.
- **Package Explorer** — browse and search all cached and locally-published packages across every registry from a single `/explore` page. Filter by registry, sort by downloads, name, or last access. The per-package detail view shows every version alongside its firewall status (Clear / Blocked / Yanked) and beta-channel gate state. An upstream search surfaces packages not yet cached. Fine-grained RBAC lets you grant explore access independently of proxy/download access (e.g. read-only CI tokens cannot browse, but developer accounts can).
- **Hot reload** — update registries, RBAC, and policies without restarting. A file watcher loads a pending reload when `config.toml` changes; the admin confirms it via the UI or `POST /api/v1/admin/config/pending/apply`. The immediate-reload endpoint (`POST /api/v1/admin/config/reload`) covers automation pipelines. Disable with `BATLEHUB_DISABLE_HOT_RELOAD=1` (e.g. read-only Kubernetes ConfigMaps). All reloads are recorded in an audit trail.
- **Global admin banner** — broadcast an info / warning / error message to all website visitors from the `/admin/config-reload` UI page or `PUT /api/v1/admin/banner`. The banner is automatically set during a config reload and cleared on completion. Backed by in-memory, Redis, or PostgreSQL depending on your cache backend — all replicas see the same message in HA deployments.
- **SBOM generation** — automatically generate SPDX 2.3 and CycloneDX 1.4 Software Bills of Materials for every cached and locally-published artifact. Dependency manifests are extracted from archives (`go.mod`, `Cargo.toml`, `package.json`, `pom.xml`, `requirements.txt`) or fetched from upstream APIs (GitHub, npm). Export a merged org-level SBOM covering all artifacts served in a time window via `GET /api/v1/sbom/export`; per-artifact SBOMs are available from the Package Explorer and via `GET /api/v1/sbom/{registry}/{name}/{version}`. Enable with `[registries.sbom]` in `config.toml`; optionally deny publishing if no manifest is found (`required = true`). See [`docs/guide/sbom.md`](docs/guide/sbom.md).
- **OpenAPI** — full Swagger UI at `/swagger-ui/` and spec dump via `batlehub dump-spec`.

---

## Quick start

### With Docker Compose

```sh
# Clone and start PostgreSQL + the server
git clone https://github.com/your-org/batlehub
cd batlehub
cp config.example.toml config.toml   # edit as needed
podman compose up -d                 # or docker compose up -d
```

The server listens on `http://localhost:8080`. The admin token from `config.example.toml` is `change-me-admin-token`.

### Build from source

**Prerequisites:** Rust 1.87+, Node 24+, PostgreSQL

```sh
# Backend
cargo build --release -p batlehub-server

# Frontend (optional — embeds the SPA into the server)
cd ui && pnpm install --frozen-lockfile && pnpm run build && cd ..

# Generate the OpenAPI spec and TypeScript client
cargo run -p batlehub-server -- --config config.example.toml dump-spec > ui/openapi.json
cd ui && pnpm run generate && pnpm run build && cd ..

# Run
./target/release/batlehub --config config.toml
```

Or use the [Task](https://taskfile.dev) shortcuts:

```sh
task compose:db    # start only postgres
task run           # cargo run with example config
task ui:dev        # vite dev server (proxies /api and /proxy to :8080)
task test          # cargo test --workspace
```

### Install batlehub-cli

**via mise** (recommended — manages version automatically):

```sh
mise use "github:batleforc/batlehub[asset_pattern=batlehub-cli-*]"
```

**via cargo** (builds from source):

```sh
cargo install --git https://github.com/batlehub/batlehub batlehub-cli
```

---

## Configuration at a glance

The server is configured with a single TOML file (`config.toml` by default, override with `--config`). You can use the [Config Generator](https://batleforc.git.batleforc.fr/batlehub/guide/config-generator.html) to help you create a configuration or see [`docs/guide/configuration.md`](docs/guide/configuration.md) for the full reference and worked examples.

### Minimal example

```toml
[server]
port = 8080

[database]
type = "postgresql"
url  = "postgresql://batlehub:changeme@localhost:5432/batlehub"

[[auth]]
type = "token"

[[auth.tokens]]
# Plain-text token (fine for local dev).
# For production, store an Argon2id PHC hash instead:
#   batlehub hash-token my-secret-token
value = "my-admin-token"
role  = "admin"

[storage]
type = "filesystem"
path = "./cache"

[[registries]]
type = "npm"
name = "npm"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
```

### Go module proxy example

```toml
[[registries]]
type = "goproxy"
name = "go"
# upstreams defaults to ["https://proxy.golang.org"]

[registries.rbac]
anonymous = ["releases:read", "source:read"]
```

Then point the go toolchain at the proxy:

### Self-hosted / private registry example

Bearer token and custom CA for a corporate Gitea instance:

```toml
[[registries]]
type      = "npm"
name      = "npm-internal"
upstreams = ["https://gitea.corp.example.com/api/packages/myorg/npm"]

[registries.upstream_auth]
type  = "bearer"
token = "npat-xxxx"

[registries.tls]
ca_cert_path = "/etc/ssl/corp-ca.pem"

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

Three auth schemes are supported: `bearer`, `basic`, and `header` (custom header such as `X-API-Key`). See [docs/guide/private-upstreams.md](docs/guide/private-upstreams.md) for the full reference.

```sh
export GONOSUMCHECK="*"
export GONOSUMDB="*"
export GOPROXY="http://localhost:8080/proxy/go,direct"
go get golang.org/x/text@latest
```

---

## Client tool configuration

The built-in **Setup Guide** page (`/setup`) generates ready-to-paste config snippets for all supported tools. The snippets below are illustrative; the UI generates them pre-filled with your server's actual address.

More info about each reigstry setup (User Side or Admin Side) can be [found in the doc](https://batleforc.git.batleforc.fr/batlehub/registries/).

### npm / yarn / pnpm

```sh
# .npmrc
registry=http://localhost:8080/proxy/npm/
```

### Cargo

```toml
# .cargo/config.toml
[source.crates-io]
replace-with = "batlehub"

[source.batlehub]
registry = "sparse+http://localhost:8080/proxy/cargo/registry/"
```

### Go

```sh
export GOPROXY="http://localhost:8080/proxy/go,direct"
export GONOSUMCHECK="*"
export GONOSUMDB="*"
```

### RubyGems

```sh
gem sources --add http://localhost:8080/proxy/gems/
# or per-command:
gem install rails --source http://localhost:8080/proxy/gems/
```

### VS Code Marketplace

```sh
# Download and install an extension via the proxy
curl -sL "http://localhost:8080/proxy/vscode/ms-python.python/latest/vsix" \
  -o extension.vsix && code --install-extension extension.vsix

# Pin a specific version
curl -H "Authorization: Bearer <token>" \
  "http://localhost:8080/proxy/vscode/ms-python.python/2024.2.1/vsix" \
  -o ms-python.python-2024.2.1.vsix
```

The proxy URL pattern is `/proxy/{registry}/{publisher}.{name}/{version}/vsix`.

### Maven

```xml
<!-- ~/.m2/settings.xml -->
<settings>
  <mirrors>
    <mirror>
      <id>batlehub</id>
      <name>BatleHub Maven Proxy</name>
      <url>http://localhost:8080/proxy/maven/maven2/</url>
      <mirrorOf>*</mirrorOf>
    </mirror>
  </mirrors>
</settings>
```

### Terraform (provider network mirror)

```hcl
# ~/.terraformrc
provider_installation {
  network_mirror {
    url = "http://localhost:8080/proxy/terraform/"
  }
}
```

### PyPI

```ini
# ~/.pip/pip.conf
[global]
index-url = http://localhost:8080/proxy/pypi/simple/
```

Or with uv:

```toml
# pyproject.toml
[[tool.uv.index]]
name    = "batlehub"
url     = "http://localhost:8080/proxy/pypi/simple/"
default = true
```

### Conda

```yaml
# ~/.condarc
channels:
  - http://localhost:8080/proxy/conda
  - nodefaults
```

### GitHub (mise)

```toml
# ~/.config/mise/config.toml
[settings.url_replacements]
"regex:^https://api\\.github\\.com/repos/(.+)" = "http://localhost:8080/proxy/github/$1"
"regex:^https://github\\.com/([^/]+)/([^/]+)/releases/download/([^/]+)/(.+)" = "http://localhost:8080/proxy/github/$1/$2/releases/download/$3/$4"
```

---

## Architecture

```mermaid
flowchart TD
  CFG["config.toml — registries entries<br>type = npm | cargo | github | openvsx | vscode-marketplace<br>| goproxy | maven | terraform | rubygems | composer | pypi | conda"]
  MAIN["server/src/main.rs<br>builds registry clients, policies, services"]
  HTTP["HTTP handlers (actix-web)<br>one module per registry type"]

  CFG --> MAIN --> HTTP

  HTTP -->|"proxy mode, or hybrid fallthrough"| proxy
  HTTP -->|"local or hybrid mode"| local

  subgraph proxy["ProxyService — caching, rules, streaming"]
    direction TB
    PM["resolve_metadata()<br>registry adapter: version info from upstream"]
    PR["evaluate rules<br>RBAC, block list, release age gate, deny latest"]
    PC["storage cache<br>filesystem or S3"]
    PF["fetch_artifact()<br>registry adapter: streams bytes from upstream"]
    PM --> PR --> PC --> PF
  end

  subgraph local["LocalRegistryService — authoritative local/hybrid registry"]
    direction TB
    LP["publish()<br>versioning → ownership → signing → quota → store"]
    LY["yank() / unyank()"]
    LB["bulk_yank() / bulk_unyank() / bulk_remove_versions()"]
    LG["get_artifact()<br>storage + signature headers"]
  end
```

### Crate structure

| Crate | Purpose |
|-------|---------|
| `crates/core` | Domain entities, ports (traits), rules, `ProxyService`, `AdminService`, `LocalRegistryService` |
| `crates/adapters` | Registry clients, auth providers, storage backends, database layer |
| `crates/config` | TOML schema and validation |
| `crates/web` | actix-web handlers, middleware, OpenAPI definitions |
| `server` | Binary entry point — wires everything together |
| `ui` | Vue 3 + Tailwind SPA (package browser, setup guide, admin panel) |


---

## Development

```sh
task build          # cargo build --workspace
task test           # cargo test --workspace
task lint           # cargo clippy --workspace
task fmt            # cargo fmt --all
task dump-spec      # regenerate ui/openapi.json
task ui:generate    # regenerate TypeScript client from openapi.json
task coverage       # generate HTML coverage report (requires PostgreSQL + MinIO)
task coverage-check # enforce ≥80% line coverage (fails the build if below threshold)
```

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — contributor guide (points to [`docs/contributing/contributing.md`](docs/contributing/contributing.md))
- [`New Registry`](https://batleforc.git.batleforc.fr/batlehub/contributing/contributing.html#new-registry-type-proxy-only)

---

## Documentation

| Document | Contents |
|----------|---------|
| [`docs/`](docs/) | VitePress documentation site — run `task docs:dev` to browse locally |
| [`docs/guide/installation.md`](docs/guide/installation.md) | Installation guide: Docker Compose, binary, Helm chart |
| [`docs/guide/administration.md`](docs/guide/administration.md) | Administration: config, auth, S3, health, package management |
| [`docs/use/index.md`](docs/use/index.md) | User guide: client setup and publishing for all registry types |
| [`docs/guide/configuration.md`](docs/guide/configuration.md) | Full TOML reference, permissions, worked examples |
| [`docs/guide/configuration.md § Registry modes`](docs/guide/configuration.md#registry-modes) | Private registry modes (local / hybrid) |
| [`docs/guide/private-upstreams.md`](docs/guide/private-upstreams.md) | Upstream auth (Bearer / Basic / header) and custom CA certificates |
| [`docs/use/publishing.md`](docs/use/publishing.md) | Step-by-step guide for publishing packages (npm, Cargo, VSIX, Go modules, gems, Maven artifacts, Terraform modules/providers, PyPI wheels, conda packages) |
| [`docs/guide/sbom.md`](docs/guide/sbom.md) | SBOM configuration, format reference, API endpoints, and export guide |
| [`docs/contributing/adding-a-registry.md`](docs/contributing/adding-a-registry.md) | Step-by-step guide for implementing a new registry adapter |
| `/swagger-ui/` (runtime) | Interactive API docs |


## Contributing, security, and license

- [`Deployment`](https://batleforc.git.batleforc.fr/batlehub/guide/installation.html) or [`docs/guide/installation.md`](docs/guide/installation.md)
- [`ROADMAP.md`](ROADMAP.md) for the full list of planned features, or browse the [Roadmap page on the documentation site](docs/guide/roadmap.md).
- ['Testing'](docs/contributing/testing.md)
- ['Access Control'](https://batleforc.git.batleforc.fr/batlehub/guide/access-control.html)
- ['RFC'](https://batleforc.git.batleforc.fr/batlehub/rfc/)
- [`SECURITY.md`](SECURITY.md) — supported versions and how to report a vulnerability
- [`LICENSE`](LICENSE) — Apache License 2.0

## IA and its role in the project

BatleHub is a solodev that cost me many white nights and a few gray hairs (not yet!!). The IA has helped me think through the design and implementation of complex features, debug tricky issues and write doc. Most of the time it did the job of reviewing my code and make sure that i wasn't going to far from the core design. I also used it to generate documentation and examples, which saved me a lot of time and made the docs more consistent. Overall, the IA has been an invaluable tool for me in this project, and I can't imagine doing it this fast without it. Understanding how some registry work has been a nightmare, and the future registry to come will be even more work, but has the wireframes and the base design is in place, working on new registry is more a matter of copy-pasting and tweaking the existing code to cover any crazy singularity of the new registry.
