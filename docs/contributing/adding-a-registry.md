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
- [ ] `tests/heavy/closed_world.sh` — a phase, its `PHASES` entry, a registry in
      `tests/heavy/config.closed-world.toml`, and a matrix row in
      `.github/workflows/test.yaml` *(the live proof — see §11)*
- [ ] An air-gap proof — a case in `crates/web/tests/air_gap.rs`, or a phase in
      `tests/heavy/airgap.sh` if a real client can drive it *(see §11)*
- [ ] `tests/heavy/authz.sh` — a client phase, hermetic if the kind has a local
      mode and `live:<kind>` if it does not *(see §11)*
- [ ] The soak — a protocol module in `perf/mock-upstream/src/protocols/`, a
      registry in `perf/config.soak.toml` and at least one arm in
      `perf/k6/soak_arms.js` *(see §11; `crates/web/tests/soak_kind_coverage.rs`
      fails until this is done or the kind is written into `NOT_SOAKED`)*

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

### The two heavy proofs every kind owes: air-gapped, and live

A unit test says the adapter parses what the upstream sends, and an integration
test says the route is wired. Neither says the thing an operator actually needs
to know, and both have been green while a kind was unusable: the ovsx download
URL pointed at a route no Open VSX client asks for, the GitLab client refused
the document its own typed route requests, and the marketplace's numeric-id
spelling — the only one an IDE ever learns — resolved nothing. Every one of
those was found by a client, not by a test double.

So a new kind is not finished until it has been driven **both ways**.

**Live.** A real client, against the real upstream, with the client unable to
reach anything but this instance — `tests/heavy/closed_world.sh`. One phase per
kind, each proving the same sentence: egress is denied, the dependency comes
from the instance, it builds, and what it built runs. Four pieces:

```bash
# 1. tests/heavy/closed_world.sh — phase_<kind>, and the name in PHASES
# 2. tests/heavy/config.closed-world.toml — an [[registries]] block for it
# 3. .github/workflows/test.yaml — a `- phase: <kind>` row under heavy-closed-world
#    (plus a setup step there if the client is not on the runner image)
bash tests/heavy/closed_world.sh <kind>      # run just yours
```

Assert on the **wire transcript**, not only on the client's exit code: a phase
that passes because the client quietly reached the upstream proves nothing, and
`heavy_wire_re_after` is what makes the difference visible. Where the kind has
no local mode, it also needs `live:<kind>` in `tests/heavy/authz.sh`
(`AUTHZ_LIVE_KINDS` + a registry in `config.authz-live.toml`): a credential
boundary's *positive* arm cannot be observed against an empty registry, because
the allowed caller has to actually succeed. Where it does have a local mode, the
hermetic client phase in `authz.sh` covers it instead.

**Air-gapped.** The same kind on an instance that can reach nothing, holding
only what was bundled into it (RFC 0008 / 0008-bis). Writing the case is half
of it: `registry_kind_coverage.rs` reads `air_gap.rs` and fails if a kind its
labs drive still declares `AirGap::Gap`, so the row and the case cannot drift
apart. They did once — eight kinds had a case and declared a gap, and the
published count said 8 of 25 when the truth was 16. This is where a kind
discovers that its client resolves through a *listing* it was never given, which
is a different failure from "the artifact is missing" and has a different fix —
`synthesise_listings`, and the recorded miss that tells the next bundle what to
carry. Add a case to `crates/web/tests/air_gap.rs`; if a real client can drive
the kind end to end, add a phase to `tests/heavy/airgap.sh` too and read the
answer off the wire the way the npm, pip and mise phases do.

Both suites need `DATABASE_URL`, and the live one needs network *for the server*
— the client is the half that gets none.

**Write the suite, then run it, then believe the kind works — in that order.**
`apk` landed its local mode with 43 green tests and a heavy suite that had never
been executed. The first run found five defects in under an hour, three of them
in the server, and together they meant every repository the feature could host
was uninstallable ([RFC 0026](/rfc/0026-alpine-apk) §13). They were invisible
from inside because the tests used **fixtures built by the same hand as the
code**: our reader walks an archive's gzip members independently, so a test
double written the same way could not show that the real format is one tar
stream across those members — and a client refused it on the first byte.

Two habits fall out of that, and they cost nothing:

- **Build one fixture with the client's own tooling** and compare. `apk index`
  produced the reference index that showed our `C:` field was computed by the
  wrong rule — a defect whose only symptom is the client downloading a package
  and *then* rejecting it.
- **Assert on what the client resolved, not on its exit code.** apk 3 reports a
  repository it could not read as `N unavailable` and exits `0`; a suite
  checking `$?` was green against a proxy that served nothing.

### The soak owes a kind an arm too

A kind that nothing drives under constant load is a kind whose client, parser
and rewriter have never been asked to run for an hour, and
`crates/web/tests/soak_kind_coverage.rs` fails on the next `cargo test` until
that is fixed or written down. Three small pieces:

1. **`perf/mock-upstream/src/protocols/<name>.rs`** — the upstream. What it owes
   is narrow: the documents the *proxy's* client parses, every digest the format
   names computed from the bytes that will be served (the proxy verifies them),
   and the same answer for the same coordinate every time. It is not a registry
   a real client could install from — that is `closed_world.sh`. Declare the
   routes `#[route(..., method = "GET", method = "HEAD")]`: actix does not
   derive `HEAD` from `#[get]`, and some clients ask before they stream.
2. **`perf/config.soak.toml`** — a `[[registries]]` block pointed at the mock.
3. **`perf/k6/soak_arms.js`** — one arm per request shape worth loading,
   typically a listing and an artifact. Give the arm a bounded `space`: a
   coordinate space that grows with the run adds a row and a stored object per
   request forever, and a verdict cannot tell that from a leak.

Then run the pre-flight, which is the part that tells you the truth:

```bash
task perf:soak PROFILE=debug DURATION=30s RATE=20
```

It asks for every arm once before the load and stops on any that does not answer
the status it declares. Do not skip it and read the load's own result instead:
that check is "not 5xx", which a `404` passes.

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
