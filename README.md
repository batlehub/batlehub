# BatleHub - Proxy Cache

claude --resume d0026b84-fb1e-4be0-8067-1fbcf2ee41ac

A self-hosted smart proxy and cache for package registries. It sits between your build tools and the internet, caches artifacts after the first download, and enforces access-control rules before any package reaches a developer or CI pipeline.

## Supported registries

| Registry | Protocol | Default upstream |
|----------|----------|-----------------|
| **GitHub** | Releases, assets, tarballs, raw files | `api.github.com` |
| **npm** | Full packument + tarball proxy | `registry.npmjs.org` |
| **Cargo** | Sparse index + `.crate` download | `crates.io` / `index.crates.io` |
| **OpenVSX** | VS Code extension VSIX download | `open-vsx.org` |
| **VS Code Marketplace** | VS Code extension VSIX download via Microsoft Gallery API | `marketplace.visualstudio.com` |
| **Go** | GOPROXY protocol (`.info`, `.mod`, `.zip`, `@latest`, `@v/list`) | `proxy.golang.org` |
| **Maven** | Maven Central-compatible metadata XML + JAR / POM downloads | `repo1.maven.org` |
| **Terraform** | Provider and module proxy protocol (v1 API) | `registry.terraform.io` |
| **RubyGems** | Gem downloads, version listing, REST info API | `rubygems.org` |
| **Composer** | Packagist v2 protocol (`packages.json`, p2 metadata, dist downloads) | `repo.packagist.org` |
| **PyPI** | Simple Repository API (PEP 503/691) + JSON API; URL rewriting for pip/uv/Poetry | `pypi.org` |
| **Conda** | repodata.json channel proxy; `.conda` and `.tar.bz2` package downloads | `conda.anaconda.org` |
| **JetBrains Marketplace** | IDE plugin API (search, compatible updates, downloads) + `updatePlugins.xml` custom repo | `plugins.jetbrains.com` |

Multiple instances of the same registry type can run in parallel (e.g. a private npm registry and the public one as fallback).

### Feature matrix

| Feature | GitHub | npm | Cargo | OpenVSX | VS Code Mkt | Go | Maven | Terraform | RubyGems | Composer | PyPI | Conda |
|---------|:------:|:---:|:-----:|:-------:|:-----------:|:--:|:-----:|:---------:|:--------:|:--------:|:----:|:-----:|
| Version listing | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ ⁵ |
| Latest version resolution | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — | ✓ | ✓ | — | ✓ | — |
| Version metadata | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Source archive download | ✓ | ✓ | ✓ | — | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Binary / extension download | ✓ | — | — | ✓ | ✓ | — | ✓ | ✓ | — | — | ✓ | ✓ |
| Raw file access | ✓ | — | — | — | — | — | — | — | — | — | — | — |
| Sparse index proxy | — | — | ✓ | — | — | — | — | — | — | — | — | — |
| Module definition file | — | — | — | — | — | ✓ | — | — | — | — | — | — |
| Publish timestamp | ⚠ ² | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠ ⁴ | ✓ | ✓ | ✓ | ⚠ ⁵ |
| Signed release detection | — | — | — | ✓ | — | — | — | — | — | — | — | — |
| Release age gate rule | ⚠ ² | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ⚠ ⁴ | ✓ | ✓ | ✓ | ⚠ ⁵ |
| Deny latest tag rule | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| Multi-upstream fanout | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| **Private publish** (`mode = local/hybrid`) | — | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ | ✓ ³ |

