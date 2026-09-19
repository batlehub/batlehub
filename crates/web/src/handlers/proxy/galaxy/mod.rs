//! Ansible Galaxy — the collections API v3, and the v1 role surface beside it
//! (RFC 0031).
//!
//! Ten routes, and the design is in which URL is a *document*:
//!
//! - **`api/`** is composed here rather than relayed: it advertises *this*
//!   instance's API versions, and `roles = "off"` removes `v1` from it as well
//!   as from the routes, so `ansible-galaxy role install` fails on the client's
//!   own version check rather than on a `404` an operator reads as a proxy
//!   fault.
//! - The **versions list** is the enforcement chokepoint. The resolver asks for
//!   it for every candidate — a pinned requirement included — and picks from
//!   what comes back, so a version absent from it cannot be selected.
//! - The **collection document** is repaired against that list: its
//!   `highest_version` is moved off a blocked version, and its `updated_at` is
//!   served as `max(upstream, newest blocked_at)` because that field is how the
//!   client decides whether to drop its day-old copy of the listing.
//! - The **version document** answers `404` when its version is blocked, and is
//!   otherwise relayed with three fields rewritten — `download_url`, `href` and
//!   `collection.href`. `artifact.sha256`, `metadata`, `manifest`, `files` and
//!   `signatures` pass through untouched, which is the invariant of §5.2: the
//!   only fields this instance writes are the ones no checksum covers.
//! - The **tarball** is never touched. It is byte-exact or it is refused at the
//!   download gate, because the client hashes the body as it reads and compares
//!   it with `artifact.sha256` from the document above.
//!
//! Every path segment reaches a storage or cache key, so each is validated here
//! for a clean `400`; `validate_coordinate` in `ProxyService::handle` and
//! `ensure_safe_key` in the storage backends remain the deeper guards.

mod publish;
mod roles;

use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};

use batlehub_core::{
    entities::{Action, PackageId, RegistryKind},
    ports::DocumentKind,
    services::blocking::galaxy as blocking_galaxy,
    services::galaxy::{
        addressed, artifact_url, collection_package, collection_url, compose_collection,
        compose_versions, discovery, parse_artifact_filename, validate_version, version_url,
        VersionEntry,
    },
    services::{LocalRegistryService, ProxyService},
};
use serde::Serialize;
use utoipa::ToSchema;

pub use publish::{galaxy_import_task, galaxy_publish};
pub use roles::{galaxy_role_artifact, galaxy_role_search, galaxy_role_versions};

use super::common::{
    local_or_proxy_document_parts, registry_public_base, require_registry_type,
    serve_local_or_proxy_artifact, synthesised_headers, LocalOrProxyArtifactOpts,
};
use crate::handlers::schemas::ArtifactBytes;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};

/// `GET api/` — the discovery document `g_connect` reads before any action.
#[derive(Debug, Serialize, ToSchema)]
pub struct GalaxyDiscovery {
    /// API version → the path segment it is served under, relative to this
    /// URL. `v1` is absent when `roles = "off"`.
    #[schema(value_type = Object)]
    pub available_versions: serde_json::Value,
    pub description: String,
}

/// The JSON content type every v3 document is served as.
pub(super) const JSON: &str = "application/json";
/// A collection tarball. The client does not read the type — it hashes the body
/// — but a browser and a `curl -O` do.
pub(super) const GZIP: &str = "application/gzip";

/// The API versions this registry serves — read by `g_connect` before any action, and `v1` is absent when `roles = "off"`.
///
/// `GET /proxy/{registry}/galaxy/api/`
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/",
    tag = "proxy/galaxy",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "The API versions this registry serves", body = GalaxyDiscovery),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/")]
pub async fn galaxy_discovery(
    path: web::Path<String>,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    // Composed, not relayed: an upstream that serves `v1` is not a reason for
    // this instance to, and `roles = "off"` has to be visible here or the
    // client's own version check never fires (§4.4).
    Ok(HttpResponse::Ok()
        .content_type(JSON)
        .json(discovery(svc.galaxy_roles(&registry).await.serves_v1())))
}

