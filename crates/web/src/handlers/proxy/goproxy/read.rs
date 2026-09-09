use std::io::Read as _;
use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    ports::DocumentKind,
    services::blocking::goproxy as go_blocking,
    services::{LocalRegistryService, ProxyService},
};

use crate::handlers::proxy::common::{
    document_response, fetch_proxy_document, proxy_document, proxy_stream, require_registry_type,
};
use crate::handlers::schemas::{ArtifactBytes, ProtocolDocument, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

/// Dispatch a local/hybrid goproxy file request.
///
/// Returns the response on success, or `Err(CoreError::NotFound)` when the
/// module/extension is not found (callers handle hybrid fallthrough).
async fn local_goproxy_file(
    local_svc: &LocalRegistryService,
    registry: &str,
    module: &str,
    version: &str,
    ext: &str,
    identity: &batlehub_core::entities::Identity,
    net: &batlehub_core::entities::CallerNet,
) -> Result<HttpResponse, batlehub_core::error::CoreError> {
    let resp = match ext {
        "info" => local_svc
            .get_go_info(registry, module, version, identity)
            .await
            .map(|info| {
                HttpResponse::Ok()
                    .content_type("application/json")
                    .json(info)
            })?,
        "mod" => local_svc
            .get_go_mod(registry, module, version, identity)
            .await
            .map(|content| HttpResponse::Ok().content_type("text/plain").body(content))?,
        "zip" => {
            local_svc
                .check_prerelease_access(registry, version, identity)
                .await?;
            // `source:read`, not `releases:read`: a module zip *is* the source,
            // and this handler's own proxy fall-through distinguishes the two
            // the same way. A registry that grants a role releases-only must not
            // hand it the source because the module happens to be local.
            local_svc
                .get_artifact(registry, module, version, Action::SourceRead, identity, net)
                .await
                .map(|bytes| {
                    HttpResponse::Ok()
                        .content_type("application/zip")
                        .body(bytes)
                })?
        }
        _ => {
            return Err(batlehub_core::error::CoreError::NotFound(format!(
                "unknown goproxy file extension '.{ext}'"
            )))
        }
    };
    Ok(resp)
}

/// Scan a Go module zip for a go.mod entry matching `mod_suffix` (exact) or
/// any path ending with `/go.mod` (fallback). Returns `None` if not found.
fn scan_zip_for_go_mod(zip_bytes: &[u8], mod_suffix: &str) -> Option<String> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    for i in 0..archive.len() {
        if let Ok(mut file) = archive.by_index(i) {
            let name = file.name().to_owned();
            if name == mod_suffix || name.ends_with("/go.mod") {
                let mut contents = String::new();
                if file.read_to_string(&mut contents).is_ok() {
                    return Some(contents);
                }
            }
        }
    }
    None
}

/// Extract the go.mod content from a Go module zip archive.
/// Go module zips contain entries named `{module}@{version}/{path}`.
/// Returns a minimal go.mod if none is found.
pub(super) fn extract_go_mod(zip_bytes: &[u8], module: &str, version: &str) -> String {
    let mod_suffix = format!("{module}@{version}/go.mod");
    scan_zip_for_go_mod(zip_bytes, &mod_suffix)
        .unwrap_or_else(|| format!("module {module}\n\ngo 1.21\n"))
}