> ² **GitHub**: publish timestamp (and therefore the age gate) is only populated for specific-tag release requests. Raw file, source tarball, and release-listing requests return no timestamp and the rule is skipped.
>
> ³ **Private publish**: set `mode = "local"` to use BatleHub as the authoritative registry (no upstream needed), or `mode = "hybrid"` to serve locally published packages first and fall through to an upstream for everything else. See [Private registries](#private-registries-local-hybrid-mode) below.
>
> ⁴ **Terraform publish timestamp**: the module version detail endpoint (`/v1/modules/{ns}/{name}/{prov}/{ver}`) is part of the official Terraform Module Registry Protocol and always provides `published_at`. The provider version detail endpoint (`/v1/providers/{ns}/{type}/{ver}`) is supported by `registry.terraform.io` but is not in the official spec — other Terraform registries may omit `published_at`. When absent, the release age gate is skipped rather than blocking access.
>
> ⁵ **Conda**: conda has no dedicated per-package version listing API. BatleHub synthesises one by scanning `repodata.json` for `noarch`, `linux-64`, `osx-64`, `osx-arm64`, and `win-64` — the union of versions found across all available platforms is returned. Platforms that return 404 or a network error are silently skipped. The publish timestamp is read from the `timestamp` field in `repodata.json` (milliseconds since epoch); most packages carry it, but some older or third-party packages omit it — when absent the release age gate is skipped rather than blocking access.

## Key features

- **Artifact caching** — first download is fetched from upstream and stored; subsequent requests are served from local or S3 storage.
- **Private / local registry** — `npm`, `cargo`, `openvsx`, `vscode-marketplace`, `goproxy`, `rubygems`, `maven`, `terraform`, `composer`, `pypi`, `conda`, and `jetbrains-marketplace` registries can be set to `mode = "local"` (fully private, no upstream) or `mode = "hybrid"` (local-first with upstream fallback). Teams publish packages directly to BatleHub using standard tools (`npm publish`, `cargo publish`, `gem push`, `mvn deploy`, `twine upload`, raw VSIX / Go zip / Terraform provider upload / Composer ZIP / conda package upload / JetBrains plugin upload).
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

The server is configured with a single TOML file (`config.toml` by default, override with `--config`). See [`docs/guide/configuration.md`](docs/guide/configuration.md) for the full reference and worked examples.

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

## Private registries (local / hybrid mode)

`npm`, `cargo`, `openvsx`, `vscode-marketplace`, `goproxy`, `rubygems`, `maven`, `terraform`, `composer`, and `jetbrains-marketplace` registries can act as authoritative private registries — not just caches. Set the `mode` field on any registry entry:

| Mode | Behaviour |
|------|-----------|
| `proxy` | Default. Forwards to upstream; publishing is rejected. |
| `local` | BatleHub is the only source. No upstream needed. Clients publish directly to BatleHub. |
| `hybrid` | Local-first. Serves locally published packages; falls back to upstream for anything else. |

### Cargo (private crate registry)

```toml
[[registries]]
type = "cargo"
name = "internal"
mode = "local"

[registries.rbac]
user  = ["source:read"]
admin = ["*"]
```

```toml
# ~/.cargo/config.toml
[registries.internal]
index = "sparse+https://batlehub.example.com/proxy/internal/registry/"
token = "<your-token>"
```

```sh
cargo publish --registry internal
```

### npm (private package registry)

```toml
[[registries]]
type = "npm"
name = "internal-npm"
mode = "local"

[registries.rbac]
user  = ["source:read"]
admin = ["*"]
```

```ini
# .npmrc
@myorg:registry=https://batlehub.example.com/proxy/internal-npm/
//batlehub.example.com/proxy/internal-npm/:_authToken=<your-token>
```

```sh
npm publish --registry https://batlehub.example.com/proxy/internal-npm/
```

### RubyGems (private gem registry)

```toml
[[registries]]
type = "rubygems"
name = "internal-gems"
mode = "local"   # or "hybrid" to fall through to rubygems.org

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

```sh
# Publish
gem push my-gem-1.0.0.gem --host https://batlehub.example.com/proxy/internal-gems \
  --key <your-token>

# Install
gem install my-gem --source https://batlehub.example.com/proxy/internal-gems
```

### VS Code extensions (private VSIX registry)

```toml
[[registries]]
type = "openvsx"     # or "vscode-marketplace"
name = "internal-ext"
mode = "local"

[registries.rbac]
user  = ["source:read"]
admin = ["*"]
```

```sh
# Upload
curl -X PUT -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-org.my-ext-1.0.0.vsix \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-ext/1.0.0/vsix"

# Download
curl -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-ext/my-org.my-ext/1.0.0/vsix" \
  -o my-org.my-ext-1.0.0.vsix
```

### Go (private module proxy)

```toml
[[registries]]
type = "goproxy"
name = "internal-go"
mode = "local"

[registries.rbac]
user  = ["source:read"]
admin = ["*"]
```

Upload a module (PUT the zip archive — `go.mod` is extracted automatically):

```sh
curl -X PUT -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/zip" \
  --data-binary @path/to/module-v1.0.0.zip \
  "https://batlehub.example.com/proxy/internal-go/example.com/mymod/@v/v1.0.0.zip"
```

Point the go tool at the private proxy:

```sh
export GONOSUMCHECK="*"
export GONOSUMDB="*"
export GOPROXY="https://batlehub.example.com/proxy/internal-go,direct"
go get example.com/mymod@v1.0.0
```

### Maven (private artifact registry)

```toml
[[registries]]
type = "maven"
name = "internal-maven"
mode = "local"

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

```xml
<!-- ~/.m2/settings.xml — credentials + mirror -->
<settings>
  <servers>
    <server>
      <id>internal-maven</id>
      <username>your-user-id</username>
      <password>your-token</password>
    </server>
  </servers>
  <mirrors>
    <mirror>
      <id>internal-maven</id>
      <url>https://batlehub.example.com/proxy/internal-maven/maven2/</url>
      <mirrorOf>*</mirrorOf>
    </mirror>
  </mirrors>
</settings>
```

```xml
<!-- pom.xml — publish target -->
<distributionManagement>
  <repository>
    <id>internal-maven</id>
    <url>https://batlehub.example.com/proxy/internal-maven/maven2/</url>
  </repository>
</distributionManagement>
```

```sh
mvn deploy
```

Non-POM files (JARs, checksums) can be uploaded before the POM arrives. The version is committed to the registry when the `.pom` file is uploaded. In `hybrid` mode, artifact requests that miss local storage fall back to the configured upstream.

### Terraform (private module and provider registry)

```toml
[[registries]]
type = "terraform"
name = "internal-tf"
mode = "local"

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

```hcl
# ~/.terraformrc — provider network mirror + credentials
provider_installation {
  network_mirror {
    url = "https://batlehub.example.com/proxy/internal-tf/"
  }
}
credentials "batlehub.example.com" {
  token = "your-token"
}
```

Upload a private module:

```sh
curl -X POST -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/gzip" \
  --data-binary @consul-module.tar.gz \
  "https://batlehub.example.com/proxy/internal-tf/v1/modules/hashicorp/consul/aws/1.0.0"
```

Upload a provider version manifest (then upload binaries per platform via `PUT .../artifact/{os}/{arch}`):

```sh
curl -X POST -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"version":"5.0.0","protocols":["5.0"],"platforms":[{"os":"linux","arch":"amd64","filename":"terraform-provider-aws_5.0.0_linux_amd64.zip","shasum":"abc123..."}]}' \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/hashicorp/aws/versions"

