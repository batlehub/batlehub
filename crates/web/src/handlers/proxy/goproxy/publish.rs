use std::sync::Arc;

use actix_web::{put, web, HttpRequest, Responder};
use sha2::{Digest, Sha256};

use batlehub_core::services::{LocalRegistryService, PublishRequest};

use crate::handlers::proxy::common::{
    collect_payload, extract_signature_headers, publish_and_respond, require_local_mode,
    require_registry_type, ArtifactSignature,
};
use crate::{
    error::AppError, extractors::AuthIdentity, services::NotificationService, RegistryMap,
    RegistryModeMap,
};

use super::read::extract_go_mod;
use crate::handlers::schemas::OkResponse;

/// Publish a Go module version by uploading its zip archive.
///
/// The zip must follow the Go module zip format: all entries prefixed with
/// `{module}@{version}/`. The `go.mod` is extracted automatically from the
/// archive. Version metadata (`.info`) is generated from the version string
/// and the current timestamp.
///
/// The module path is inferred from the URL; the version from the filename
/// (`{version}.zip`).
#[utoipa::path(
    put,
    path = "/proxy/{registry}/{module}/@v/{filename}",
    tag = "proxy/goproxy",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("module"   = String, Path, description = "Go module path (may contain slashes)"),
        ("filename" = String, Path, description = "Version zip: {version}.zip"),
    ),
    request_body(content_type = "application/zip", description = "Go module zip archive"),
    responses(
        (status = 200, description = "Module published", body = OkResponse),
        (status = 400, description = "Invalid payload or filename"),
        (status = 403, description = "Access denied"),
        (status = 409, description = "Version already published"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[put("/proxy/{registry}/{module:[^@]+}@v/{filename}")]
pub async fn goproxy_publish(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    payload: web::Payload,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, raw_module, filename) = path.into_inner();
    require_registry_type(&registry, "goproxy", &map)?;
    require_local_mode(&registry, &mode_map)?;
    let (raw_version, ext) = filename
        .rsplit_once('.')
        .ok_or_else(|| AppError::bad_request(format!("invalid filename '{filename}'")))?;
    // Stored under the protocol's case encoding (`!acme`), the one spelling a
    // Go client asks for, whichever spelling was uploaded; the zip itself
    // names the real path. Stored verbatim, `Acme` was a module no client
    // could fetch, and in hybrid the public `!acme` answered in its place.
    let real_module = go_unescape(raw_module.trim_end_matches('/'));
    let real_version = go_unescape(raw_version);
    let go = batlehub_core::entities::RegistryKind::Goproxy;
    let module = go.canonical_package_name(&real_module).into_owned();
    let version = go.canonical_package_name(&real_version).into_owned();
    let version = version.as_str();
    if ext != "zip" {
        return Err(AppError::bad_request(format!(
            "only .zip uploads are supported (got '.{ext}')"
        )));
    }

    let zip_bytes = collect_payload(payload).await?;
    let checksum = hex::encode(Sha256::digest(&zip_bytes));

    let go_mod = extract_go_mod(&zip_bytes, &real_module, &real_version);
    let now = chrono::Utc::now().to_rfc3339();
    let index_metadata = serde_json::json!({
        "Version": version,
        "Time": now,
        "go_mod": go_mod
    });

    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);

    publish_and_respond(
        &local_svc,
        &notification_svc,
        PublishRequest {
            unlisted: false,
            registry,
            name: module,
            version: version.to_owned(),
            artifact: zip_bytes,
            checksum,
            index_metadata,
            publisher: identity.0,
            signature_bytes,
            signature_type,
        },
        actix_web::http::StatusCode::OK,
        OkResponse::new(),
    )
    .await
}

/// The inverse of the GOPROXY case encoding: `!a` is `A`.
fn go_unescape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match (c, c == '!') {
            (_, true) => out.extend(chars.next().map(|n| n.to_ascii_uppercase())),
            (c, false) => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn go_unescape_reverses_the_case_encoding() {
        assert_eq!(
            super::go_unescape("github.com/!acme/!x"),
            "github.com/Acme/X"
        );
        assert_eq!(super::go_unescape("v1.0.0-!r!c1"), "v1.0.0-RC1");
        assert_eq!(super::go_unescape("plain"), "plain");
    }
}