/// Every allowed version of a collection, as one page — the chokepoint the resolver picks from.
///
/// `GET …/api/v3/collections/{namespace}/{name}/versions/`
///
/// The chokepoint. One page, always: `links` is entirely null and `meta.count`
/// describes what is served, because no continuation this instance could emit
/// survives the client's own `urljoin` under a path prefix.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Collection namespace"),
        ("name" = String, Path, description = "Collection name"),
    ),
    responses(
        (status = 200, description = "Every allowed version of the collection, as one page", body = crate::handlers::schemas::UpstreamDocument),
        (status = 400, description = "Not a collection name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown collection or registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/")]
pub async fn galaxy_versions(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name) = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    let package = collection_package(&namespace, &name).map_err(AppError::from)?;
    let public_base = registry_public_base(&req, &registry);

    let (doc, synthesised) = versions_document(
        &req,
        &registry,
        &package,
        &identity,
        &svc,
        &local_svc,
        &modes,
        &public_base,
    )
    .await?;
    let mut builder = HttpResponse::Ok();
    builder.content_type(JSON);
    synthesised_headers(&mut builder, synthesised);
    Ok(builder.json(doc))
}

/// The filtered, one-page versions listing — local rows in local mode, the
/// assembled upstream document otherwise.
///
/// Shared with the collection handler, which needs the *survivors* to repair
/// `highest_version`: the two documents have to agree about which versions
/// exist or the client chases a `highest_version` its own listing does not
/// carry.
#[allow(clippy::too_many_arguments)]
async fn versions_document(
    req: &HttpRequest,
    registry: &str,
    package: &str,
    identity: &AuthIdentity,
    svc: &web::Data<Arc<ProxyService>>,
    local_svc: &web::Data<Arc<LocalRegistryService>>,
    modes: &RegistryModeMap,
    public_base: &str,
) -> Result<(serde_json::Value, Option<u32>), AppError> {
    let _ = req;
    let (namespace, name) =
        batlehub_core::services::galaxy::parse_collection(package).map_err(AppError::from)?;
    let local = {
        let local_svc = Arc::clone(local_svc);
        let registry = registry.to_owned();
        let package = package.to_owned();
        let base = public_base.to_owned();
        let namespace = namespace.to_owned();
        let name = name.to_owned();
        move |id: batlehub_core::entities::Identity| async move {
            let rows = local_svc
                .get_galaxy_versions(&registry, &package, &id)
                .await?;
            let entries: Vec<VersionEntry> = rows
                .iter()
                .filter(|r| !r.yanked)
                .map(|r| VersionEntry {
                    version: r.version.clone(),
                    created_at: Some(
                        r.published_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    ),
                    requires_ansible: r
                        .index_metadata
                        .get("requires_ansible")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                })
                .collect();
            Ok::<_, batlehub_core::error::CoreError>(compose_versions(
                &base, &namespace, &name, &entries,
            ))
        }
    };

    local_or_proxy_document_parts(
        svc,
        modes,
        registry,
        identity.clone(),
        local,
        format!("collection '{package}' has no published versions"),
        // A listing names no version. `"latest"` is the placeholder every
        // listing route uses — a coordinate with an empty version is refused
        // at `validate_coordinate`, and nothing reads this field for a
        // document.
        PackageId::new(registry, package, "latest"),
        Action::ReleasesList,
        DocumentKind::Versions,
        public_base.to_owned(),
    )
    .await
}