# Upload the platform binary
curl -X PUT -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/zip" \
  --data-binary @terraform-provider-aws_5.0.0_linux_amd64.zip \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64"
```

Yank / unyank a module or provider version:

```sh
# Yank module version (hidden from listings, download still returns stored artifact)
curl -X DELETE -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-tf/v1/modules/hashicorp/consul/aws/versions/1.0.0"

# Unyank module version
curl -X POST -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-tf/v1/modules/hashicorp/consul/aws/versions/1.0.0/unyank"

# Yank / unyank a provider version
curl -X DELETE -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/hashicorp/aws/versions/5.0.0"
curl -X POST -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-tf/v1/providers/hashicorp/aws/versions/5.0.0/unyank"
```

Artifact signing is supported on both module and provider manifest uploads — attach `X-Artifact-Signature` (base64) and `X-Signature-Type` headers. The signature is stored and returned on every artifact download or provider download-info response.

See [`docs/guide/configuration.md § Registry modes`](docs/guide/configuration.md#registry-modes) for the full reference including hybrid mode and client-side setup.

### Composer (private PHP package registry)

```toml
[[registries]]
type   = "composer"
name   = "internal-composer"
mode   = "local"   # or "hybrid" to fall through to Packagist

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

Point Composer clients at the proxy by adding a repository entry in `composer.json`:

```json
{
  "repositories": [
    {
      "type": "composer",
      "url": "https://batlehub.example.com/proxy/internal-composer/",
      "options": {
        "http": {
          "header": ["Authorization: Bearer <your-token>"]
        }
      }
    }
  ]
}
```

Alternatively, store credentials in `auth.json` (never committed to VCS):

```json
{
  "http-basic": {
    "batlehub.example.com": {
      "username": "user",
      "password": "<your-token>"
    }
  }
}
```

