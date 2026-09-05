use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};

use batlehub_config::schema::RegistryMode as Mode;
use batlehub_core::{
    entities::PackageId,
    ports::DocumentKind,
    services::{LocalRegistryService, ProxyService},
};

use crate::handlers::proxy::common::{
    fetch_proxy_document, proxy_stream, registry_public_base, require_registry_type,
    serve_local_or_proxy_artifact, LocalOrProxyArtifactOpts,
};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};

use super::parse_pypi_filename;
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use batlehub_core::entities::Action;

// ── Proxy routes ──────────────────────────────────────────────────────────────

/// Proxy the PyPI Simple Repository API root index.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/simple/",
    tag = "proxy/pypi",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Simple index HTML", body = ProtocolDocument, content_type = "text/html"),
        (status = 404, description = "Registry not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/simple/")]
pub async fn pypi_simple_root(
    path: web::Path<String>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "pypi", &map)?;

    // Represent the root index as a special sentinel PackageId.
    let pkg = PackageId::new(&registry, "__simple__", "__root__");
    proxy_stream(svc, pkg, identity, Action::ReleasesRead, Some("text/html")).await
}

/// Proxy the PyPI Simple Repository API for a specific package, rewriting file
/// URLs so artifacts are downloaded through the batlehub cache.
///
/// In local/hybrid mode, the page is generated from locally-published packages.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/simple/{package}/",
    tag = "proxy/pypi",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Package name"),
    ),
    responses(
        (status = 200, description = "Simple index page with rewritten file URLs", body = ProtocolDocument, content_type = "text/html"),
        (status = 404, description = "Package not found"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[get("/proxy/{registry}/simple/{package}/")]
pub async fn pypi_simple_package(
    path: web::Path<(String, String)>,
    req: HttpRequest,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, package) = path.into_inner();
    require_registry_type(&registry, "pypi", &map)?;

    let mode = mode_map.get(&registry);
    let normalized = batlehub_adapters::registry::pypi::normalize_name(&package);

    let proxy_base = registry_public_base(&req, &registry);

    if mode == Mode::Local {
        let html = local_svc
            .get_pypi_simple_page(&registry, &normalized, &proxy_base, &identity.0)
            .await
            .map_err(AppError::from)?;
        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .body(html));
    }

    if mode == Mode::Hybrid {
        // Try local first; fall through to upstream if not found.
        match local_svc
            .get_pypi_simple_page(&registry, &normalized, &proxy_base, &identity.0)
            .await
        {
            Ok(html) => {
                return Ok(HttpResponse::Ok()
                    .content_type("text/html; charset=utf-8")
                    .body(html));
            }
            Err(batlehub_core::error::CoreError::NotFound(_)) => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }

    // Which of the two representations the client wants. PEP 691 JSON and
    // PEP 503 HTML are different documents for one URL, so they are different
    // `DocumentKind`s and land in different metadata-cache slots — keyed
    // together, whichever one warmed the cache would be served to clients that
    // asked for the other.
    let wants_json = req
        .headers()
        .get(actix_web::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("application/vnd.pypi.simple"));
    let doc_kind = if wants_json {
        DocumentKind::SIMPLE_JSON
    } else {
        DocumentKind::Versions
    };

    // Through `ProxyService` rather than a direct `fetch_simple_page`: that is
    // what authorises the read, audits a refusal, caches the document and — the
    // reason this route moved — removes administratively blocked versions
    // before pip resolves against them.
    let doc = fetch_proxy_document(
        svc,
        PackageId::new(&registry, &normalized, "__simple__"),
        identity,
        Action::ReleasesRead,
        doc_kind,
        proxy_base.clone(),
    )
    .await?;

    // The URL rewrite stays here rather than in `blocking::rewrite_urls`: it
    // needs the *serialised* body in either encoding, and `rewrite_simple_page`
    // already handles both.
    let (body, content_type) = match &doc.body {
        batlehub_core::ports::DocumentBody::Text(html) => {
            (html.clone().into_bytes(), doc.content_type.clone())
        }
        batlehub_core::ports::DocumentBody::Json(v) => (
            serde_json::to_vec(v).unwrap_or_default(),
            doc.content_type.clone(),
        ),
    };
    let rewritten = batlehub_adapters::registry::pypi::rewrite_simple_page(
        &body,
        Some(&content_type),
        &proxy_base,
    );

    let mut builder = HttpResponse::Ok();
    builder.content_type(content_type);
    crate::handlers::proxy::common::listing_headers(&mut builder, &doc);
    Ok(builder.body(rewritten))
}

/// The PyPI JSON API — `GET /pypi/{package}/json`.
///
/// RFC 0009 §7.6. pip does not need it (the simple index is the resolver's
/// input), but Poetry reads it for some sources and a good deal of ad-hoc
/// tooling assumes it exists.
///
/// Rendered from the **filtered** simple index rather than fetched separately,
/// so it cannot list a release the simple page hides — two documents describing
/// one package have to agree, and the cheapest way to guarantee that is to give
/// them one source.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/pypi/{package}/json",
    tag = "proxy/pypi",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("package"  = String, Path, description = "Project name"),
    ),
    responses(
        (status = 200, description = "PyPI JSON API document", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or project"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/pypi/{package}/json")]
