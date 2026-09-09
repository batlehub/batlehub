use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};
use bytes::Bytes;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    ports::DocumentKind,
    services::{artifact_storage_key, validate_coordinate, LocalRegistryService, ProxyService},
};

use super::super::common::{
    append_signature_headers, proxy_document, proxy_stream, require_registry_type,
};
use super::nuspec::{content_type_for, extract_nuspec_from_nupkg};
use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

// ── Flat container — version list ─────────────────────────────────────────────

/// Return the list of available versions for a NuGet package (flat container).
///
/// In `local`/`hybrid` mode this is generated from locally published packages.
/// In `proxy` mode it is fetched from the upstream flat container.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nuget/v3/flat/{id}/index.json",
    tag = "proxy/nuget",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id" = String, Path, description = "Package ID (case-insensitive)"),
    ),
    responses(
        (status = 200, description = "Version list JSON", body = UpstreamDocument),
        (status = 404, description = "Package not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nuget/v3/flat/{id}/index.json")]
pub async fn nuget_flat_versions(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, id_raw) = path.into_inner();
    require_registry_type(&registry, "nuget", &map)?;

    let id = id_raw.to_lowercase();
    let mode = mode_map.get(&registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        // Enforce registry RBAC before the local version listing (the proxy
        // fall-through runs the rule chain; a local hit otherwise bypasses it).
        svc.authorize_read(
            &PackageId::new(&registry, &id, "__index__"),
            &identity.0,
            Action::ReleasesRead,
        )
        .await
        .map_err(AppError::from)?;
        match local_svc
            .get_nuget_versions(&registry, &id, &identity)
            .await
        {
            Ok(versions) => {
                let version_list: Vec<&str> = versions
                    .iter()
                    .filter(|v| !v.yanked)
                    .map(|v| v.version.as_str())
                    .collect();
                let body = serde_json::json!({ "versions": version_list });
                return Ok(HttpResponse::Ok()
                    .content_type("application/json")
                    .json(body));
            }
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {
                // fall through to upstream proxy
            }
            Err(CoreError::NotFound(msg)) => return Err(AppError::not_found(msg)),
            Err(e) => return Err(AppError::from(e)),
        }
    }

    // Proxy mode or hybrid miss. `proxy_document` rather than `proxy_stream`:
    // this is the document `dotnet restore` resolves a version range against,
    // so it has to have administratively blocked versions removed before a
    // resolver picks one and is then refused the download.
    proxy_document(
        svc,
        PackageId::new(&registry, &id, "__index__"),
        identity,
        Action::ReleasesRead,
        DocumentKind::Versions,
        String::new(),
    )
    .await
}

// ── Flat container — artifact download ───────────────────────────────────────

/// Download a NuGet package artifact (`.nupkg`, `.nuspec`, checksum, etc.).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/nuget/v3/flat/{id}/{version}/{filename}",
    tag = "proxy/nuget",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id"       = String, Path, description = "Package ID"),
        ("version"  = String, Path, description = "Package version"),
        ("filename" = String, Path, description = "Artifact filename"),
    ),
    responses(
        (status = 200, description = "Artifact bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 404, description = "Artifact not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/nuget/v3/flat/{id}/{version}/{filename}")]
pub async fn nuget_flat_download(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, id_raw, version, filename) = path.into_inner();
    require_registry_type(&registry, "nuget", &map)?;

    let id = id_raw.to_lowercase();
    // Edge chokepoint for the local branch, which builds a storage key directly
    // (the proxy branch is guarded inside `ProxyService::handle`).
    validate_coordinate(&id, &version, Some(&filename)).map_err(AppError::from)?;
    let mode = mode_map.get(&registry);

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        // The `.nuspec` variant is extracted from the same stored `.nupkg`, so
        // the key is built here — but the read goes through
        // `get_artifact_at_key`, which applies the rule chain **and**
        // `check_visibility`. The chain was already enforced on this route; the
        // visibility check was not, so the flat *index* refused an
        // Internal-visibility package while the `.nupkg` beside it was served
        // (survey finding 6).
        let storage_key = artifact_storage_key(&registry, &id, &version);
        // `filename` distinguishes the `.nupkg` from the `.nuspec` extracted out
        // of it, both in the audit trail and in the download count — the two are
        // one stored blob but two requests, and `nuget restore` makes both.
        let pkg = batlehub_core::entities::PackageId::new(&registry, &id, &version)
            .with_artifact(&filename);
        match local_svc
            .get_artifact_at_key(
                &pkg,
                &storage_key,
                Action::ReleasesRead,
                &identity,
                &identity.1,
            )
            .await
        {
            Ok(Some(buf)) => {
                let body = if filename.ends_with(".nuspec") {
                    Bytes::from(extract_nuspec_from_nupkg(&buf)?)
                } else {
                    // Already an owned `Bytes`; respond with it directly instead of
                    // copying the whole artifact into a fresh `Vec<u8>`.
                    buf
                };
                let mut resp = HttpResponse::Ok();
                resp.content_type(content_type_for(&filename));
                append_signature_headers(&mut resp, &local_svc, &registry, &id, &version).await;
                return Ok(resp.body(body));
            }
            Ok(None) if mode == RegistryMode::Hybrid => {} // fall through
            Ok(None) => {
                return Err(AppError::not_found(format!(
                    "{id}@{version} not found in local registry"
                )));
            }
            // Only a storage fault falls through; an authorization refusal is an
            // answer and must not become an upstream request.
            Err(batlehub_core::error::CoreError::Storage(e)) if mode == RegistryMode::Hybrid => {
                tracing::warn!("local storage error, falling back to proxy: {e}");
            }
            Err(e) => return Err(AppError::from(e)),
        }
    }

    proxy_stream(
        svc,
        PackageId::new(&registry, &id, &version).with_artifact(&filename),
        identity,
        Action::ReleasesRead,
        Some(content_type_for(&filename)),
    )
    .await
}