Publish a package by uploading a ZIP archive containing a valid `composer.json` at its root (or inside a single top-level directory, matching the GitHub archive layout):

```sh
# Create the ZIP (must contain composer.json with "name" and "version" fields)
zip -r my-vendor-my-package-1.0.0.zip my-vendor-my-package-1.0.0/

# Upload
curl -X POST \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/zip" \
  --data-binary @my-vendor-my-package-1.0.0.zip \
  "https://batlehub.example.com/proxy/internal-composer/api/upload"
```

Yank a version (hides it from listings; download returns 404):

```sh
curl -X DELETE \
  -H "Authorization: Bearer <token>" \
  "https://batlehub.example.com/proxy/internal-composer/api/packages/my-vendor/my-package/versions/1.0.0"
```

### PyPI (private Python package registry)

```toml
[[registries]]
type = "pypi"
name = "internal-pypi"
mode = "local"   # or "hybrid" to fall through to pypi.org

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

Point pip and other tools at the proxy:

```ini
# ~/.pip/pip.conf
[global]
index-url = https://batlehub.example.com/proxy/internal-pypi/simple/
```

```toml
# pyproject.toml — uv
[[tool.uv.index]]
name    = "batlehub"
url     = "https://batlehub.example.com/proxy/internal-pypi/simple/"
default = true
```

Publish with twine:

```sh
python -m build   # produces dist/*.whl and dist/*.tar.gz

