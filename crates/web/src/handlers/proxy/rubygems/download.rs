use super::{
    document_response, fetch_proxy_document, get, proxy_stream, require_registry_type,
    serve_local_or_proxy_artifact, serve_local_or_proxy_document, web, AppError, Arc, AuthIdentity,
    HttpResponse, LocalOrProxyArtifactOpts, LocalRegistryService, PackageId, ProxyService,
    RegistryMap, RegistryModeMap, Responder,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{error::CoreError, ports::DocumentKind};

use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use batlehub_core::entities::Action;

/// Download a gem file.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/gems/{filename}",
    tag = "proxy/rubygems",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("filename" = String, Path, description = "Gem filename, e.g. rails-7.1.0.gem"),
    ),
    responses(
        (status = 200, description = "Gem binary", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Gem not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/gems/{filename}")]
pub async fn gem_download(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, filename) = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;

    let stem = filename
        .strip_suffix(".gem")
        .ok_or_else(|| AppError::bad_request(format!("invalid gem filename: {filename}")))?;

    let (name, version) = batlehub_adapters::registry::rubygems::split_gem_stem(stem)
        .ok_or_else(|| AppError::bad_request(format!("cannot parse gem filename: {filename}")))?;

    serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        name,
        version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: "gem",
            local_content_type: "application/octet-stream",
            proxy_content_type: Some("application/octet-stream"),
            action: Action::ReleasesRead,
            check_prerelease: true,
            append_signature: true,
        },
    )
    .await
}

/// Get gem information JSON (latest version).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/v1/gems/{name}.json",
    tag = "proxy/rubygems",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Gem name"),
    ),
    responses(
        (status = 200, description = "Gem info JSON", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Gem not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/v1/gems/{name}.json")]
pub async fn gem_info(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;

    let pkg = PackageId::new(&registry, &name, "info");
    let not_found_msg = format!("gem '{name}' not found");
    let mode = mode_map.get(&registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        svc.authorize_read(&pkg, &identity.0, Action::ReleasesRead)
            .await
            .map_err(AppError::from)?;
        match local_svc
            .get_rubygems_gem_info(&registry, &name, &identity)
            .await
        {
            Ok(json) => {
                return Ok(HttpResponse::Ok()
                    .content_type("application/json")
                    .json(json))
            }
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(CoreError::NotFound(_)) => return Err(AppError::not_found(not_found_msg)),
            Err(e) => return Err(AppError::from(e)),
        }
    }

    proxy_gem_info(svc, registry, name, identity).await
}

/// The gem document, repaired against the gem's *filtered* version list.
///
/// This document names exactly one version — the gem's current release — and
/// carries no list to pick a replacement from, so hiding a blocked release
/// means composing two documents rather than editing one. Both go through
/// `ProxyService::version_document`, so both are authorised, cached and
/// filtered on the way.
///
/// **Fails open on the versions fetch.** If the versions API is unreachable the
/// gem document is served as upstream sent it, matching the rest of this path:
/// degrading to showing more than intended is the direction that keeps a
/// registry working, and the download gate still refuses the bytes.
async fn proxy_gem_info(
    svc: web::Data<Arc<ProxyService>>,
    registry: String,
    name: String,
    identity: AuthIdentity,
) -> Result<HttpResponse, AppError> {
    let mut gem = fetch_proxy_document(
        svc.clone(),
        PackageId::new(&registry, &name, "info"),
        AuthIdentity(identity.0.clone(), identity.1.clone()),
        Action::ReleasesRead,
        DocumentKind::GEM,
        String::new(),
    )
    .await?;

    let versions = fetch_proxy_document(
        svc,
        PackageId::new(&registry, &name, "versions"),
        identity,
        Action::ReleasesRead,
        DocumentKind::Versions,
        String::new(),
    )
    .await;

    match versions {
        Ok(v) => {
            let available = rubygems_version_numbers(&v);
            if let Some(json) = gem.body.as_json_mut() {
                if let Some(hidden) =
                    batlehub_core::services::blocking::rubygems::repair_gem(json, &available)
                {
                    tracing::debug!(
                        registry = %registry,
                        package = %name,
                        hidden = %hidden,
                        "rebuilt the gem document around an allowed version"
                    );
                }
            }
        }
        Err(e) => tracing::warn!(
            registry = %registry,
            package = %name,
            error = %e,
            "could not load the filtered version list; serving the gem document unrepaired"
        ),
    }

    Ok(document_response(gem))
}

/// The `number` of every entry in a (filtered) RubyGems versions document.
fn rubygems_version_numbers(doc: &batlehub_core::ports::VersionDocument) -> Vec<String> {
    doc.body
        .as_json()
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("number").and_then(|n| n.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// List all versions of a gem.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/v1/versions/{name}.json",
    tag = "proxy/rubygems",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Gem name"),
    ),
    responses(
        (status = 200, description = "Gem versions JSON array", body = Vec<UpstreamDocument>),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Gem not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/v1/versions/{name}.json")]
pub async fn gem_versions(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;

    let pkg = PackageId::new(&registry, &name, "versions");
    let not_found_msg = format!("gem '{name}' not found");
    let (fetch_registry, fetch_name) = (registry.clone(), name.clone());
    // A version listing, not an artifact: the proxy fall-through fetches and
    // filters the document so `bundle install` never resolves a constraint onto
    // a blocked version and then be refused the `.gem`.
    serve_local_or_proxy_document(
        svc,
        &mode_map,
        &registry,
        identity,
        move |identity: batlehub_core::entities::Identity| async move {
            local_svc
                .get_rubygems_versions(&fetch_registry, &fetch_name, &identity)
                .await
        },
        not_found_msg,
        pkg,
        Action::ReleasesRead,
        DocumentKind::Versions,
        "application/json",
        String::new(),
    )
    .await
}

/// Serve a compressed gemspec file.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/quick/Marshal.4.8/{filename}",
    tag = "proxy/rubygems",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("filename" = String, Path, description = "Gemspec filename, e.g. rails-7.1.0.gemspec.rz"),
    ),
    responses(
        (status = 200, description = "Zlib-compressed gemspec", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Gemspec not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/quick/Marshal.4.8/{filename}")]
pub async fn gem_gemspec(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, filename) = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;

    let stem = filename
        .strip_suffix(".gemspec.rz")
        .ok_or_else(|| AppError::bad_request(format!("invalid gemspec filename: {filename}")))?;

    let (name, version) =
        batlehub_adapters::registry::rubygems::split_gem_stem(stem).ok_or_else(|| {
            AppError::bad_request(format!("cannot parse gemspec filename: {filename}"))
        })?;

    let pkg = PackageId::new(&registry, name, version).with_artifact("gemspec");
    proxy_stream(
        svc,
        pkg,
        identity,
        Action::ReleasesRead,
        Some("application/octet-stream"),
    )
    .await
}