/// The collection document, with `highest_version` moved off a blocked version and `updated_at` bumped past the block.
///
/// `GET …/api/v3/collections/{namespace}/{name}/`
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Collection namespace"),
        ("name" = String, Path, description = "Collection name"),
    ),
    responses(
        (status = 200, description = "The collection document, with highest_version repaired and updated_at bumped past any block", body = crate::handlers::schemas::UpstreamDocument),
        (status = 400, description = "Not a collection name"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown collection or registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/")]
pub async fn galaxy_collection(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name) = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    let package = collection_package(&namespace, &name).map_err(AppError::from)?;
    let public_base = registry_public_base(&req, &registry);

    // The survivors first: the repair below needs the list this instance
    // actually serves, not the one upstream would have.
    let (listing, _) = versions_document(
        &req,
        &registry,
        &package,
        &identity,
        &svc,
        &local_svc,
        &modes,
        &public_base,
    )
    .await?;
    let surviving: Vec<String> = listing
        .get("data")
        .or_else(|| listing.get("results"))
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("version")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let local = {
        let base = public_base.clone();
        let namespace = namespace.clone();
        let name = name.clone();
        let newest = batlehub_core::services::blocking::best_latest(&surviving);
        let updated = listing
            .get("data")
            .and_then(|v| v.as_array())
            .and_then(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.get("created_at")?.as_str())
                    .max()
                    .map(str::to_owned)
            });
        move |_id: batlehub_core::entities::Identity| async move {
            Ok::<_, batlehub_core::error::CoreError>(compose_collection(
                &base,
                &namespace,
                &name,
                newest.as_deref(),
                updated.as_deref(),
            ))
        }
    };

    let (mut doc, synthesised) = local_or_proxy_document_parts(
        &svc,
        &modes,
        &registry,
        identity.clone(),
        local,
        format!("collection '{package}' has no published versions"),
        PackageId::new(&registry, &package, "latest"),
        Action::ReleasesList,
        DocumentKind::COLLECTION,
        public_base.clone(),
    )
    .await?;

    // Two edits, both against the listing above: `highest_version` off a
    // blocked version, and `updated_at` to `max(upstream, newest blocked_at)` —
    // the field `get_collection_versions` re-reads uncached on every resolve to
    // decide whether its day-old copy of the listing is still good (§4.4).
    let blocked = svc
        .blocked_versions_for(&registry, &package, RegistryKind::Galaxy)
        .await;
    let changed_at = svc.blocked_changed_at(&registry, &package).await;
    let repair = blocking_galaxy::repair_collection(&mut doc, &blocked, &surviving, changed_at);
    if repair.highest_moved || repair.updated_at_bumped {
        tracing::debug!(
            registry = %registry,
            package = %package,
            highest_moved = repair.highest_moved,
            updated_at_bumped = repair.updated_at_bumped,
            "repaired a collection document against this instance's own listing"
        );
    }

    // `versions_url` and `href` have to name this instance whatever the source:
    // an upstream document carries upstream's, and the client follows them.
    if let Some(obj) = doc.as_object_mut() {
        obj.insert(
            "href".to_owned(),
            serde_json::json!(collection_url(&public_base, &namespace, &name)),
        );
        obj.insert(
            "versions_url".to_owned(),
            serde_json::json!(batlehub_core::services::galaxy::versions_url(
                &public_base,
                &namespace,
                &name
            )),
        );
    }

    let mut builder = HttpResponse::Ok();
    builder.content_type(JSON);
    synthesised_headers(&mut builder, synthesised);
    Ok(builder.json(doc))
}

