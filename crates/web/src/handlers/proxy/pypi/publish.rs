use std::sync::Arc;

use actix_multipart::Multipart;
use actix_web::{post, web, HttpRequest, Responder};
use bytes::BytesMut;
use futures::StreamExt;
use sha2::{Digest, Sha256};

use batlehub_core::services::{LocalRegistryService, PublishRequest};

use crate::handlers::proxy::common::{
    extract_signature_headers, publish_and_respond, require_local_mode, require_registry_type,
    ArtifactSignature, MAX_UPLOAD_BYTES,
};
use crate::handlers::schemas::MessageResponse;
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};

// ── Publish route (twine-compatible) ─────────────────────────────────────────

/// Publish a Python distribution (local/hybrid registries only).
///
/// Accepts `multipart/form-data` as produced by `twine upload`.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/legacy/",
    tag = "proxy/pypi",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "File uploaded", body = MessageResponse),
        (status = 400, description = "Malformed multipart, or an unacceptable distribution filename"),
        (status = 403, description = "Access denied or quota exceeded"),
        (status = 409, description = "Version already published"),
        (status = 422, description = "Invalid payload"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[post("/proxy/{registry}/legacy/")]
pub async fn pypi_publish(
    req: HttpRequest,
    path: web::Path<String>,
    mut multipart: Multipart,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "pypi", &map)?;
    require_local_mode(&registry, &mode_map)?;

    let mut action: Option<String> = None;
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut sha2: Option<String> = None;
    let mut content: Option<bytes::Bytes> = None;
    let mut filename: Option<String> = None;
    let mut requires_python: Option<String> = None;

    // Raw `Multipart` is not covered by `PayloadConfig`, so bound the cumulative
    // accumulation ourselves to prevent an unauthenticated OOM. Same ceiling as
    // `collect_payload`.
    let mut total_bytes: u64 = 0;
    while let Some(field_result) = multipart.next().await {
        let mut field =
            field_result.map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?;

        let field_name = field.name().unwrap_or("").to_owned();
        let file_name = field
            .content_disposition()
            .and_then(|cd| cd.get_filename())
            .map(str::to_owned);

        let mut buf = BytesMut::new();
        while let Some(chunk) = field.next().await {
            let chunk = chunk.map_err(|e| AppError::bad_request(format!("chunk error: {e}")))?;
            total_bytes += chunk.len() as u64;
            if total_bytes > MAX_UPLOAD_BYTES {
                return Err(AppError::from(
                    batlehub_core::error::CoreError::PayloadTooLarge(format!(
                        "upload exceeds the {MAX_UPLOAD_BYTES}-byte limit"
                    )),
                ));
            }
            buf.extend_from_slice(&chunk);
        }
        let bytes = buf.freeze();

        match field_name.as_str() {
            ":action" => action = Some(String::from_utf8_lossy(&bytes).into_owned()),
            "name" => name = Some(String::from_utf8_lossy(&bytes).into_owned()),
            "version" => version = Some(String::from_utf8_lossy(&bytes).into_owned()),
            "sha2" => sha2 = Some(String::from_utf8_lossy(&bytes).into_owned()),
            "requires_python" => {
                requires_python = Some(String::from_utf8_lossy(&bytes).into_owned())
                    .filter(|r| !r.trim().is_empty())
            }
            "content" => {
                filename = file_name;
                content = Some(bytes);
            }
            _ => {}
        }
    }

    let action = action.unwrap_or_default();
    if action != "file_upload" {
        return Err(AppError::bad_request(format!(
            "unsupported :action '{action}'; expected 'file_upload'"
        )));
    }

    let name = name.ok_or_else(|| AppError::bad_request("missing 'name' field".to_owned()))?;
    // Stored under the PEP 503 name: the simple index and the file download
    // both look the project up by it, so `My_Pkg` stored verbatim was a
    // project no `pip install` could find.
    let name = batlehub_core::entities::RegistryKind::Pypi
        .canonical_package_name(&name)
        .into_owned();
    // PEP 508's character set, after normalisation: lowercase ASCII, digits and
    // single dashes. Also what keeps a name from carrying a `/`, which the
    // version-directory delete of the extra files relies on.
    if name.is_empty()
        || name.starts_with('-')
        || name.ends_with('-')
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(AppError::bad_request(format!(
            "invalid project name '{name}': PEP 508 allows letters, digits, '-', '_' and '.'"
        )));
    }
    let version =
        version.ok_or_else(|| AppError::bad_request("missing 'version' field".to_owned()))?;
    let content =
        content.ok_or_else(|| AppError::bad_request("missing 'content' field".to_owned()))?;
    // The filename arrives verbatim from the `content` part's
    // `Content-Disposition`, and `enforce_publish_policy` validates only `name`
    // and `version` — so this is the only thing standing between a hostile
    // `filename` and `index_metadata`, which the Simple index reads back. The
    // synthesised fallback is validated too: it is built from `name`, and a
    // name is only checked for path-safety, not for a distribution's character
    // set.
    let filename = filename.unwrap_or_else(|| format!("{name}-{version}.tar.gz"));
    super::validate_distribution_filename(&filename).map_err(AppError::bad_request)?;

    let computed_checksum = hex::encode(Sha256::digest(&content));

    if let Some(ref client_sha2) = sha2 {
        if client_sha2 != &computed_checksum {
            return Err(AppError::bad_request("sha2 checksum mismatch".to_owned()));
        }
    }

    // A later file of a version already published here (the wheel after the
    // sdist): added to that version rather than refused as a duplicate publish.
    if let Some(existing) = local_svc
        .pypi_version_filenames(&registry, &name, &version)
        .await
        .map_err(AppError::from)?
    {
        if existing.contains(&filename) {
            return Err(AppError::from(batlehub_core::error::CoreError::Conflict(
                format!("{filename} already exists in {name}@{version}"),
            )));
        }
        local_svc
            .add_pypi_file(
                batlehub_core::services::PypiFileUpload {
                    registry: &registry,
                    name: &name,
                    version: &version,
                    filename: &filename,
                    bytes: content,
                    sha256: &computed_checksum,
                    requires_python: requires_python.as_deref(),
                },
                &identity.0,
            )
            .await
            .map_err(AppError::from)?;
        return Ok(actix_web::HttpResponse::Ok()
            .json(MessageResponse::new(format!("File uploaded: {filename}"))));
    }

    let mut index_metadata = serde_json::json!({
        "name": name,
        "version": version,
        "filename": filename,
        "sha256": computed_checksum,
    });
    if let Some(r) = requires_python {
        index_metadata["requires_python"] = serde_json::Value::String(r);
    }

    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);

    publish_and_respond(
        &local_svc,
        &notification_svc,
        PublishRequest {
            unlisted: false,
            registry,
            name,
            version,
            artifact: content,
            checksum: computed_checksum,
            index_metadata,
            publisher: identity.0,
            signature_bytes,
            signature_type,
        },
        actix_web::http::StatusCode::OK,
        MessageResponse::new(format!("File uploaded: {filename}")),
    )
    .await
}
