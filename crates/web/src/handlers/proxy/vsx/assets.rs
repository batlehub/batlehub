//! Serving the bytes an editor asks for: the VSIX itself, and the files inside
//! it.
//!
//! Every asset except the package is an entry in the VSIX, so one cached
//! artifact answers all of them and local mode behaves identically to proxy
//! mode. Fetching the package goes through the same helper as the plain
//! download route, which is what puts these reads behind the rule chain, the
//! cache and the audit trail.

use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};
use bytes::Bytes;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    services::{LocalRegistryService, ProxyRequest, ProxyResponse, ProxyService},
};

use super::protocol::asset_type;
use super::{archive, require_single_segment, require_vsx, VSIX_ARTIFACT};
use crate::handlers::schemas::ArtifactBytes;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

/// `GET …/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage`
///
/// The download URL the editor takes from `fallbackAssetUri`, and the one
/// `code --install-extension` resolves to upstream. Serves the same bytes as
/// the `Microsoft.VisualStudio.Services.VSIXPackage` asset below; both exist
/// because different editor versions reach for different ones.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage",
    tag = "proxy/openvsx",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("publisher" = String, Path, description = "Extension publisher"),
        ("name"      = String, Path, description = "Extension name"),
        ("version"   = String, Path, description = "Extension version"),
    ),
    responses(
        (status = 200, description = "VSIX package", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or extension"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/vscode/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage")]
pub async fn vsx_vspackage(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, publisher, name, version) = path.into_inner();
    require_vsx(&registry, &map)?;
    let extension_id = qualified(&publisher, &name)?;

    let bytes = vsix_bytes(
        &svc,
        &local_svc,
        &mode_map,
        &registry,
        &extension_id,
        &version,
        &identity,
    )
    .await?;
    Ok(HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(bytes))
}

/// `GET …/vscode/asset/{publisher}/{name}/{version}/{asset_type}`
///
/// What the editor builds by appending an asset type to `assetUri`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/vscode/asset/{publisher}/{name}/{version}/{asset_type}",
    tag = "proxy/openvsx",
    params(
        ("registry"   = String, Path, description = "Registry name"),
        ("publisher"  = String, Path, description = "Extension publisher"),
        ("name"       = String, Path, description = "Extension name"),
        ("version"    = String, Path, description = "Extension version"),
        ("asset_type" = String, Path, description = "Microsoft.VisualStudio.* asset type"),
    ),
    responses(
        (status = 200, description = "Asset bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown asset type, or the extension does not ship it"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/vscode/asset/{publisher}/{name}/{version}/{asset_type}")]
pub async fn vsx_asset(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, publisher, name, version, requested) = path.into_inner();
    require_vsx(&registry, &map)?;
    let extension_id = qualified(&publisher, &name)?;

    let bytes = vsix_bytes(
        &svc,
        &local_svc,
        &mode_map,
        &registry,
        &extension_id,
        &version,
        &identity,
    )
    .await?;

    // The package itself is the archive, not a file in it.
    if requested == asset_type::VSIX_PACKAGE {
        return Ok(HttpResponse::Ok()
            .content_type("application/octet-stream")
            .body(bytes));
    }

    let Some(path_in_vsix) = resolve_asset_path(&bytes, &requested) else {
        return Err(AppError::not_found(format!(
            "extension {extension_id}@{version} has no '{requested}' asset"
        )));
    };
    serve_entry(&bytes, &path_in_vsix)
}

/// `GET …/vscode/unpkg/{publisher}/{name}/{version}/{path}`
///
/// `resourceUrlTemplate`: an arbitrary file inside the extension, which the
/// editor uses for web extensions and for rendering the detail pane without
/// downloading the whole package.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/vscode/unpkg/{publisher}/{name}/{version}/{path}",
    tag = "proxy/openvsx",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("publisher" = String, Path, description = "Extension publisher"),
        ("name"      = String, Path, description = "Extension name"),
        ("version"   = String, Path, description = "Extension version"),
        ("path"      = String, Path, description = "Path inside the extension"),
    ),
    responses(
        (status = 200, description = "File bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Invalid path"),
        (status = 404, description = "No such file in the extension"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/vscode/unpkg/{publisher}/{name}/{version}/{path:.*}")]
pub async fn vsx_unpkg(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, publisher, name, version, inner) = path.into_inner();
    require_vsx(&registry, &map)?;
    let extension_id = qualified(&publisher, &name)?;
    // Belt and braces with `archive::read_entry`'s own check: rejecting here
    // keeps the traversal guard visible at the route that takes a free path.
    batlehub_core::services::validate_path_safe("extension file", &inner)
        .map_err(AppError::from)?;

    let bytes = vsix_bytes(
        &svc,
        &local_svc,
        &mode_map,
        &registry,
        &extension_id,
        &version,
        &identity,
    )
    .await?;
    serve_entry(&bytes, &inner)
}

/// `GET …/vscode/item?itemName=publisher.name`
///
/// `itemUrl` — where the editor's "View in Marketplace" link goes. A redirect
/// rather than rendered HTML: the only thing this needs to do is point at a
/// page that already exists, and hand-writing markup here would be the one
/// place in this feature where escaping mattered.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/vscode/item",
    tag = "proxy/openvsx",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("itemName" = String, Query, description = "Extension ID in publisher.name format"),
    ),
    responses(
        (status = 302, description = "Redirect to the extension's page"),
        (status = 400, description = "Missing or malformed itemName"),
        (status = 404, description = "Unknown registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/vscode/item")]
