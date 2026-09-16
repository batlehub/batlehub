//! Generic file-mirror proxy handler.
//!
//! Some upstreams have no package protocol at all — they are a plain HTTP file
//! tree addressed by path: toolchain tarballs (`nodejs.org/dist`,
//! `static.rust-lang.org`, `dl.google.com/go`) and single-binary vendor CDNs
//! (`get.helm.sh`, `binaries.sonarsource.com`). A `generic`
//! registry mirrors one such tree: every request streams the file from upstream
//! (caching it on the first miss) via [`ProxyService`], using the shared
//! [`batlehub_adapters::registry::PathProxyRegistryClient`] — the same client
//! behind the deb/rpm/pacman/jetbrains paths.
//!
//! Two things distinguish it from `jetbrains`, and both are enforced by config
//! validation rather than here:
//!
//! - **`upstreams` is mandatory** — there is no sensible default file tree.
//! - **`path_allow` is mandatory** — the upstream path is passed straight
//!   through, so a registry pointed at a host that serves many unrelated
//!   tenants (a shared bucket, a multi-vendor CDN) would otherwise relay *every*
//!   path on that host. The allowlist is applied inside the client, so it also
//!   covers cache warming; `path_allow = ["**"]` is the explicit opt-out.
//!
//! There is no local hosting, publishing, or signing. Note that mirrored
//! archives are often large and [`ProxyService::handle`] buffers the whole
//! artifact before caching, so `limits.max_artifact_size_bytes` (default 500
//! MiB) may need raising.

use std::sync::Arc;

use actix_web::{get, web, Responder};

use batlehub_core::{entities::PackageId, services::ProxyService};

use super::common::{proxy_stream, require_registry_type};
use crate::handlers::schemas::ArtifactBytes;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};
use batlehub_core::entities::Action;

/// Serve a file from a generic upstream file tree
/// (`GET /proxy/{registry}/generic/{path}`).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/generic/{path}",
    tag = "proxy/generic",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "Upstream file path (e.g. v24.18.0/node-v24.18.0-linux-x64.tar.gz)"),
    ),
    responses(
        (status = 200, description = "Artifact streamed from upstream (cached)", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Unsafe path"),
        (status = 403, description = "Access denied, or path outside the registry's path_allow allowlist"),
        (status = 404, description = "Not found or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/generic/{path:.*}")]
pub async fn generic_get(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file_path) = path.into_inner();
    require_registry_type(&registry, "generic", &map)?;
    // Edge validation: reject traversal before the path reaches a storage key. The
    // storage backend's `ensure_safe_key` is the deeper guard.
    batlehub_core::services::validate_path_safe("path", &file_path).map_err(AppError::from)?;

    // The whole upstream path is carried in the synthetic `repo` coordinate's
    // artifact, exactly like the jetbrains and deb/rpm path-proxy fall-through;
    // RBAC (`releases:read`), the `path_allow` allowlist and caching are all
    // handled below this point (in `ProxyService::handle` and the client).
    let pkg = PackageId::new(&registry, "repo", "_").with_artifact(&file_path);
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some("application/octet-stream"),
    )
    .await
}
