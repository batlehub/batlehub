//! Forgejo/Gitea-specific handlers. Release endpoints are shared with the GitHub
//! handlers (identical URL scheme); this module adds the package-registry
//! passthrough, which is Forgejo-only.

use std::sync::Arc;

use actix_web::{get, web, Responder};

use batlehub_core::{entities::PackageId, services::ProxyService};

use super::common::proxy_stream;
use crate::handlers::schemas::ArtifactBytes;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};
use batlehub_core::entities::Action;

fn require_forgejo(registry: &str, map: &RegistryMap) -> Result<(), AppError> {
    match map.type_of(registry).as_deref() {
        Some("forgejo") => Ok(()),
        Some(_) => Err(AppError::not_found(format!(
            "registry '{registry}' is not a forgejo registry"
        ))),
        None => Err(AppError::not_found(format!(
            "unknown registry '{registry}'"
        ))),
    }
}

/// Proxy a Forgejo/Gitea package-registry path (`/api/packages/{owner}/…`).
///
/// Transparent passthrough/cache of the Forgejo Packages API — ideal for the
/// **generic** package registry (immutable file downloads). Ecosystem registries
/// (npm, Maven, PyPI, …) are better served by the matching typed adapter pointed at
/// the package endpoint, which rewrites metadata URLs.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/packages/{path}",
    tag = "proxy/forgejo",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path" = String, Path, description = "Path under /api/packages/ (e.g. {owner}/generic/{name}/{version}/{file})"),
    ),
    responses(
        (status = 200, description = "Package file", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 404, description = "Not found or unknown registry"),
        (status = 403, description = "Access denied"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/packages/{path:.*}")]
pub async fn fj_packages(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, api_path) = path.into_inner();
    require_forgejo(&registry, &map)?;
    batlehub_core::services::validate_path_safe("path", &api_path).map_err(AppError::from)?;
    let pkg = PackageId::new(&registry, "_packages", "_")
        .with_artifact(format!("pkgpath/api/packages/{api_path}"));
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, None).await
}

/// Download a release asset by its Forgejo attachment uuid.
///
/// `{forge}/attachments/{uuid}` is how Forgejo addresses a release asset
/// without naming its repository, and clients build that URL themselves rather
/// than follow the release document — `mise`'s `forgejo:` backend uses it for
/// every asset it installs, and never the `browser_download_url` this proxy
/// rewrites. Without this route there is no URL such a client could be pointed
/// at, so its download goes to the forge: past the rule chain, the cache and
/// the audit trail, and in a closed world nowhere at all.
///
/// The uuid names no repository, so the coordinate is the one remembered when
/// this instance served the release document the uuid came from (see
/// [`batlehub_core::services::forge_attachments`]). A uuid nothing was
/// remembered for is a `404`: this is a route onto what this instance has
/// served, not an opaque relay for the forge's attachment space.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/attachments/{uuid}",
    tag = "proxy/forgejo",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("uuid" = String, Path, description = "Forgejo attachment uuid, as the release document gave it"),
    ),
    responses(
        (status = 200, description = "Asset binary stream", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry, or an attachment this instance has not served a release document for"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/attachments/{uuid}")]
pub async fn fj_attachment(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, uuid) = path.into_inner();
    require_forgejo(&registry, &map)?;
    let Some(pkg) =
        batlehub_core::services::forge_attachments::resolve(svc.cache.as_ref(), &registry, &uuid)
            .await
    else {
        return Err(AppError::not_found(format!(
            "attachment '{uuid}' is not one this registry has served a release document for; \
             read the release through this proxy first"
        )));
    };
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, None).await
}
