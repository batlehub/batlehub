//! The v1 role surface (RFC 0031 §4.4).
//!
//! Three read endpoints, and a limit that has to be stated rather than
//! discovered: `Role.install` builds
//! `https://github.com/{github_user}/{github_repo}/archive/{version}.tar.gz`
//! *itself*, and only prefers a `download_url` when the matching entry of
//! `v1/roles/{id}/versions/` carries one. galaxy.ansible.com carries one for
//! every version — `geerlingguy.docker`, 81 versions, each pointing at
//! `github.com` — so rewriting that field routes the bytes through this
//! instance. Two cases it cannot reach, and the registry page says so:
//!
//! - a role with **no** published versions installs from its default branch,
//!   straight from GitHub;
//! - a `requirements.yml` entry with an explicit `src:` URL was never a
//!   registry request at all.
//!
//! `roles = "off"` answers `404` here *and* removes `v1` from the discovery
//! document, so the client fails on its own *"requires API versions 'v1'"*.

use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};

use batlehub_core::{
    entities::{Action, PackageId, RegistryKind},
    ports::DocumentKind,
    services::blocking::galaxy as blocking_galaxy,
    services::galaxy::{addressed, role_alias, role_key, validate_version},
    services::{validate_path_safe, ProxyService},
};

use super::super::common::{
    fetch_proxy_document, proxy_stream, registry_public_base, require_registry_type,
};
use super::{GZIP, JSON};
use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

/// Refuse the whole v1 surface when the operator turned it off, and answer with
/// how much of it is served.
async fn require_roles(
    svc: &ProxyService,
    registry: &str,
) -> Result<batlehub_core::services::galaxy::GalaxyRoleMode, AppError> {
    let mode = svc.galaxy_roles(registry).await;
    if mode.serves_v1() {
        return Ok(mode);
    }
    Err(AppError::not_found(format!(
        "registry '{registry}' does not serve the v1 role API (roles = \"off\")"
    )))
}

/// The numeric role id upstream addresses a role by, as a path segment.
///
/// Not parsed as an integer: galaxy's ids are integers today and the value only
/// ever travels back out as a path segment, so what matters is that it cannot
/// be one.
fn validate_role_id(id: &str) -> Result<(), AppError> {
    if id.is_empty() || id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(AppError::bad_request(format!("'{id}' is not a role id")));
    }
    Ok(())
}

/// Look a role up by owner and name — what `lookup_role_by_name` reads for the numeric id every later v1 request uses.
///
/// `GET …/api/v1/roles/?owner__username={user}&name={role}`
///
/// Relayed. It names no version, so it carries no filtering obligation — a role
/// with one blocked version still exists.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v1/roles/",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("owner__username" = Option<String>, Query, description = "The role's GitHub user"),
        ("name" = Option<String>, Query, description = "The role's name"),
    ),
    responses(
        (status = 200, description = "The role, if it exists", body = UpstreamDocument),
        (status = 400, description = "Not a role name"),
        (status = 404, description = "Unknown registry, or roles = \"off\""),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v1/roles/")]
pub async fn galaxy_role_search(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<std::collections::HashMap<String, String>>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    require_roles(&svc, &registry).await?;

    let (Some(user), Some(role)) = (query.get("owner__username"), query.get("name")) else {
        return Err(AppError::bad_request(
            "a role lookup needs both 'owner__username' and 'name'",
        ));
    };
    let package = role_key(user, role).map_err(AppError::from)?;
    let public_base = registry_public_base(&req, &registry);

    let doc = fetch_proxy_document(
        svc,
        PackageId::new(&registry, &package, "latest"),
        identity,
        Action::ReleasesList,
        DocumentKind::ROLE,
        public_base,
    )
    .await?;
    let body = doc.body.as_json().cloned().unwrap_or(serde_json::json!({}));
    Ok(HttpResponse::Ok().content_type(JSON).json(body))
}

/// Every allowed version of a role, as one page, each `download_url` pointed here under `roles = "proxy"`.
///
/// `GET …/api/v1/roles/{id}/versions/`
///
/// Filtered, one page, and each surviving entry's `download_url` rewritten to
/// this instance under `roles = "proxy"`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v1/roles/{id}/versions/",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id" = String, Path, description = "The numeric role id from the search above"),
    ),
    responses(
        (status = 200, description = "Every allowed version of the role, as one page", body = UpstreamDocument),
        (status = 400, description = "Not a role id"),
        (status = 404, description = "Unknown role or registry, or roles = \"off\""),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v1/roles/{id}/versions/")]
pub async fn galaxy_role_versions(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, id) = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    let mode = require_roles(&svc, &registry).await?;
    validate_role_id(&id)?;
    let public_base = registry_public_base(&req, &registry);

    // The numeric id is the upstream's address; the *name* is what a block, an
    // explore row and a grant are written on. So the alias is resolved to the
    // canonical coordinate before anything else runs — the same mechanism the
    // JetBrains Marketplace uses for its numeric update ids, and for the same
    // reason: cached and blocked under the id, a role would answer to a
    // spelling no operator ever types (§6.2).
    let package = canonical_role(&svc, &registry, &id).await?;

    let doc = fetch_proxy_document(
        svc.clone(),
        PackageId::new(&registry, addressed(&package, &id), "latest"),
        identity,
        Action::ReleasesList,
        DocumentKind::ROLE_VERSIONS,
        public_base.clone(),
    )
    .await?;
    let mut body = doc.body.as_json().cloned().unwrap_or(serde_json::json!({}));
    // Whether or not anything was blocked: a relayed `next_link` is joined
    // against a deliberately stripped `scheme://netloc/`, so no continuation
    // this instance emits could be followed back to it.
    blocking_galaxy::collapse_role_page(&mut body);

    if mode.proxies_bytes() {
        rewrite_role_downloads(&mut body, &public_base, &id);
    }
    Ok(HttpResponse::Ok().content_type(JSON).json(body))
}

