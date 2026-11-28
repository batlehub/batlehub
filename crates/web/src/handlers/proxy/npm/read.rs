use super::{
    get, post, proxy_stream, registry_public_base, require_npm, require_npm_or_cargo,
    serve_local_or_proxy_artifact, serve_local_or_proxy_document, serve_local_or_proxy_json, web,
    AppError, Arc, AuthIdentity, HttpRequest, HttpResponse, LocalOrProxyArtifactOpts,
    LocalRegistryService, PackageId, ProxyService, RegistryMap, RegistryModeMap, Responder,
    UpstreamMap,
};
use crate::handlers::proxy::common::attachment_disposition;
use crate::handlers::proxy::upstream::{cached_forward, Outbound};
use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use batlehub_core::entities::Action;

/// Fetch package metadata (all versions / packument).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{package}",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package / crate name"),
    ),
    responses(
        (status = 200, description = "Package metadata JSON", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{package}")]
pub async fn get_packument(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    req: HttpRequest,
) -> Result<impl Responder, AppError> {
    let (registry, package) = path.into_inner();
    require_npm_or_cargo(&registry, &map)?;

    let pkg = PackageId::listing(&registry, &package, "latest");
    if map.is_type(&registry, "npm") {
        let url = registry_public_base(&req, &registry);
        let not_found_msg = format!("package '{package}' not found");
        let (fetch_registry, fetch_package) = (registry.clone(), package.clone());
        let proxy_url = url.clone();
        // A packument is a document, not an artifact: the proxy fall-through
        // fetches and rewrites it (blocked versions out, tarball URLs back to
        // this host) rather than streaming upstream bytes through, which for
        // this route would have served the `latest` tarball as the packument.
        return serve_local_or_proxy_document(
            svc,
            &mode_map,
            &registry,
            identity,
            move |identity: batlehub_core::entities::Identity| async move {
                local_svc
                    .get_npm_packument(&fetch_registry, &fetch_package, &url, &identity)
                    .await
            },
            not_found_msg,
            pkg,
            Action::ReleasesRead,
            batlehub_core::ports::DocumentKind::Versions,
            "application/json",
            proxy_url,
        )
        .await;
    }

    proxy_stream(svc, pkg, identity, Action::ReleasesRead, None).await
}

/// Fetch package version metadata.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{package}/{version}",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package / crate name"),
        ("version"  = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = "Version metadata JSON", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{package}/{version}")]
pub async fn get_version(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    req: HttpRequest,
) -> Result<impl Responder, AppError> {
    let (registry, package, version) = path.into_inner();
    require_npm_or_cargo(&registry, &map)?;

    let pkg = PackageId::new(&registry, &package, &version);
    if map.is_type(&registry, "npm") {
        let url = registry_public_base(&req, &registry);
        let not_found_msg = format!("{package}@{version} not found");
        let (fetch_registry, fetch_package, fetch_version) =
            (registry.clone(), package.clone(), version.clone());
        return serve_local_or_proxy_json(
            svc,
            &mode_map,
            &registry,
            identity,
            move |identity: batlehub_core::entities::Identity| async move {
                local_svc
                    .get_npm_version(
                        &fetch_registry,
                        &fetch_package,
                        &fetch_version,
                        &url,
                        &identity,
                    )
                    .await
            },
            not_found_msg,
            pkg,
            Action::ReleasesRead,
            None,
        )
        .await;
    }

    proxy_stream(svc, pkg, identity, Action::ReleasesRead, None).await
}

/// Download npm package tarball for a specific version.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{package}/{version}/tarball",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package name"),
        ("version"  = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = "npm .tgz tarball", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{package}/{version}/tarball")]
pub async fn download_tarball(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, package, version) = path.into_inner();
    require_npm(&registry, &map)?;

    let mut resp = serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        &package,
        &version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: "tarball",
            local_content_type: "application/octet-stream",
            proxy_content_type: None,
            action: Action::SourceRead,
            check_prerelease: true,
            append_signature: false,
        },
    )
    .await?;
    resp.headers_mut().insert(
        actix_web::http::header::CONTENT_DISPOSITION,
        attachment_disposition(&tarball_file_name(&package, &version))?,
    );
    Ok(resp)
}

/// npm's own name for a version's tarball: `pkg-1.2.3.tgz`, with the scope
/// dropped.
///
/// `@babel/core` publishes `core-7.24.0.tgz`, not `@babel/core-7.24.0.tgz` —
/// registries serve it under `/@babel/core/-/core-7.24.0.tgz`, and the slash in
/// the scope is not a filename character in the first place. Keeping the scope
/// would put a name on disk that no npm user recognises and that
/// `sanitize_filename` would have to mangle anyway.
fn tarball_file_name(package: &str, version: &str) -> String {
    let base = package.rsplit('/').next().unwrap_or(package);
    format!("{base}-{version}.tgz")
}

/// `npm audit`, quick mode — on the path npm sends.
///
/// See `AUDIT_QUICK` below for why there are two routes per endpoint.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/-/npm/v1/security/audits/quick",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "npm registry name"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Audit advisory data, from cache or upstream", body = UpstreamDocument),
        (status = 404, description = "Unknown or non-npm registry"),
        (status = 502, description = "Upstream unreachable and no cached answer"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/-/npm/v1/security/audits/quick")]
pub async fn audit_quick(
    path: web::Path<(String,)>,
    body: web::Json<serde_json::Value>,
    map: web::Data<RegistryMap>,
    upstream_map: web::Data<UpstreamMap>,
    svc: web::Data<Arc<ProxyService>>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let (registry,) = path.into_inner();
    forward_npm_audit(
        &registry,
        AUDIT_QUICK,
        body.into_inner(),
        &map,
        &upstream_map,
        &svc,
        &client,
    )
    .await
}

/// `npm audit`, bulk mode — the default since npm 7, on the path npm sends.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/-/npm/v1/security/advisories/bulk",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "npm registry name"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Bulk advisory data, from cache or upstream", body = UpstreamDocument),
        (status = 404, description = "Unknown or non-npm registry"),
        (status = 502, description = "Upstream unreachable and no cached answer"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/-/npm/v1/security/advisories/bulk")]
pub async fn audit_bulk(
    path: web::Path<(String,)>,
    body: web::Json<serde_json::Value>,
    map: web::Data<RegistryMap>,
    upstream_map: web::Data<UpstreamMap>,
    svc: web::Data<Arc<ProxyService>>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let (registry,) = path.into_inner();
    forward_npm_audit(
        &registry,
        AUDIT_BULK,
        body.into_inner(),
        &map,
        &upstream_map,
        &svc,
        &client,
    )
    .await
}

/// Deprecated alias of the quick audit endpoint — npm sends `/-/npm/v1/security/audits/quick`.
///
/// This path was never npm's, so nothing is lost by removing it — but it has
/// shipped, some deployment may have scripted it, and removing a live route is
/// a separate decision from fixing the bug. To be removed in a later release.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/-/npm/v1/audit/quick",
    tag = "proxy/npm",
    params(("registry" = String, Path, description = "npm registry name")),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Deprecated alias of the quick audit endpoint", body = UpstreamDocument),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/-/npm/v1/audit/quick")]
pub async fn audit_quick_legacy(
    path: web::Path<(String,)>,
    body: web::Json<serde_json::Value>,
    map: web::Data<RegistryMap>,
    upstream_map: web::Data<UpstreamMap>,
    svc: web::Data<Arc<ProxyService>>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let (registry,) = path.into_inner();
    forward_npm_audit(
        &registry,
        AUDIT_QUICK,
        body.into_inner(),
        &map,
        &upstream_map,
        &svc,
        &client,
    )
    .await
}

/// Deprecated alias of the bulk audit endpoint — npm sends `/-/npm/v1/security/advisories/bulk`.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/-/npm/v1/audit/bulk",
    tag = "proxy/npm",
    params(("registry" = String, Path, description = "npm registry name")),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Deprecated alias of the bulk audit endpoint", body = UpstreamDocument),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/-/npm/v1/audit/bulk")]
