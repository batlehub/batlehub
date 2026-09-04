# Adding a New Registry

This guide walks through every change needed to wire a new upstream registry into batlehub. The OpenVSX adapter (`crates/adapters/src/registry/openvsx.rs`) is used as the reference implementation throughout.

## 1. Architecture Overview

```
config.toml
  └─ type = "myregistry"
       │
       ▼
server/src/main.rs            instantiates MyRegistryClient
       │
       ▼
crates/adapters/
  └─ registry/myregistry.rs   implements RegistryClient trait
       │                         resolve_metadata() → PackageMetadata
       │                         fetch_artifact()   → ArtifactStream
       ▼
crates/core/
  └─ services/proxy.rs        orchestrates caching, rules, streaming
       │
       ▼
crates/web/
  └─ handlers/proxy/          HTTP routes that build PackageId and call ProxyService
```

Every request goes through `ProxyService::handle()`, which:
1. Calls `resolve_metadata` to get version info (used for rules evaluation and in-memory caching).
2. Evaluates RBAC, block-list, and any configured rules.
3. Checks the artifact storage cache; on a miss, calls `fetch_artifact` and caches the result.
4. Returns the byte stream to the HTTP handler.

---

## 2. Checklist

- [ ] `crates/adapters/src/registry/myregistry.rs` — adapter struct + `RegistryClient` impl
- [ ] `crates/adapters/src/registry/mod.rs` — `pub mod` + `pub use`
- [ ] `crates/adapters/Cargo.toml` — feature declaration + default
- [ ] `crates/core/src/entities/registry_kind.rs` — add the variant to `RegistryKind` and its `ALL` slice
- [ ] `server/src/main.rs` — import + `make_one` arm + `urls` arm
- [ ] `crates/web/src/handlers/proxy/myregistry.rs` — HTTP handler(s) *(if needed)*
- [ ] `crates/web/src/handlers/proxy/mod.rs` — `pub mod`
- [ ] `crates/web/src/lib.rs` — import handler, register route(s), update `ApiDoc` tags
- [ ] `ui/src/config/registryTypes.ts` — add a `RegistryTypeDef` entry

---

## 3. Step 1 — Implement the adapter

Create `crates/adapters/src/registry/myregistry.rs`. Implement the two required methods of `RegistryClient`.

```rust
use async_trait::async_trait;
use futures::TryStreamExt;
use serde::Deserialize;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{ArtifactStream, RegistryClient},
};

pub struct MyRegistryClient {
    http: reqwest::Client,
    base_url: String,
}

impl MyRegistryClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("batlehub/0.1")
            .build()
            .expect("failed to build MyRegistry HTTP client");
        Self { http, base_url: base_url.into() }
    }
}

// ── Serde types (mirror the upstream API response) ────────────────────────────

#[derive(Deserialize)]
struct MyPackage { /* ... */ }

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for MyRegistryClient {
    fn registry_type(&self) -> &str {
        "myregistry"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        // 1. Fetch upstream metadata (with "latest" resolution if needed).
        // 2. Populate `published_at` — required for the release_age_gate rule.
        // 3. Populate `is_signed`   — required for the require_signed_release rule.
        // 4. Set `download_url` only when pkg.artifact matches the relevant artifact type.
        // 5. Store registry-specific fields in `extra` as a JSON value.
        todo!()
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<ArtifactStream, CoreError> {
        // Fetch and stream the artifact bytes from upstream.
        todo!()
    }
}
```

### PackageId conventions

`PackageId` ties together the registry name, package identifier, version, and optional artifact discriminator. Choose conventions that map cleanly to the upstream API.

| Field | Example values | Notes |
|---|---|---|
| `registry` | `"myregistry"` | Set by the proxy from the request URL |
| `name` | `"my-package"` | Whatever uniquely identifies the package |
| `version` | `"1.2.3"`, `"latest"` | Resolve `"latest"` inside the adapter |
| `artifact` | `None`, `Some("tarball")` | Use `None` for metadata-only; check this in `resolve_metadata` when deciding whether to populate `download_url` |

`pkg.cache_key()` produces `"{registry}/{name}/{version}"` (no artifact) or `"{registry}/{name}/{version}/{artifact}"` (with artifact). These are the storage keys. Keep the conventions stable — changing them invalidates cached artifacts.

### Error handling

Return `CoreError::NotFound` for 404s (enables fanout fallback to the next upstream). Return `CoreError::Registry` for all other upstream errors.