/// Point every surviving entry's `download_url` at this instance.
///
/// Under `roles = "index"` this is not called and the upstream URL is relayed:
/// the metadata is proxied and the bytes are not, which is the difference the
/// `roles` table promises.
fn rewrite_role_downloads(doc: &mut serde_json::Value, public_base: &str, id: &str) {
    let Some(entries) = doc.get_mut("results").and_then(|r| r.as_array_mut()) else {
        return;
    };
    for entry in entries {
        let version = entry
            .get("version")
            .and_then(|v| v.as_str())
            .or_else(|| entry.get("name").and_then(|v| v.as_str()))
            .map(str::to_owned);
        let Some(version) = version else { continue };
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        obj.insert(
            "download_url".to_owned(),
            serde_json::json!(format!(
                "{}/galaxy/api/v1/roles/{id}/download/{version}.tar.gz",
                public_base.trim_end_matches('/')
            )),
        );
    }
}

/// A role archive, fetched server-side from the host its listing named — served only under `roles = "proxy"`.
///
/// `GET …/api/v1/roles/{id}/download/{filename}`
///
/// Only under `roles = "proxy"`. The archive is fetched server-side, through
/// the SSRF guard and only from the fixed role-download allowlist — the
/// registry's own upstream plus `github.com`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v1/roles/{id}/download/{filename}",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id" = String, Path, description = "The numeric role id"),
        ("filename" = String, Path, description = "{version}.tar.gz"),
    ),
    responses(
        (status = 200, description = "The role archive", body = ArtifactBytes, content_type = "application/gzip"),
        (status = 400, description = "Not a role id or archive name"),
        (status = 403, description = "Blocked version or access denied"),
        (status = 404, description = "Unknown role or registry, or roles is not \"proxy\""),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v1/roles/{id}/download/{filename}")]
pub async fn galaxy_role_artifact(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, id, filename) = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    let mode = require_roles(&svc, &registry).await?;
    if !mode.proxies_bytes() {
        return Err(AppError::not_found(format!(
            "registry '{registry}' proxies role metadata and not role bytes (roles = \"index\")"
        )));
    }
    validate_role_id(&id)?;
    validate_path_safe("filename", &filename).map_err(AppError::from)?;
    let version = filename
        .strip_suffix(".tar.gz")
        .ok_or_else(|| AppError::bad_request(format!("'{filename}' is not a role archive name")))?;
    validate_version(version).map_err(AppError::from)?;

    let package = canonical_role(&svc, &registry, &id).await?;

    proxy_stream(
        svc,
        PackageId::new(&registry, &package, version).with_artifact("tarball"),
        identity,
        Action::ReleasesRead,
        Some(GZIP),
    )
    .await
}

/// `roles/{user}.{role}` for the numeric id a request arrived under.
///
/// Cached by `ProxyService::canonical_coordinate`, because the mapping is a
/// fact about an upstream role that does not change and one `role install`
/// asks for it twice — once for the listing and once for the archive.
async fn canonical_role(
    svc: &web::Data<Arc<ProxyService>>,
    registry: &str,
    id: &str,
) -> Result<String, AppError> {
    let alias = PackageId::new(registry, role_alias(id), "latest");
    match svc.canonical_coordinate(&alias).await {
        Ok(Some(resolved)) => Ok(resolved.name),
        Ok(None) => Err(AppError::not_found(format!(
            "role {id} is not known to registry '{registry}'"
        ))),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Roles keep the `RegistryKind` the rest of the module uses, named here so the
/// import above is not unused when the v1 surface is compiled out.
const _: RegistryKind = RegistryKind::Galaxy;