twine upload \
  --repository-url https://batlehub.example.com/proxy/internal-pypi/legacy/ \
  --username __token__ \
  --password $BATLEHUB_TOKEN \
  dist/*
```

The wheel filename is parsed automatically to extract name and version — no extra metadata flags needed. In `hybrid` mode, packages not found locally fall through to PyPI and are cached on the way back.

### Conda (private conda channel)

```toml
[[registries]]
type = "conda"
name = "internal-conda"
mode = "local"   # or "hybrid" to fall through to conda-forge / anaconda
# upstreams defaults to ["https://conda.anaconda.org"]

[registries.rbac]
user  = ["releases:read", "source:read"]
admin = ["*"]
```

Point conda at the proxy:

```yaml
# ~/.condarc
channels:
  - https://batlehub.example.com/proxy/internal-conda
  - nodefaults
```

Publish a package (`.tar.bz2` or `.conda` format):

```sh
# Build with conda-build
conda build my-recipe/

# Upload — specify the target platform in the URL
curl -X POST \
  -H "Authorization: Bearer $BATLEHUB_TOKEN" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @my-pkg-1.0.0-py311h0_0.tar.bz2 \
  "https://batlehub.example.com/proxy/internal-conda/linux-64/"
```

Metadata (`name`, `version`, `build`, `depends`) is extracted automatically from `info/index.json` inside the archive. The channel's `repodata.json` is updated immediately and is served merged with upstream in `hybrid` mode.

---

## Private registry — advanced features

These features apply to all registry types in `local` or `hybrid` mode.

### Ownership & team management

The first user to publish a package automatically becomes its admin. Subsequent publishes require the caller to be a registered owner. Owners can be users or groups with `admin` or `maintainer` roles.

```sh
# List owners
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/my-pkg/owners

# Add a group owner
curl -X POST -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"principal_type":"group","principal_id":"oidc:backend-team","role":"maintainer"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/my-pkg/owners

# Remove an owner
curl -X DELETE -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/my-pkg/owners/user/alice
```

### Team namespaces & package visibility

Team namespaces let an auth-provider group (from OIDC claims or Kubernetes) claim a package name prefix within a registry. Only members of that group — plus admins — may publish packages whose name starts with the claimed prefix. Groups are not managed inside BatleHub; membership is read from the `groups` claim delivered by the auth provider on every request.

Package visibility controls who can **download** a package, independently of who can publish it:

| Visibility | Who can download |
|------------|-----------------|
| `public` (default) | Everyone, including unauthenticated users |
| `internal` | Any authenticated user |
| `team` | Only members of the group that owns the namespace |

Visibility is package-level (all versions share the same setting). Changing visibility takes effect immediately on the next request; no cache flush is required. Publishing a new version inherits the existing package visibility automatically.

```sh
# Claim the "frontend" namespace for the group "oidc:frontend-team"
curl -X POST -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"prefix":"frontend","group_id":"oidc:frontend-team"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces

# Only members of "oidc:frontend-team" can now publish "frontend/utils", "frontend/ui", etc.

# List namespace claims for a registry
curl -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces

# Set a package to team-only visibility
curl -X PUT -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"visibility":"team"}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/packages/frontend%2Futils/visibility

# Release the namespace claim
curl -X DELETE -H "Authorization: Bearer <admin-token>" \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/namespaces/frontend
```

### Versioning policies

Enforce versioning rules at publish time — violations return HTTP 422.

```toml
[[registries]]
type = "cargo"
name = "internal"
mode = "local"

[registries.versioning]
enforce_semver   = true   # reject non-semver versions
allow_prerelease = false  # reject pre-release versions (e.g. 1.0.0-beta.1)
# version_pattern = "^\\d+\\.\\d+\\.\\d+$"  # optional regex
```

### Artifact signing

Attach a signature to any publish; BatleHub stores it and returns it on every download.

```sh
# Publish with a signature
SIGNATURE=$(gpg --detach-sign --armor artifact.tgz | base64 -w0)
curl -X PUT -H "Authorization: Bearer <token>" \
  -H "X-Artifact-Signature: $SIGNATURE" \
  -H "X-Signature-Type: pgp" \
  --data-binary @artifact.tgz \
  "https://batlehub.example.com/proxy/internal/..."

# Download — response includes the stored headers:
#   X-Artifact-Signature: <base64>
#   X-Signature-Type: pgp
```

Optionally require signatures for all publishes:

```toml
[registries.signing]
required      = true
allowed_types = ["pgp", "ed25519"]
```

### Bulk operations

Yank, unyank, or permanently delete many versions in one admin API call.

```sh
curl -X POST -H "Authorization: Bearer <admin-token>" \
  -H "Content-Type: application/json" \
  -d '{"packages":[{"name":"my-pkg","version":"1.0.0"},{"name":"my-pkg","version":"1.0.1"}]}' \
  https://batlehub.example.com/api/v1/admin/registries/internal-npm/bulk-yank
```

Endpoints: `bulk-yank`, `bulk-unyank`, `bulk-delete`. Response includes `processed`, `succeeded`, and a `failed` list with per-item errors.

### Publish quota

Limit how much each user can publish.

```toml
[registries.quota]
max_storage_bytes_per_user = 1073741824   # 1 GiB
max_packages_per_user      = 500
enforcement                = "block"      # or "warn"
```

Quota state is returned on every publish via `X-Quota-Storage-Used`, `X-Quota-Storage-Limit`, `X-Quota-Packages-Used`, `X-Quota-Packages-Limit`, and `X-Quota-Warning` headers.

---

## Client tool configuration

The built-in **Setup Guide** page (`/setup`) generates ready-to-paste config snippets for all supported tools. The snippets below are illustrative; the UI generates them pre-filled with your server's actual address.

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

```
config.toml
  └─ [[registries]]  type = "npm" | "cargo" | "github" | "openvsx" | "vscode-marketplace"
                               | "goproxy" | "maven" | "terraform" | "rubygems" | "composer"
                               | "pypi" | "conda"
         │
         ▼
server/src/main.rs         — builds registry clients, policies, services
         │
         ▼
ProxyService               — orchestrates caching, rules, streaming
  ├── resolve_metadata()   → registry adapter (fetches version info from upstream)
  ├── evaluate rules       → RBAC, block list, release age gate, deny latest
  ├── storage cache        → filesystem or S3
  └── fetch_artifact()     → registry adapter (streams bytes from upstream)
         │
         ▼
LocalRegistryService       — authoritative local/hybrid registry
  ├── publish()            → versioning check → ownership check → signing check → quota → store
  ├── yank() / unyank()
  ├── bulk_yank() / bulk_unyank() / bulk_remove_versions()
  └── get_artifact()       → storage + signature headers
         │
         ▼
HTTP handlers (actix-web)  — one module per registry type
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

## Permissions

| Permission | Meaning |
|-----------|---------|
| `releases:read` | List versions and download release assets / metadata |
| `source:read` | Download source archives (tarballs, `.crate`, `.zip`) |
| `*` | All permissions |

Role inheritance: `admin` ⊃ `user` ⊃ `anonymous`. Group permissions from OIDC or Kubernetes claims are additive on top of role permissions.

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

### Fuzzing

The `fuzz/` directory contains libFuzzer targets for the most security-sensitive code paths:

| Target | What it covers |
|--------|---------------|
| `fuzz_rbac_evaluate` | RBAC group-string parsing (colon-based provider prefix splitting) |
| `fuzz_package_id_cache_key` | `PackageId::cache_key()` with arbitrary registry/name/version strings |
| `fuzz_deny_latest` | Version-string comparison — verifies only exact `"latest"` is blocked |
| `fuzz_release_age` | Chrono timestamp arithmetic with arbitrary past/future dates |

Requires a nightly Rust toolchain and `cargo-fuzz`:

```sh
rustup install nightly
cargo install cargo-fuzz

# Run a specific target (30 s by default)
task fuzz TARGET=fuzz_deny_latest

# Run longer or with a different target
task fuzz TARGET=fuzz_rbac_evaluate MAX_TIME=300
```

Corpus and crash inputs are saved under `fuzz/corpus/<target>/` and `fuzz/artifacts/<target>/` respectively.

### Adding a new registry type

See [`docs/contributing/adding-a-registry.md`](docs/contributing/adding-a-registry.md) for a step-by-step guide with code templates.

---

## Deployment

### Pre-built image and binary

Every tagged release publishes:

- A **multi-arch container image** (`linux/amd64` + `linux/arm64`) to the GitHub Container Registry:
  ```sh
  docker pull ghcr.io/batleforc/batlehub:<version>
  ```
- A **statically linked server binary** attached to the GitHub Release page.

### Docker image (build from source)

```sh
docker build -t batlehub .
docker run -p 8080:8080 \
  -v /path/to/config.toml:/etc/batlehub/config.toml \
  -v /path/to/cache:/var/cache/batlehub \
  batlehub
```

### Helm chart

A Helm chart is available in `helm/batlehub/` for Kubernetes deployments:

```sh
helm install batlehub ./helm/batlehub \
  --namespace batlehub --create-namespace \
  --set database.url="postgresql://batlehub:changeme@postgres:5432/batlehub" \
  --set "auth.tokens[0].value=my-admin-token" \
  --set "auth.tokens[0].role=admin"
```

See [`docs/guide/installation.md`](docs/guide/installation.md) for the full Helm reference including values, S3 storage, and GitOps patterns.

The image uses a multi-stage build (Rust builder → Node UI builder → Debian slim runtime). The compiled binary and built SPA are copied into the final stage.

### Environment variable overrides

Key settings can be overridden at runtime without editing the config file:

| Variable | Config field |
|----------|-------------|
| `PROXY_CACHE__SERVER__PORT` | `server.port` |
| `PROXY_CACHE__DATABASE__URL` | `database.url` |
| `PROXY_CACHE__STORAGE__PATH` | `storage.path` (single filesystem backend) |
| `PROXY_CACHE__OTEL__ENDPOINT` | `otel.endpoint` |

Full list in [`docs/guide/configuration.md § Environment Variable Overrides`](docs/guide/configuration.md#_5-environment-variable-overrides).

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

## Roadmap

See [`ROADMAP.md`](ROADMAP.md) for the full list of planned features, or browse the [Roadmap page on the documentation site](docs/guide/roadmap.md).

## Contributing, security, and license

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — contributor guide (points to [`docs/contributing/contributing.md`](docs/contributing/contributing.md))
- [`SECURITY.md`](SECURITY.md) — supported versions and how to report a vulnerability
- [`LICENSE`](LICENSE) — Apache License 2.0

## IA and its role in the project

BatleHub is a solodev that cost me many white nights and a few gray hairs (not yet!!). The IA has helped me think through the design and implementation of complex features, debug tricky issues and write doc. Most of the time it did the job of reviewing my code and make sure that i wasn't going to far from the core design. I also used it to generate documentation and examples, which saved me a lot of time and made the docs more consistent. Overall, the IA has been an invaluable tool for me in this project, and I can't imagine doing it this fast without it. Understanding how some registry work has been a nightmare, and the future registry to come will be even more work, but has the wireframes and the base design is in place, working on new registry is more a matter of copy-pasting and tweaking the existing code to cover any crazy singularity of the new registry.