```rust
if resp.status() == reqwest::StatusCode::NOT_FOUND {
    return Err(CoreError::NotFound(format!("package {} not found", pkg.name)));
}

resp.error_for_status()
    .map_err(|e| CoreError::Registry(e.to_string()))?
    .json::<MyPackage>()
    .await
    .map_err(|e| CoreError::Registry(e.to_string()))
```

---

## 4. Step 2 — Export the adapter

Add the module behind a Cargo feature flag in `crates/adapters/src/registry/mod.rs`:

```rust
#[cfg(feature = "registry-myregistry")]
pub mod myregistry;
#[cfg(feature = "registry-myregistry")]
pub use myregistry::MyRegistryClient;
```

---

## 5. Step 3 — Add a Cargo feature flag

In `crates/adapters/Cargo.toml`, declare the feature and enable it by default:

```toml
[features]
default = [
    ...,
    "registry-myregistry",   # add here
]
...
registry-myregistry = []     # add here alongside registry-npm, registry-cargo, etc.
```

If the adapter needs extra dependencies, list them as optional in `[dependencies]` and reference them from the feature:

```toml
[features]
registry-myregistry = ["dep:some-crate"]

[dependencies]
some-crate = { version = "1", optional = true }
```

---

## 6. Step 4 — Register the type in config validation

