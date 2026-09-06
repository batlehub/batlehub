use std::sync::Arc;

use actix_web::{get, put, web, HttpRequest, HttpResponse, Responder};
use sha2::{Digest, Sha256};

use batlehub_core::services::{LocalRegistryService, ProxyService, PublishRequest};

use super::common::{
    attachment_disposition, collect_payload, extract_signature_headers, require_local_mode,
    serve_local_or_proxy_artifact, ArtifactSignature, LocalOrProxyArtifactOpts,
};
use crate::handlers::schemas::{ArtifactBytes, OkResponse};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

/// The `PackageId::artifact` sub-coordinate a VSIX is stored and read under.
///
/// Must equal `RegistryKind::{Openvsx,VscodeMarketplace}.warm_artifact()`, or
/// the warmer pre-fetches into a slot the download path never looks in. The
/// unit test at the bottom of this file is the drift guard, matching the one
/// `jetbrains_marketplace/mod.rs` keeps for `PLUGIN_ARTIFACT`.
pub(crate) const VSIX_ARTIFACT: &str = "vsix";

pub fn require_openvsx(registry: &str, map: &RegistryMap) -> Result<(), AppError> {
    match map.type_of(registry).as_deref() {
        Some("openvsx") | Some("vscode-marketplace") => Ok(()),
        Some(_) => Err(AppError::not_found(format!(
            "registry '{registry}' is not an openvsx or vscode-marketplace registry"
        ))),
        None => Err(AppError::not_found(format!(
            "unknown registry '{registry}'"
        ))),
    }
}

/// Download a VS Code extension VSIX package.
///
/// Extension IDs follow the `{publisher}.{name}` convention (e.g. `ms-python.python`).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{extension_id}/{version}/vsix",
    tag = "proxy/openvsx",
    params(
        ("registry"     = String, Path, description = "Registry name"),
        ("extension_id" = String, Path, description = "Extension ID in publisher.name format"),
        ("version"      = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = "VS Code extension VSIX package", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown registry or extension"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{extension_id}/{version}/vsix")]
pub async fn download_vsix(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, extension_id, version) = path.into_inner();
    require_openvsx(&registry, &map)?;

    // Through the shared helper rather than a hand-rolled local/hybrid branch.
    // The hand-rolled version read straight from local storage without calling
    // `svc.authorize_read`, so a local or hybrid hit answered without ever
    // consulting the registry's `[registries.rbac]` chain — the proxy
    // fall-through ran it, the local path did not. Every other ecosystem goes
    // through this helper precisely so that cannot happen.
    let mut resp = serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        &extension_id,
        &version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: VSIX_ARTIFACT,
            local_content_type: "application/octet-stream",
            proxy_content_type: None,
            action: Action::SourceRead,
            check_prerelease: true,
            append_signature: true,
        },
    )
    .await?;
    // `{namespace}.{extension}-{version}.vsix` is OpenVSX's own name for the
    // file — the same one its `…/file/{filename}` route serves it under — so a
    // browser saving from here and a browser saving from upstream write the same
    // name, rather than one of them writing `vsix`.
    resp.headers_mut().insert(
        actix_web::http::header::CONTENT_DISPOSITION,
        attachment_disposition(&format!("{extension_id}-{version}.vsix"))?,
    );
    Ok(resp)
}