pub async fn audit_bulk_legacy(
    path: web::Path<(String,)>,
    body: web::Json<serde_json::Value>,
    map: web::Data<RegistryMap>,
    upstream_map: web::Data<UpstreamMap>,
    svc: web::Data<Arc<ProxyService>>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let (registry,) = path.into_inner();
    forward_npm_audit(
        &registry,
        AUDIT_BULK,
        body.into_inner(),
        &map,
        &upstream_map,
        &svc,
        &client,
    )
    .await
}

/// The two audit endpoints npm actually calls, as sub-paths of the registry.
///
/// RFC 0009 §7.1. This used to be a single `"quick"`/`"bulk"` discriminant
/// interpolated into `{upstream}/-/npm/v1/audit/{endpoint}` — a path
/// `registry.npmjs.org` does not answer, matching inbound routes npm does not
/// send. Both halves of the round trip were addressed to an endpoint that
/// exists in neither direction, and four tests asserted the invented paths
/// because they were written from our implementation rather than from npm's.
///
/// The real ones, from npm's own registry client:
pub(super) const AUDIT_QUICK: &str = "/-/npm/v1/security/audits/quick";
pub(super) const AUDIT_BULK: &str = "/-/npm/v1/security/advisories/bulk";

async fn forward_npm_audit(
    registry: &str,
    endpoint: &str,
    body: serde_json::Value,
    map: &RegistryMap,
    upstream_map: &UpstreamMap,
    svc: &Arc<ProxyService>,
    client: &reqwest::Client,
) -> Result<HttpResponse, AppError> {
    require_npm(registry, map)?;

    let upstream = upstream_map
        .upstream_for(registry)
        .ok_or_else(|| AppError::not_found(format!("no upstream configured for '{registry}'")))?;

    // The request body selects the answer — `npm audit` POSTs the dependency
    // set — so it belongs in the key. Hashed rather than embedded: a lockfile's
    // worth of dependencies is far too long for a cache key, and the digest is
    // stable across the map ordering `serde_json` happens to emit.
    let digest = {
        use sha2::{Digest, Sha256};
        let canonical = serde_json::to_vec(&body).unwrap_or_default();
        hex::encode(Sha256::digest(&canonical))
    };
    let cache_key = format!("audit:{registry}:{endpoint}:{digest}");

    cached_forward(
        svc,
        client,
        registry,
        &cache_key,
        Outbound::post_json(format!("{upstream}{endpoint}"), body),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::tarball_file_name;

    /// npm's own name for the file, which is not the package's name.
    ///
    /// A scoped package publishes `core-7.24.0.tgz` — registries serve it at
    /// `/@babel/core/-/core-7.24.0.tgz` — so keeping the scope would put a name
    /// on disk that no npm user recognises, and one whose `/` is not a filename
    /// character in the first place.
    #[test]
    fn scoped_packages_drop_the_scope() {
        assert_eq!(
            tarball_file_name("@babel/core", "7.24.0"),
            "core-7.24.0.tgz"
        );
        assert_eq!(tarball_file_name("lodash", "4.17.21"), "lodash-4.17.21.tgz");
    }
}
