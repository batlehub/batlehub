//! The npm CLI surface that was not a package read.
//!
//! RFC 0009 §7.1 (rest). `npm whoami`, `npm ping` and `npm dist-tag` had no
//! routes — and the first two did not fail cleanly. They were eaten by the
//! three-segment catch-all `/proxy/{registry}/{package}/{version}` and answered
//! **`200 OK` with an npm version document**: `whoami` taken for package `-`
//! at version `whoami`. A wrong answer under a success status, which nothing
//! downstream can detect. `protocol_conformance.rs` found it on its first run
//! and pinned it until this phase.
//!
//! ## dist-tags is a listing
//!
//! `dist-tags` maps tag → version, so a tag naming a blocked version hands the
//! client a version it cannot have — the same failure a packument listing a
//! blocked version causes, in three lines of JSON instead of three hundred.
//!
//! It is not filtered *here*, though: `npm_dist_tags` reads the map out of the
//! packument, which `blocking::npm::strip_packument` has already repaired
//! against the blocked set. One filter, two documents, and no way for them to
//! disagree — a second filter over the same facts is a second thing to keep in
//! step.

use std::sync::Arc;

use actix_web::{delete, get, put, web, HttpRequest, HttpResponse, Responder};

use batlehub_core::entities::PackageId;
use batlehub_core::services::{LocalRegistryService, ProxyService};

use super::require_npm;
use crate::handlers::proxy::common::{local_or_proxy_document_value, registry_public_base};
use crate::handlers::schemas::ProtocolDocument;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

/// `npm ping`.
///
/// npm treats any `200` with a JSON body as a healthy registry. It is the first
/// call `npm doctor` makes, and it used to be answered by the package handler.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/-/ping",
    tag = "proxy/npm",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Registry is reachable", body = ProtocolDocument),
        (status = 404, description = "Unknown or non-npm registry"),
    ),
)]
#[get("/proxy/{registry}/-/ping")]
pub async fn npm_ping(
    path: web::Path<String>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_npm(&registry, &map)?;
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(serde_json::json!({})))
}

/// `npm whoami`.
///
/// Answers from the identity the auth middleware already resolved, so it
/// reports who BatleHub thinks you are rather than who the upstream would —
/// which is the useful answer when the token is BatleHub's.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/-/whoami",
    tag = "proxy/npm",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "The authenticated username", body = ProtocolDocument),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown or non-npm registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/-/whoami")]
pub async fn npm_whoami(
    path: web::Path<String>,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_npm(&registry, &map)?;

    // npm prints "username" from this field and errors when it is absent, so an
    // anonymous caller gets a 401 rather than a `200` naming nobody.
    let username = identity.0.user_id.clone().ok_or_else(|| {
        AppError::unauthorized("not authenticated: `npm whoami` needs a token".to_owned())
    })?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(serde_json::json!({ "username": username })))
}

/// `npm dist-tag ls`.
///
/// Filtered: see the module docs. A tag naming a blocked version is repaired
/// onto the newest allowed release rather than served, because a tag is a
/// promise that the version it names can be installed.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/-/package/{package}/dist-tags",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package name"),
    ),
    responses(
        (status = 200, description = "Tag to version map", body = ProtocolDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or package"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/-/package/{package}/dist-tags")]
pub async fn npm_dist_tags(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, package) = path.into_inner();
    require_npm(&registry, &map)?;
    batlehub_core::services::validate_package_name(&package)
        .map_err(|e| AppError::bad_request(format!("invalid package name: {e}")))?;

    // Read from the *filtered* packument rather than from a separate upstream
    // call: the packument's `dist-tags` is already repaired against the blocked
    // set, so taking it from there is what makes the two documents agree by
    // construction instead of by a second filter that could drift.
    //
    // Through the same mode ladder `get_packument` uses, and for the same
    // reason: a local registry's packages exist in the database, not upstream.
    // Reading the document straight off `ProxyService` — which is what this
    // handler used to do — asked npmjs.org about a package published here, so
    // `npm dist-tag ls` answered `404` for a package `npm view` had described a
    // second earlier (RFC 0009 §12.16).
    let public_base = registry_public_base(&req, &registry);
    let pkg = PackageId::new(&registry, &package, "latest");
    let (fetch_registry, fetch_package, base) =
        (registry.clone(), package.clone(), public_base.clone());
    let doc = local_or_proxy_document_value(
        &svc,
        &mode_map,
        &registry,
        identity,
        move |identity: batlehub_core::entities::Identity| async move {
            local_svc
                .get_npm_packument(&fetch_registry, &fetch_package, &base, &identity)
                .await
        },
        format!("package '{package}' not found"),
        pkg,
        Action::ReleasesRead,
        batlehub_core::ports::DocumentKind::Versions,
        public_base,
    )
    .await?;

    let tags = doc
        .get("dist-tags")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(tags))
}

/// `npm dist-tag add` — declined, with a reason the client prints.
///
/// BatleHub *derives* `dist-tags` from the published version set: `latest` is
/// the newest allowed release, recomputed on every read so a block moves it
/// immediately (RFC 0006). A stored tag would be overwritten by the next
/// request, and `npm dist-tag ls` would then report something the client never
/// set — so accepting the write would be a `200` that does not hold.
///
/// `501` rather than `404`: the route exists and the request is well-formed,
/// and the difference tells a client to stop rather than to look elsewhere.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/-/package/{package}/dist-tags/{tag}",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package name"),
        ("tag"      = String, Path, description = "Tag name"),
    ),
    request_body(content_type = "application/json", description = "The version to tag, as a JSON string"),
    responses(
        (status = 404, description = "Unknown registry"),
        (status = 501, description = "Tag writes are not implemented for this registry"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/-/package/{package}/dist-tags/{tag}")]
pub async fn npm_dist_tag_add(
    path: web::Path<(String, String, String)>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    let (registry, package, tag) = path.into_inner();
    require_npm(&registry, &map)?;

    Err(AppError::not_implemented(format!(
        "dist-tags are derived from the published version set on this registry, so \
         '{tag}' cannot be set for '{package}' independently; publish the version \
         you want tagged as `latest`"
    )))
}

/// `npm dist-tag rm`. Declined for the same reason as `add`.
#[utoipa::path(
    delete,
    path = "/proxy/{registry}/-/package/{package}/dist-tags/{tag}",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package name"),
        ("tag"      = String, Path, description = "Tag name"),
    ),
    responses(
        (status = 404, description = "Unknown registry"),
        (status = 501, description = "Tag writes are not implemented for this registry"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/proxy/{registry}/-/package/{package}/dist-tags/{tag}")]
pub async fn npm_dist_tag_remove(
    path: web::Path<(String, String, String)>,
    map: web::Data<RegistryMap>,
) -> Result<HttpResponse, AppError> {
    let (registry, package, tag) = path.into_inner();
    require_npm(&registry, &map)?;
    Err(AppError::not_implemented(format!(
        "dist-tags are derived from the published version set on this registry, so \
         '{tag}' cannot be removed from '{package}' independently"
    )))
}