pub async fn pypi_json(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    req: HttpRequest,
) -> Result<impl Responder, AppError> {
    let (registry, package) = path.into_inner();
    require_registry_type(&registry, "pypi", &map)?;
    batlehub_core::services::validate_package_name(&package)
        .map_err(|e| AppError::bad_request(format!("invalid project name: {e}")))?;

    let proxy_base = registry_public_base(&req, &registry);
    let pkg = batlehub_core::entities::PackageId::new(&registry, &package, "latest");
    let proxy_req = batlehub_core::services::ProxyRequest {
        package_id: pkg,
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: None,
        user_agent: None,
    };

    // PEP 691 JSON rather than the HTML page: same filtered content, already
    // structured, so nothing here has to parse anchors.
    let doc = svc
        .version_document(
            &proxy_req,
            batlehub_core::ports::DocumentKind::SIMPLE_JSON,
            &proxy_base,
        )
        .await
        .map_err(AppError::from)?;

    let files = doc
        .body
        .as_json()
        .and_then(|j| j.get("files"))
        .and_then(|f| f.as_array())
        .cloned()
        .unwrap_or_default();

    let urls: Vec<serde_json::Value> = files
        .iter()
        .map(|f| {
            serde_json::json!({
                "filename": f.get("filename").cloned().unwrap_or(serde_json::Value::Null),
                "url": f.get("url").cloned().unwrap_or(serde_json::Value::Null),
                "digests": f.get("hashes").cloned().unwrap_or(serde_json::json!({})),
                "yanked": f.get("yanked").cloned().unwrap_or(serde_json::json!(false)),
            })
        })
        .collect();

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(serde_json::json!({
            "info": { "name": package },
            "urls": urls,
            // Deliberately empty: `releases` is a version→files map, and the
            // simple index this renders from is a flat file list with no
            // version grouping. Synthesising one would mean parsing versions out
            // of filenames — which is exactly the guessing PEP 691 exists to end.
            "releases": {},
        })))
}

/// Download a PyPI distribution file through the proxy cache.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/packages/{filename}",
    tag = "proxy/pypi",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("filename" = String, Path, description = "Distribution filename"),
    ),
    responses(
        (status = 200, description = "Distribution bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 404, description = "File not found"),
        (status = 422, description = "Cannot parse filename"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/packages/{filename}")]
pub async fn pypi_file_download(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, filename) = path.into_inner();
    require_registry_type(&registry, "pypi", &map)?;

    // PEP 658: `{file}.metadata` is a sibling of the distribution, not a
    // distribution of its own, so the coordinate comes from the stripped name
    // while the full filename stays the artifact sub-coordinate — that is what
    // tells the adapter to fetch the sibling and keeps the two cached apart.
    let coordinate_name = filename.strip_suffix(".metadata").unwrap_or(&filename);
    let (name, version) = parse_pypi_filename(coordinate_name).ok_or_else(|| {
        AppError::unprocessable(format!("cannot parse PyPI filename: {filename}"))
    })?;

    serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        &name,
        &version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: &filename,
            local_content_type: "application/octet-stream",
            proxy_content_type: Some("application/octet-stream"),
            action: Action::ReleasesRead,
            check_prerelease: false,
            append_signature: false,
        },
    )
    .await
}