pub async fn vsx_item(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ItemQuery>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_vsx(&registry, &map)?;

    let item = query.item_name.trim();
    batlehub_core::services::validate_package_name(item).map_err(AppError::from)?;
    if !item.contains('.') {
        return Err(AppError::bad_request(format!(
            "itemName must be 'publisher.name', got '{item}'"
        )));
    }

    // The console's own package detail page. Scheme and host come from
    // `trusted_origin`, the one place that decides whether a forwarded header
    // may influence a generated URL — `ConnectionInfo` is lint-denied for
    // exactly that reason.
    let (scheme, host) = crate::middleware::proxy_trust::trusted_origin(&req);
    let target = format!(
        "{scheme}://{host}/packages/{}/{}",
        batlehub_adapters::registry::percent_encode(&registry),
        batlehub_adapters::registry::percent_encode(item),
    );
    Ok(HttpResponse::Found()
        .insert_header(("Location", target))
        .finish())
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct ItemQuery {
    #[serde(rename = "itemName")]
    pub item_name: String,
}

// ── shared ───────────────────────────────────────────────────────────────────

fn qualified(publisher: &str, name: &str) -> Result<String, AppError> {
    require_single_segment("publisher", publisher)?;
    require_single_segment("extension name", name)?;
    Ok(format!("{publisher}.{name}"))
}

/// Which file inside the VSIX an asset type names.
///
/// The manifest is at a fixed path; the icon's path comes from the manifest;
/// the prose files are conventions with several spellings each, so they are
/// matched rather than assumed.
fn resolve_asset_path(vsix: &[u8], requested: &str) -> Option<String> {
    match requested {
        asset_type::MANIFEST => Some("package.json".to_owned()),
        asset_type::ICON => archive::parse_manifest(vsix).and_then(|m| m.icon),
        asset_type::DETAILS => {
            archive::find_entry(vsix, |n| n.to_ascii_lowercase().starts_with("readme"))
        }
        asset_type::CHANGELOG => {
            archive::find_entry(vsix, |n| n.to_ascii_lowercase().starts_with("changelog"))
        }
        asset_type::LICENSE => {
            archive::find_entry(vsix, |n| n.to_ascii_lowercase().starts_with("license"))
        }
        _ => None,
    }
}

pub(super) fn serve_entry(vsix: &[u8], path_in_vsix: &str) -> Result<HttpResponse, AppError> {
    match archive::read_entry(vsix, path_in_vsix)? {
        Some(body) => Ok(HttpResponse::Ok()
            .content_type(archive::content_type_for(path_in_vsix))
            .body(body)),
        None => Err(AppError::not_found(format!(
            "'{path_in_vsix}' is not in this extension"
        ))),
    }
}

/// The VSIX bytes for one extension version, from wherever this registry keeps
/// them.
///
/// Local and hybrid read local storage; proxy (and a hybrid miss) go through
/// `ProxyService::handle`, which runs the rule chain — including
/// `BlockListRule`, so a blocked version is refused here even though the
/// gallery already hid it. That is the download gate RFC 0006 describes:
/// hiding governs which version a resolver picks, the gate governs whether the
/// caller may have the one it asked for.
pub(super) async fn vsix_bytes(
    svc: &web::Data<Arc<ProxyService>>,
    local_svc: &web::Data<Arc<LocalRegistryService>>,
    mode_map: &RegistryModeMap,
    registry: &str,
    extension_id: &str,
    version: &str,
    identity: &AuthIdentity,
) -> Result<Bytes, AppError> {
    let mode = mode_map.get(registry);
    let pkg = PackageId::new(registry, extension_id, version);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        // `source:read`: a VSIX is the extension's source archive, and this is
        // the route the rule-chain gap was first found and fixed on. The chain
        // now runs inside `get_artifact` for every ecosystem, against the
        // resource type named here.
        match local_svc
            .get_artifact(
                registry,
                extension_id,
                version,
                Action::SourceRead,
                &identity.0,
            )
            .await
        {
            Ok(bytes) => return Ok(bytes),
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }
    if mode == RegistryMode::Local {
        return Err(AppError::not_found(format!(
            "extension {extension_id}@{version} not found in local registry '{registry}'"
        )));
    }

    let req = ProxyRequest {
        package_id: pkg.with_artifact(VSIX_ARTIFACT),
        identity: identity.0.clone(),
        action: Action::SourceRead.to_owned(),
        ip_address: None,
        user_agent: None,
    };
    match svc.handle(req).await.map_err(AppError::from)? {
        ProxyResponse::Denied { reason } => Err(AppError::forbidden(reason)),
        ProxyResponse::Stream(stream) | ProxyResponse::ForgeStream { stream, .. } => {
            super::super::common::collect_storage_stream(stream).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qualified_name_is_two_single_segments() {
        assert_eq!(qualified("acme", "tool").unwrap(), "acme.tool");
        assert!(
            qualified("acme/evil", "tool").is_err(),
            "a decoded slash must not widen the coordinate"
        );
        assert!(qualified("acme", "").is_err());
    }

    #[test]
    fn the_manifest_is_at_a_fixed_path_and_unknown_types_resolve_to_nothing() {
        assert_eq!(
            resolve_asset_path(b"", asset_type::MANIFEST).as_deref(),
            Some("package.json")
        );
        assert!(resolve_asset_path(b"", "Microsoft.VisualStudio.Made.Up").is_none());
    }
}