/// One version's document — `download_url` pointed here, `artifact.sha256` relayed untouched, `404` when the version is blocked.
///
/// `GET …/api/v3/collections/{namespace}/{name}/versions/{version}/`
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/{version}/",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Collection namespace"),
        ("name" = String, Path, description = "Collection name"),
        ("version" = String, Path, description = "Collection version"),
    ),
    responses(
        (status = 200, description = "The version document, with download_url pointed at this instance", body = crate::handlers::schemas::UpstreamDocument),
        (status = 400, description = "Not a collection coordinate"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Blocked, unknown version, or unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v3/collections/{namespace}/{name}/versions/{version}/")]
pub async fn galaxy_version_detail(
    req: HttpRequest,
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, version) = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    let package = collection_package(&namespace, &name).map_err(AppError::from)?;
    validate_version(&version).map_err(AppError::from)?;
    let public_base = registry_public_base(&req, &registry);

    // A blocked version is absent here as well as from the listing. The
    // resolver never gets this far for a range — the version is not in the list
    // it chose from — so this is the path a *pinned* requirement and a client
    // holding a stale listing take, and both get ansible's own
    // unsatisfiable-requirements error rather than a download.
    let blocked = svc
        .blocked_versions_for(&registry, &package, RegistryKind::Galaxy)
        .await;
    if blocked.contains(&version) {
        return Err(AppError::not_found(format!(
            "{package} {version} is not available from this registry"
        )));
    }

    let local = {
        let local_svc = Arc::clone(&local_svc);
        let registry = registry.clone();
        let package = package.clone();
        let version = version.clone();
        let base = public_base.clone();
        let namespace = namespace.clone();
        let name = name.clone();
        move |id: batlehub_core::entities::Identity| async move {
            let rows = local_svc
                .get_galaxy_versions(&registry, &package, &id)
                .await?;
            let row = rows
                .into_iter()
                .find(|r| r.version == version)
                .ok_or_else(|| {
                    batlehub_core::error::CoreError::NotFound(format!(
                        "{package} {version} is not published here"
                    ))
                })?;
            Ok::<_, batlehub_core::error::CoreError>(publish::local_version_document(
                &base, &namespace, &name, &row,
            ))
        }
    };

    let (mut doc, synthesised) = local_or_proxy_document_parts(
        &svc,
        &modes,
        &registry,
        identity.clone(),
        local,
        format!("{package} {version} is not published here"),
        PackageId::new(&registry, addressed(&package, &version), version.clone()),
        Action::ReleasesRead,
        DocumentKind::VERSION_DETAIL,
        public_base.clone(),
    )
    .await?;

    rewrite_version_document(&mut doc, &public_base, &namespace, &name, &version);
    let mut builder = HttpResponse::Ok();
    builder.content_type(JSON);
    synthesised_headers(&mut builder, synthesised);
    Ok(builder.json(doc))
}

/// The three URL fields §4.4 names, and nothing else.
///
/// `artifact.sha256`, `metadata`, `manifest`, `files` and `signatures` are the
/// fields a client verifies or reads as facts about the publisher's own
/// artifact, so they pass through exactly as received — the invariant of §5.2.
pub(super) fn rewrite_version_document(
    doc: &mut serde_json::Value,
    public_base: &str,
    namespace: &str,
    name: &str,
    version: &str,
) {
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    obj.insert(
        "download_url".to_owned(),
        serde_json::json!(artifact_url(public_base, namespace, name, version)),
    );
    obj.insert(
        "href".to_owned(),
        serde_json::json!(version_url(public_base, namespace, name, version)),
    );
    if let Some(collection) = obj.get_mut("collection").and_then(|c| c.as_object_mut()) {
        collection.insert(
            "href".to_owned(),
            serde_json::json!(collection_url(public_base, namespace, name)),
        );
    }
}

/// The collection tarball, byte-exact — the client hashes it against `artifact.sha256` from the version document.
///
/// `GET …/api/v3/artifacts/collections/{filename}`
///
/// The served path ends in the upstream filename because `_download_file`
/// derives the name it writes by slicing `.tar.gz` off the last path segment.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/galaxy/api/v3/artifacts/collections/{filename}",
    tag = "proxy/galaxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("filename" = String, Path, description = "{namespace}-{name}-{version}.tar.gz"),
    ),
    responses(
        (status = 200, description = "The collection tarball, byte-exact", body = ArtifactBytes, content_type = "application/gzip"),
        (status = 400, description = "Not a collection artifact name"),
        (status = 403, description = "Blocked version or access denied"),
        (status = 404, description = "Unknown version or registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/galaxy/api/v3/artifacts/collections/{filename}")]
pub async fn galaxy_artifact(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    modes: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, filename) = path.into_inner();
    require_registry_type(&registry, "galaxy", &map)?;
    // Parsed back into the coordinate it claims before it becomes a storage
    // key — this handler builds the key itself, so the Maven and NuGet-flat
    // rule applies (§6.5).
    let (package, version) = parse_artifact_filename(&filename).map_err(AppError::from)?;

    serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &modes,
        &registry,
        &package,
        &version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: "tarball",
            local_content_type: GZIP,
            proxy_content_type: Some(GZIP),
            action: Action::ReleasesRead,
            check_prerelease: true,
            append_signature: true,
        },
    )
    .await
}