`crates/config/src/schema/mod.rs`'s `AppConfig::validate()` rejects unknown registry types at startup — but it does so generically, by parsing the configured string into `RegistryKind` (`registry.registry_type.parse::<RegistryKind>()?`). There's no per-type string list to edit here: add the new variant to the `RegistryKind` enum and its `ALL` slice in `crates/core/src/entities/registry_kind.rs`, and this validation (plus anything else that matches on `RegistryKind`, like `server/src/builders.rs`'s client-construction match) picks it up automatically — the compiler will point you at every match that needs a new arm.

Four of those matches are **exhaustive on purpose**, with no wildcard arm, because each one is generated into a published table and a table that claims coverage dispatch cannot deliver is the failure RFC 0009 was written about:

| Accessor | Answers | Appears in |
| --- | --- | --- |
| `listing_filter()` | how a version listing is filtered | the [listing-filter table](/registries/#version-listings) |
| `readme_support()` | where this kind's README comes from | the [README support table](/registries/#readmes) |
| `upstream_detail()` | whether the console may ask upstream about a package held nowhere here | the same table's *Held nowhere here* column |
| `fetchable_by_version()` | whether *Fetch this version* has a single meaning | the same table's *Fetchable* column |

Each `None` variant carries the **reason** as a `&'static str`, and the endpoint, the config warning and the generated table all quote it — so there is one sentence about why a kind does not do something, not three that can drift apart. Write the reason for a reader who is looking for a gap, not for a compiler.

---

## 7. Step 5 — Wire up the server

`server/src/main.rs` — two changes inside `build_registry_client()`.

**Import the client:**

```rust
use batlehub_adapters::registry::{
    ...,
    MyRegistryClient,
};
```

**Add an arm to `make_one`** (instantiation) and **`urls`** (default upstream):

```rust
fn make_one(registry_type: &str, url: &str) -> Arc<dyn RegistryClient> {
    match registry_type {
        "github"      => Arc::new(GithubRegistryClient::new(url, None)),
        "npm"         => Arc::new(NpmRegistryClient::new(url)),
        "cargo"       => Arc::new(CargoRegistryClient::new(url)),
        "openvsx"     => Arc::new(OpenVsxRegistryClient::new(url)),
        "myregistry"  => Arc::new(MyRegistryClient::new(url)),   // ← add
        other => panic!("registry type '{other}' is configured but no adapter is compiled in"),
    }
}

let urls = match reg.registry_type.as_str() {
    "github"     => resolve_urls(&reg.upstreams, "https://api.github.com"),
    "npm"        => resolve_urls(&reg.upstreams, "https://registry.npmjs.org"),
    "cargo"      => resolve_urls(&reg.upstreams, "https://crates.io"),
    "openvsx"    => resolve_urls(&reg.upstreams, "https://open-vsx.org"),
    "myregistry" => resolve_urls(&reg.upstreams, "https://myregistry.example.com"),  // ← add
    other => panic!("registry type '{other}' is configured but no adapter is compiled in"),
};
```

The `resolve_urls` helper returns the `upstreams` list from the config, or falls back to the default if the list is empty. When multiple upstreams are configured, a `FanoutRegistryClient` wraps them automatically.

---

## 8. Step 6 — Add HTTP handlers

Decide whether the new registry can share existing routes or needs new ones.

### Sharing existing routes (simplest)

If your registry uses the same two-part URL scheme as npm and cargo (`/proxy/{registry}/{package}` and `/proxy/{registry}/{package}/{version}`), extend the type guard in `crates/web/src/handlers/proxy/npm.rs`:

```rust
fn require_npm_or_cargo(registry: &str, map: &RegistryMap) -> Result<(), AppError> {
    match map.type_of(registry) {
        Some("npm") | Some("cargo") | Some("openvsx") | Some("myregistry") => Ok(()),
        ...
    }
}
```

### Adding a registry-specific download route

If your registry has a distinct artifact URL suffix (e.g., `.vsix`, `.whl`), create `crates/web/src/handlers/proxy/myregistry.rs`:

```rust
use std::sync::Arc;
use actix_web::{HttpResponse, Responder, get, web};
use bytes::Bytes;
use futures::StreamExt;
use batlehub_core::{entities::PackageId, services::{ProxyRequest, ProxyResponse, ProxyService}};
use crate::{RegistryMap, error::AppError, extractors::AuthIdentity};
use crate::handlers::schemas::ArtifactBytes;

pub fn require_myregistry(registry: &str, map: &RegistryMap) -> Result<(), AppError> {
    match map.type_of(registry) {
        Some("myregistry") => Ok(()),
        Some(_) => Err(AppError::not_found(format!("registry '{registry}' is not a myregistry registry"))),
        None    => Err(AppError::not_found(format!("unknown registry '{registry}'"))),
    }
}

#[utoipa::path(
    get,
    path = "/proxy/{registry}/{package}/{version}/myext",
    tag = "proxy/myregistry",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package name"),
        ("version"  = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = "Package artifact", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{package}/{version}/myext")]
pub async fn download_myext(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, package, version) = path.into_inner();
    require_myregistry(&registry, &map)?;
    let pkg = PackageId::new(&registry, &package, &version).with_artifact("myext");

    let req = ProxyRequest {
        package_id: pkg,
        identity: identity.0.clone(),
        resource_type: "source:read".to_owned(),
    };
    match svc.handle(req).await.map_err(AppError::from)? {
        ProxyResponse::Denied { reason } => Err(AppError::forbidden(reason)),
        ProxyResponse::Stream(stream) => {
            let body = stream.filter_map(|chunk| async move {
                chunk.ok().map(Ok::<Bytes, actix_web::Error>)
            });
            Ok(HttpResponse::Ok().streaming(body))
        }
    }
}
```

### Route ordering

actix-web resolves routes in registration order for patterns with equal specificity. Literal path segments take priority over parameterized ones, so `/proxy/{r}/{p}/{v}/myext` (literal `myext` suffix) routes correctly without conflicting with `/proxy/{r}/{p}/{v}/tarball` or `/proxy/{r}/{p}/{v}/vsix`. Still, **register more specific routes before less specific ones**.

---

## 9. Step 7 — Register routes and update OpenAPI

In `crates/web/src/lib.rs`:

**Add the module to the handler import:**

```rust
use handlers::proxy::{
    ...,
    myregistry::download_myext,
};
```

**Register the route in `collect_routes`** (before the shared catch-all routes):

```rust
// MyRegistry artifact download (literal "myext" suffix)
cfg.service(download_myext);
```

**Add the OpenAPI tag to `ApiDoc`:**

```rust
#[derive(OpenApi)]
#[openapi(
    tags(
        ...,
        (name = "proxy/myregistry", description = "MyRegistry proxy — package metadata and artifacts"),
    ),
    ...
)]
pub struct ApiDoc;
```

**Every `200`/`201` must declare a body**, and **the status you declare must be the status you
send**. `crates/web/tests/openapi_contract.rs` enforces both. The first walks the generated
document and fails on any success response that has only a `description` — a response with no
schema makes the generated TypeScript client emit `unknown`, and leaves the docs site's API
reference blank for that endpoint. The second reads the handler beside the annotation and fails
when the two disagree about a `2xx`: the annotation is written by hand next to a body written
separately, and a wrong number there breaks nothing until a client believes it. If your handler
answers `201`, say `201`. Point `body` at a real DTO where the handler has one;
otherwise use the shared markers in `crates/web/src/handlers/schemas.rs`:

| Marker | For |
| --- | --- |
| `ArtifactBytes` | artifact bytes streamed from storage or upstream |
| `UpstreamDocument` | a JSON document the registry protocol defines (`Vec<UpstreamDocument>` when it is a list) |
| `ProtocolDocument` | a non-JSON protocol document — XML, HTML, plain text |
| `OkResponse` / `MessageResponse` | `{"ok": true}` / `{"message": "…"}` acknowledgements |

If the handler builds an ad-hoc `json!` of its own invention, that is the finding: give it a named
struct deriving `ToSchema` in the same module and serialise *that*, so the documented schema and
the bytes on the wire come from one type.

---

## 10. Step 8 — Update the Setup Guide

`SetupGuide.vue` is fully data-driven from `ui/src/config/registryTypes.ts` — you don't touch the `.vue` file at all. Add one `RegistryTypeDef` entry to the `REGISTRY_TYPE_DEFS` array:

```ts
{
  id: "myregistry",
  label: "MyRegistry",
  fileHint: "myregistry.toml",
  description: `Replaces the upstream MyRegistry index with the proxy.`,
  snippets: [
    {
      key: "myregistry",
      lang: "bash",
      template: (ctx) => {
        const b   = ctx.base;
        const reg = ctx.registryName;
        return `# example: download a package\ncurl ${b}/proxy/${reg}/my-package/1.0.0/myext -o pkg.myext`;
      },
    },
  ],
},
```

`id` becomes the tab's value/key and, by default, the API `type` it activates for — set `apiTypes: [...]` instead when the tab should light up for more than one configured registry type (see the `mise` composite entry). `SetupGuide.vue` derives the tab trigger, tab content, registry-name input, and snippet copy button from this array automatically — see the `id: "nuget"` entry in `registryTypes.ts` for a fuller example with multiple snippets and a `note`.

---

## 11. Testing

### Unit tests for the adapter

Add tests to `myregistry.rs` using `mockito` (already a dev-dependency in `batlehub-adapters`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[tokio::test]
    async fn resolve_metadata_returns_correct_version() {
        let mut server = Server::new_async().await;
        let _mock = server.mock("GET", "/api/my-package")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"version":"1.2.3","timestamp":"2024-01-01T00:00:00Z"}"#)
            .create_async()
            .await;

        let client = MyRegistryClient::new(server.url());
        let pkg = PackageId::new("myregistry", "my-package", "latest");
        let meta = client.resolve_metadata(&pkg).await.unwrap();

        assert_eq!(meta.id.version, "1.2.3");
        assert!(meta.published_at.is_some());
    }

    #[tokio::test]
    async fn resolve_metadata_returns_not_found_for_404() {
        let mut server = Server::new_async().await;
        let _mock = server.mock("GET", "/api/unknown-package")
            .with_status(404)
            .create_async()
            .await;

        let client = MyRegistryClient::new(server.url());
        let pkg = PackageId::new("myregistry", "unknown-package", "latest");
        let result = client.resolve_metadata(&pkg).await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }
}
```

### Integration test

Add a case to the relevant file under `crates/web/tests/` (one file per feature/registry area; shared app-factory infrastructure like `FixedRegistry` and `InMemoryRepo` lives in `crates/web/tests/common/mod.rs`). Search for `proxy_npm_tarball_accessible_by_user` (in `cargo_and_downloads.rs`) as a template — the pattern is:

1. Build a `RegistryMap` with `"myregistry"` as the type.
2. Send a `TestRequest::get()` to the new URL.
3. Assert the status code and response body.

### Manual verification

Add an `[[registries]]` block with `type = "myregistry"` to a local `config.toml` and start the server:

```toml
[[registries]]
type = "myregistry"
name = "myregistry"

[registries.rbac]
anonymous = ["releases:read", "source:read"]
```

Then exercise the endpoints:

```sh
# Metadata
curl http://localhost:8080/proxy/myregistry/my-package

# Specific version
curl http://localhost:8080/proxy/myregistry/my-package/1.2.3

# Artifact download
curl http://localhost:8080/proxy/myregistry/my-package/1.2.3/myext -o output.myext
```

Verify the artifact appears in the configured storage backend after the first request, and that subsequent requests are served from cache (check the `tracing` log output for `"artifact cache hit"`).