/// Fetch the latest version info for a Go module.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{module}/@latest",
    tag = "proxy/goproxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("module"   = String, Path, description = "Go module path (may contain slashes)"),
    ),
    responses(
        (status = 200, description = "Latest version info JSON", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Module not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{module:[^@]+}@latest")]
pub async fn goproxy_latest(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, raw_module) = path.into_inner();
    require_registry_type(&registry, "goproxy", &map)?;
    let module = raw_module.trim_end_matches('/');

    let pkg = PackageId::new(&registry, module, "latest");
    let not_found_msg = format!("module '{module}' not found");
    let mode = mode_map.get(&registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        svc.authorize_read(&pkg, &identity.0, Action::ReleasesRead)
            .await
            .map_err(AppError::from)?;
        match local_svc.get_go_latest(&registry, module, &identity).await {
            Ok(json) => {
                return Ok(HttpResponse::Ok()
                    .content_type("application/json")
                    .json(json))
            }
            Err(CoreError::NotFound(_)) if matches!(mode, RegistryMode::Hybrid) => {}
            Err(CoreError::NotFound(_)) => return Err(AppError::not_found(not_found_msg)),
            Err(e) => return Err(AppError::from(e)),
        }
    }

    proxy_go_latest(svc, registry, module.to_owned(), identity).await
}

/// `@latest`, re-resolved against the module's *filtered* `@v/list`.
///
/// `@latest` names one version and carries no list, so hiding a blocked release
/// from it is re-resolution rather than removal — and the list it has to
/// re-resolve against is a second document. Both go through
/// `ProxyService::version_document`, so both are authorised, cached and
/// filtered on the way; `@latest` is a low-frequency endpoint next to `@v/list`,
/// which is what makes the second fetch affordable here and not there.
///
/// **Fails open on the list fetch**, like every other step on this path: an
/// unreachable `@v/list` serves `@latest` as upstream sent it rather than
/// turning a metadata blip into a broken module.
async fn proxy_go_latest(
    svc: web::Data<Arc<ProxyService>>,
    registry: String,
    module: String,
    identity: AuthIdentity,
) -> Result<HttpResponse, AppError> {
    let latest = fetch_proxy_document(
        svc.clone(),
        PackageId::new(&registry, &module, "latest"),
        AuthIdentity(identity.0.clone(), identity.1.clone()),
        Action::ReleasesRead,
        DocumentKind::LATEST,
        String::new(),
    )
    .await?;

    let list = fetch_proxy_document(
        svc,
        PackageId::new(&registry, &module, "latest"),
        identity,
        Action::ReleasesRead,
        DocumentKind::Versions,
        String::new(),
    )
    .await;

    let Ok(list) = list else {
        tracing::warn!(
            registry = %registry,
            module = %module,
            "could not load the filtered @v/list; serving @latest unrepaired"
        );
        return Ok(document_response(latest));
    };

    let allowed = go_blocking::versions_in_list(list.body.as_text().unwrap_or_default());
    let Some(json) = latest.body.as_json() else {
        return Ok(document_response(latest));
    };
    match go_blocking::repaired_latest(json, &allowed) {
        Some(repaired) => Ok(HttpResponse::Ok()
            .content_type(latest.content_type.clone())
            .json(repaired)),
        // Every version is blocked. A 404 is what the Go client already handles
        // for a module with no releases, and is honest: there is no latest
        // version this client may have.
        None => Err(AppError::not_found(format!(
            "module '{module}' has no available versions"
        ))),
    }
}

/// List known versions for a Go module.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{module}/@v/list",
    tag = "proxy/goproxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("module"   = String, Path, description = "Go module path (may contain slashes)"),
    ),
    responses(
        (status = 200, description = "Newline-separated version list", body = ProtocolDocument, content_type = "text/plain"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Module not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{module:[^@]+}@v/list")]
pub async fn goproxy_list(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, raw_module) = path.into_inner();
    require_registry_type(&registry, "goproxy", &map)?;
    let module = raw_module.trim_end_matches('/');
    let mode = mode_map.get(&registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        match local_svc
            .get_go_version_list(&registry, module, &identity)
            .await
        {
            Ok(list) => {
                return Ok(HttpResponse::Ok().content_type("text/plain").body(list));
            }
            Err(CoreError::NotFound(_)) if matches!(mode, RegistryMode::Hybrid) => {}
            Err(CoreError::NotFound(_)) => {
                return Ok(HttpResponse::Ok().content_type("text/plain").body(""));
            }
            Err(e) => return Err(AppError::from(e)),
        }
    }

    // `@v/list` is the document `go get` resolves a version query against, so
    // it goes through `proxy_document` (fetch, filter, answer in `text/plain`)
    // rather than `proxy_stream`, which would forward the upstream's own list
    // with blocked versions still in it.
    proxy_document(
        svc,
        PackageId::new(&registry, module, "latest"),
        identity,
        Action::ReleasesRead,
        DocumentKind::Versions,
        String::new(),
    )
    .await
}

/// Fetch a versioned Go module file: `.info`, `.mod`, or `.zip`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{module}/@v/{filename}",
    tag = "proxy/goproxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("module"   = String, Path, description = "Go module path (may contain slashes)"),
        ("filename" = String, Path, description = "Versioned file: {version}.info, {version}.mod, or {version}.zip"),
    ),
    responses(
        (status = 200, description = "Requested module file — `.info` (JSON), `.mod` (text) or `.zip`, per the requested extension", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Unknown file type"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Module or version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{module:[^@]+}@v/{filename}")]
