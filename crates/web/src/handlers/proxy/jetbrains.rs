//! JetBrains IDE-archive proxy handler.
//!
//! JetBrains IDE installers are addressed purely by file path on a download CDN
//! (e.g. `https://download.jetbrains.com/idea/idea-2026.1.tar.gz`) and have no
//! metadata API, so this is a pure proxy cache: every request streams the file
//! from upstream (caching it on the first miss) via [`ProxyService`]. There is no
//! local hosting, publishing, or signing — the upstream client is the generic
//! [`batlehub_adapters::registry::PathProxyRegistryClient`].
//!
//! **Note:** IDE archives are large (~1–1.7 GB). [`ProxyService::handle`] buffers
//! the whole artifact in memory before caching and enforces
//! `limits.max_artifact_size_bytes` (default 500 MiB), so that limit must be
//! raised for IDE archives to be cached.

use std::sync::Arc;

use actix_web::{get, web, Responder};

use batlehub_core::{entities::PackageId, services::ProxyService};

use super::common::{proxy_stream, require_registry_type};
use crate::handlers::schemas::ArtifactBytes;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};
use batlehub_core::entities::Action;

/// Serve a file from a JetBrains download repository.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/jetbrains/{path}",
    tag = "proxy/jetbrains",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "Upstream file path (e.g. idea/idea-2026.1.tar.gz)"),
    ),
    responses(
        (status = 200, description = "Artifact streamed from upstream (cached)", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Not found or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/jetbrains/{path:.*}")]
pub async fn jetbrains_get(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, file_path) = path.into_inner();
    require_registry_type(&registry, "jetbrains", &map)?;
    // Edge validation: reject traversal before the path reaches a storage key. The
    // storage backend's `ensure_safe_key` is the deeper guard.
    batlehub_core::services::validate_path_safe("path", &file_path).map_err(AppError::from)?;

    // The whole upstream path is carried in the synthetic `repo` coordinate's
    // artifact, exactly like the deb/rpm path-proxy fall-through; RBAC
    // (`releases:read`) and caching are handled inside `ProxyService::handle`.
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