/// Upload a VS Code extension VSIX package.
///
/// Accepts raw VSIX bytes (ZIP archive). The extension ID follows the
/// `{publisher}.{name}` convention (e.g. `my-org.my-extension`).
#[utoipa::path(
    put,
    path = "/proxy/{registry}/{extension_id}/{version}/vsix",
    tag = "proxy/openvsx",
    params(
        ("registry"     = String, Path, description = "Registry name"),
        ("extension_id" = String, Path, description = "Extension ID in publisher.name format"),
        ("version"      = String, Path, description = "Version"),
    ),
    request_body(content_type = "application/octet-stream", description = "Raw VSIX bytes"),
    responses(
        (status = 200, description = "Extension published", body = OkResponse),
        (status = 400, description = "Invalid payload"),
        (status = 403, description = "Access denied"),
        (status = 409, description = "Version already published"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/{extension_id}/{version}/vsix")]
pub async fn vsix_publish(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    payload: web::Payload,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
) -> Result<impl Responder, AppError> {
    let (registry, extension_id, version) = path.into_inner();
    require_openvsx(&registry, &map)?;
    require_local_mode(&registry, &mode_map)?;

    let vsix_bytes = collect_payload(payload).await?;
    let checksum = hex::encode(Sha256::digest(&vsix_bytes));

    let publisher = extension_id
        .split('.')
        .next()
        .unwrap_or(&extension_id)
        .to_owned();
    // Record what the manifest says, so the gallery can render more than a bare
    // coordinate. Best-effort on purpose: this endpoint takes raw bytes and
    // makes no claim they are a VSIX, so an unreadable manifest degrades to the
    // three keys below rather than refusing the publish. An extension that
    // appears in the editor with no description is worse than one that renders
    // fully, and far better than one that could not be published.
    let index_metadata = match super::vsx::archive::parse_manifest(&vsix_bytes) {
        Some(manifest) => {
            // The URL is the coordinate every other route addresses by, so it
            // wins over a manifest that disagrees — but a disagreement is worth
            // saying out loud, since it usually means the wrong file was
            // uploaded.
            let manifest_id = manifest.extension_id();
            if manifest_id != extension_id || manifest.version != version {
                tracing::warn!(
                    url_coordinate = %format!("{extension_id}@{version}"),
                    manifest_coordinate = %format!("{manifest_id}@{}", manifest.version),
                    "VSIX manifest disagrees with the publish URL; using the URL"
                );
            }
            manifest.index_metadata(&extension_id, &version)
        }
        None => serde_json::json!({
            "id": extension_id,
            "version": version,
            "publisher": publisher,
        }),
    };

    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);

    let signed_bytes = vsix_bytes.clone();
    let (registry_name, ext_name, version_name) =
        (registry.clone(), extension_id.clone(), version.clone());
    let quota = local_svc
        .publish(PublishRequest {
            unlisted: false,
            registry,
            name: extension_id,
            version,
            artifact: vsix_bytes,
            checksum,
            index_metadata,
            publisher: identity.0.clone(),
            signature_bytes,
            signature_type,
        })
        .await
        .map_err(AppError::from)?;
    super::vsx::signing::sign_after_publish(
        &local_svc,
        &registry_name,
        &ext_name,
        &version_name,
        &signed_bytes,
    )
    .await;

    let mut resp = HttpResponse::Ok();
    for (name, value) in quota.headers() {
        resp.insert_header((name, value));
    }
    Ok(resp.json(OkResponse::new()))
}

#[cfg(test)]
mod tests {
    use super::VSIX_ARTIFACT;
    use batlehub_core::entities::{FetchArtifact, RegistryKind};

    /// The warmer and the download path must agree on the cache slot.
    ///
    /// They did not: `warm_artifact()` returned `None` while `download_vsix`
    /// read `.with_artifact("vsix")`, so pre-fetching an extension wrote a key
    /// nothing looked in. This is the same drift guard
    /// `jetbrains_marketplace/mod.rs` keeps for `PLUGIN_ARTIFACT`.
    #[test]
    fn vsix_artifact_matches_the_warmed_sub_coordinate() {
        for kind in [RegistryKind::Openvsx, RegistryKind::VscodeMarketplace] {
            assert_eq!(
                kind.warm_artifact(),
                Some(FetchArtifact::Fixed(VSIX_ARTIFACT)),
                "{kind}: warming and downloading must use the same cache slot"
            );
        }
    }
}

/// `PUT /proxy/{registry}/{extension_id}/{version}/vsix/signature` — attach an
/// upstream's signature archive to a version this registry holds (RFC 0020
/// §13.6). A marketplace extension republished locally keeps, this way, the
/// signature a stock editor verifies; the registry checks the archive's
/// manifest against the stored bytes, keeps it as-is, and never signs over
/// it. Authorised as a publish of the same version.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/{extension_id}/{version}/vsix/signature",
    tag = "proxy/openvsx",
    params(
        ("registry"     = String, Path, description = "Registry name"),
        ("extension_id" = String, Path, description = "Extension ID (publisher.name)"),
        ("version"      = String, Path, description = "Published version"),
    ),
    request_body(content = Vec<u8>, content_type = "application/zip", description = "The signature archive: `.signature.manifest` plus `.signature.p7s` and/or `.signature.sig`"),
    responses(
        (status = 200, description = "Signature attached", body = OkResponse),
        (status = 400, description = "Not a signature archive, or its manifest does not describe the stored VSIX"),
        (status = 403, description = "Not allowed to publish this version"),
        (status = 404, description = "Unknown registry, or version not published here"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/{extension_id}/{version}/vsix/signature")]
pub async fn vsix_signature_attach(
    path: web::Path<(String, String, String)>,
    payload: web::Payload,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
) -> Result<impl Responder, AppError> {
    let (registry, extension_id, version) = path.into_inner();
    require_openvsx(&registry, &map)?;
    require_local_mode(&registry, &mode_map)?;
    batlehub_core::services::validate_package_name(&extension_id).map_err(AppError::from)?;
    batlehub_core::services::validate_path_safe("version", &version).map_err(AppError::from)?;
    let archive = collect_payload(payload).await?;
    local_svc
        .attach_vsix_signature(&registry, &extension_id, &version, archive, &identity.0)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}