pub async fn goproxy_file(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, raw_module, filename) = path.into_inner();
    require_registry_type(&registry, "goproxy", &map)?;
    let module = raw_module.trim_end_matches('/');
    let mode = mode_map.get(&registry);

    let (version, ext) = filename
        .rsplit_once('.')
        .ok_or_else(|| AppError::not_found(format!("unknown goproxy file '{filename}'")))?;

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        match local_goproxy_file(
            &local_svc,
            &registry,
            module,
            version,
            ext,
            &identity,
            &identity.1,
        )
        .await
        {
            Ok(resp) => return Ok(resp),
            Err(CoreError::NotFound(_)) if matches!(mode, RegistryMode::Hybrid) => {}
            Err(CoreError::NotFound(msg)) => return Err(AppError::not_found(msg)),
            Err(e) => return Err(AppError::from(e)),
        }
    }

    let (pkg, content_type, action) = match ext {
        "info" => (
            PackageId::new(&registry, module, version),
            "application/json",
            Action::ReleasesRead,
        ),
        "mod" => (
            PackageId::new(&registry, module, version).with_artifact("mod"),
            "text/plain",
            Action::ReleasesRead,
        ),
        "zip" => (
            PackageId::new(&registry, module, version).with_artifact("zip"),
            "application/zip",
            Action::SourceRead,
        ),
        _ => {
            return Err(AppError::not_found(format!(
                "unknown goproxy file extension '.{ext}'"
            )));
        }
    };

    proxy_stream(svc, pkg, identity, action, Some(content_type)).await
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn make_zip_with_go_mod(entry_name: &str, content: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        zip.start_file(entry_name, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content.as_bytes()).unwrap();
        zip.finish().unwrap();
        buf
    }

    #[test]
    fn extract_go_mod_exact_name_match() {
        let content = "module github.com/foo/bar\n\ngo 1.21\n";
        let zip = make_zip_with_go_mod("github.com/foo/bar@v1.0.0/go.mod", content);
        let result = extract_go_mod(&zip, "github.com/foo/bar", "v1.0.0");
        assert_eq!(result, content);
    }

    #[test]
    fn extract_go_mod_fallback_suffix_match() {
        let content = "module example.com/mod\n\ngo 1.22\n";
        let zip = make_zip_with_go_mod("example.com/mod@v2.0.0/go.mod", content);
        // Pass a different module/version — falls back to suffix match
        let result = extract_go_mod(&zip, "other/path", "v0.0.0");
        assert_eq!(result, content);
    }

    #[test]
    fn extract_go_mod_not_found_returns_minimal_fallback() {
        let zip = make_zip_with_go_mod("README.md", "hello");
        let result = extract_go_mod(&zip, "github.com/foo/bar", "v1.0.0");
        assert!(result.contains("module github.com/foo/bar"));
        assert!(result.contains("go 1.21"));
    }

    #[test]
    fn extract_go_mod_invalid_zip_returns_fallback() {
        let result = extract_go_mod(b"not a zip", "example.com/mod", "v1.0.0");
        assert!(result.contains("module example.com/mod"));
    }
}
