use std::sync::Arc;

use actix_web::{put, web, HttpResponse, Responder};
use base64::Engine as _;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tracing::Instrument as _;

use batlehub_core::{
    entities::NotificationEventType,
    services::{LocalRegistryService, PublishRequest},
};

use crate::handlers::proxy::common::{
    collect_payload, dispatch_notification, extract_signature_headers, require_local_mode,
    ArtifactSignature,
};
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};

use super::require_npm;

/// npm's publish acknowledgement: an empty JSON object.
///
/// `npm publish` reports success from the status code and the quota headers;
/// the body carries nothing, and this type says so rather than leaving the
/// response undocumented.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct NpmPublishResponse {}

/// Publish a new npm package version (`npm publish`).
///
/// Accepts the standard npm publish wire format: a JSON body containing the
/// package metadata under `versions` and the base64-encoded tarball under
/// `_attachments`.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/{name}",
    tag = "proxy/npm",
    params(("registry" = String, Path, description = "Registry name"),
           ("name" = String, Path, description = "Package name")),
    request_body(content_type = "application/json", description = "npm publish payload"),
    responses(
        (status = 200, description = "Package published", body = NpmPublishResponse),
        (status = 400, description = "Invalid payload"),
        (status = 403, description = "Access denied"),
        (status = 409, description = "Version already published"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[put("/proxy/{registry}/{name}")]
pub async fn npm_publish(
    req: actix_web::HttpRequest,
    path: web::Path<(String, String)>,
    payload: web::Payload,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    require_npm(&registry, &map)?;
    require_local_mode(&registry, &mode_map)?;

    let raw = collect_payload(payload)
        .instrument(batlehub_core::services::stage("publish_body"))
        .await?;

    let body: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| AppError::bad_request(format!("invalid JSON: {e}")))?;

    // npm publish sends exactly one version per request.
    let versions = body
        .get("versions")
        .and_then(|v| v.as_object())
        .ok_or_else(|| AppError::bad_request("missing 'versions' object"))?;
    let (version_str, version_meta) = versions
        .iter()
        .next()
        .ok_or_else(|| AppError::bad_request("'versions' is empty"))?;

    let attachments = body
        .get("_attachments")
        .and_then(|a| a.as_object())
        .ok_or_else(|| AppError::bad_request("missing '_attachments'"))?;
    let (_filename, attachment) = attachments
        .iter()
        .next()
        .ok_or_else(|| AppError::bad_request("'_attachments' is empty"))?;

    let data_b64 = attachment
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| AppError::bad_request("missing 'data' in attachment"))?;

    let tarball_bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| AppError::bad_request(format!("invalid base64 in attachment: {e}")))?;
    let tarball_bytes = Bytes::from(tarball_bytes);

    let checksum = hex::encode(Sha256::digest(&tarball_bytes));

    // Strip the tarball URL — it will be rewritten dynamically when serving.
    let mut meta = version_meta.clone();
    if let Some(obj) = meta.as_object_mut() {
        if let Some(dist) = obj.get_mut("dist").and_then(|d| d.as_object_mut()) {
            dist.remove("tarball");
        }
    }

    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);
    let actor = identity.0.user_id.clone().unwrap_or_default();

    let quota = local_svc
        .publish(PublishRequest {
            unlisted: false,
            registry: registry.clone(),
            name: name.clone(),
            version: version_str.clone(),
            artifact: tarball_bytes,
            checksum,
            index_metadata: meta,
            publisher: identity.0.clone(),
            signature_bytes,
            signature_type,
        })
        .await
        .map_err(AppError::from)?;

    // The README is not index data — widening `index_metadata` would change
    // what package managers receive — so it is stored separately, keyed by the
    // version it was published with (RFC 0007 §6.4).
    //
    // The version object's own `readme` first, then the document root's. The
    // root is package-level in the packument, but on a publish it describes
    // exactly the version being published: this *is* that version, and there is
    // no older one it could be describing.
    if let Some(text) = publish_readme(version_meta, &body) {
        local_svc
            .record_publish_readme(
                &registry,
                &name,
                version_str,
                text,
                // npm READMEs are markdown by convention and by what every npm
                // client renders. Nothing in the publish document declares
                // otherwise, so there is nothing else to read it from.
                batlehub_core::entities::ReadmeFormat::Markdown,
            )
            .instrument(batlehub_core::services::stage("publish_readme"))
            .await;
    }

    dispatch_notification(
        &notification_svc,
        NotificationEventType::PackagePublished,
        &registry,
        &name,
        Some(version_str.clone()),
        &actor,
    );

    let mut resp = HttpResponse::Ok();
    for (k, v) in quota.headers() {
        resp.insert_header((k, v));
    }
    Ok(resp.json(NpmPublishResponse {}))
}

/// The README text an npm publish document carries, if any.
///
/// The version object's `readme` first, then the document root's. npm's own
/// placeholder for "the tarball had no README" is a string, so a check for
/// presence alone would store an error message as documentation.
fn publish_readme(version_meta: &serde_json::Value, body: &serde_json::Value) -> Option<String> {
    /// npm writes this rather than omitting the field.
    const MISSING: &str = "ERROR: No README data found!";

    [version_meta.get("readme"), body.get("readme")]
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .find(|t| !t.is_empty() && *t != MISSING)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_versions_own_readme_wins_over_the_document_root() {
        let version = serde_json::json!({ "readme": "# this version" });
        let body = serde_json::json!({ "readme": "# the package" });
        assert_eq!(
            publish_readme(&version, &body).as_deref(),
            Some("# this version")
        );
    }

    /// A publish document's root README describes exactly the version being
    /// published — there is no older version it could belong to — so it is used
    /// when the version object carries none.
    #[test]
    fn the_root_readme_is_used_when_the_version_has_none() {
        let body = serde_json::json!({ "readme": "# the package" });
        assert_eq!(
            publish_readme(&serde_json::json!({}), &body).as_deref(),
            Some("# the package")
        );
    }

    /// npm's placeholder is a string, so a presence check alone would store an
    /// error message as documentation.
    #[test]
    fn npms_missing_readme_placeholder_is_not_a_readme() {
        let version = serde_json::json!({ "readme": "ERROR: No README data found!" });
        let body = serde_json::json!({ "readme": "   \n " });
        assert_eq!(publish_readme(&version, &body), None);
    }

    #[test]
    fn a_document_with_no_readme_anywhere_yields_none() {
        assert_eq!(
            publish_readme(&serde_json::json!({}), &serde_json::json!({})),
            None
        );
        // A non-string `readme` is not a README either.
        assert_eq!(
            publish_readme(
                &serde_json::json!({ "readme": 42 }),
                &serde_json::json!({ "readme": null })
            ),
            None
        );
    }
}
