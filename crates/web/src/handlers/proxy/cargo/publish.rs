use super::helpers::{
    crate_names_collide, local_crate_key, metadata_to_index_entry, parse_publish_body,
    stored_crate_name,
};
use super::{
    collect_payload, delete, extract_signature_headers, put, require_cargo, require_local_mode,
    web, AppError, Arc, ArtifactSignature, AuthIdentity, CoreError, Digest, HttpResponse,
    LocalRegistryService, NotificationEventType, NotificationService, PublishRequest, RegistryMap,
    RegistryModeMap, Responder, Sha256,
};
use batlehub_core::services::readme::detect;

use crate::handlers::schemas::OkResponse;

/// `cargo publish`'s acknowledgement, in the shape crates.io defines: a
/// `warnings` object the client prints back to the user.
///
/// This server does not validate categories or badges, so all three lists are
/// always empty — but the client reads the keys, so they are always present.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct CratePublishResponse {
    pub warnings: CratePublishWarnings,
}

/// The three warning buckets `cargo` looks for in a publish response.
#[derive(Default, serde::Serialize, utoipa::ToSchema)]
pub struct CratePublishWarnings {
    pub invalid_categories: Vec<String>,
    pub invalid_badges: Vec<String>,
    pub other: Vec<String>,
}

/// Publish a new crate version (`cargo publish`).
#[utoipa::path(
    put,
    path = "/proxy/{registry}/api/v1/crates/new",
    tag = "proxy/cargo",
    params(("registry" = String, Path, description = "Registry name")),
    request_body(content_type = "application/octet-stream", description = "Cargo publish binary payload (length-prefixed metadata + .crate bytes)"),
    responses(
        (status = 200, description = "Crate published successfully", body = CratePublishResponse),
        (status = 400, description = "Invalid publish payload"),
        (status = 403, description = "Access denied"),
        (status = 409, description = "Version already published"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[put("/proxy/{registry}/api/v1/crates/new")]
pub async fn cargo_publish(
    req: actix_web::HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_cargo(&registry, &map)?;
    require_local_mode(&registry, &mode_map)?;

    let body = collect_payload(payload).await?;

    let (meta_json, crate_bytes) = parse_publish_body(body).map_err(AppError::bad_request)?;

    let declared = meta_json
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("missing 'name' in publish metadata"))?;
    // A new crate must not shadow an existing one under crates.io's rule:
    // `foo_bar` beside `foo-bar` is two crates cargo cannot tell apart.
    let existing = local_svc
        .backend
        .list_package_names(&registry)
        .await
        .map_err(AppError::from)?;
    let name = stored_crate_name(&existing, declared);
    if let Some(other) = existing
        .iter()
        .find(|n| **n != name && crate_names_collide(n, &name))
    {
        return Err(AppError::from(CoreError::Conflict(format!(
            "crate name '{declared}' collides with the existing crate '{other}'"
        ))));
    }
    let version = meta_json
        .get("vers")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("missing 'vers' in publish metadata"))?
        .to_owned();

    let checksum = hex::encode(Sha256::digest(&crate_bytes));

    let mut entry =
        metadata_to_index_entry(&meta_json, &checksum).map_err(AppError::bad_request)?;

    // Cargo-specific: validate caller-declared checksum against computed value.
    if !entry.cksum.is_empty() && entry.cksum != checksum {
        return Err(AppError::bad_request(format!(
            "checksum mismatch: declared {} but computed {}",
            entry.cksum, checksum
        )));
    }
    entry.cksum = checksum.clone();

    let index_metadata =
        serde_json::to_value(&entry).map_err(|e| AppError::bad_request(e.to_string()))?;

    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);
    let actor = identity.0.user_id.clone().unwrap_or_default();

    let quota = local_svc
        .publish(PublishRequest {
            unlisted: false,
            registry: registry.clone(),
            name: name.clone(),
            version: version.clone(),
            artifact: crate_bytes,
            checksum,
            index_metadata,
            publisher: identity.0.clone(),
            signature_bytes,
            signature_type,
        })
        .await
        .map_err(AppError::from)?;

    // `metadata_to_index_entry` narrows the publish metadata to the nine fields
    // the sparse index carries, and `readme`/`readme_file` are not among them —
    // widening the index entry would change what cargo receives. So the text is
    // stored separately, keyed by the version it was published with
    // (RFC 0007 §6.4). This is the only source cargo has: the sparse index has
    // no README field, so a proxied crate's README comes from the `.crate`.
    if let Some(text) = publish_readme(&meta_json) {
        local_svc
            .record_publish_readme(
                &registry,
                &name,
                &version,
                text,
                // `readme_file` names the file the text came from, so its
                // extension is what declares the markup — the same rule the
                // archive extractor applies to the same file.
                detect::format_from_filename(
                    meta_json
                        .get("readme_file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("README.md"),
                ),
            )
            .await;
    }

    super::super::common::dispatch_notification(
        &notification_svc,
        NotificationEventType::PackagePublished,
        &registry,
        &name,
        Some(version),
        &actor,
    );

    let mut resp = HttpResponse::Ok();
    for (k, v) in quota.headers() {
        resp.insert_header((k, v));
    }
    Ok(resp.json(CratePublishResponse {
        warnings: CratePublishWarnings::default(),
    }))
}

/// Yank a published crate version.
#[utoipa::path(
    delete,
    path = "/proxy/{registry}/api/v1/crates/{name}/{version}/yank",
    tag = "proxy/cargo",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Crate name"),
        ("version"  = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = "Yanked", body = OkResponse),
        (status = 403, description = "Access denied"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/proxy/{registry}/api/v1/crates/{name}/{version}/yank")]
pub async fn cargo_yank(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, name, version) = path.into_inner();
    require_cargo(&registry, &map)?;
    require_local_mode(&registry, &mode_map)?;
    let name = local_crate_key(&local_svc, &registry, &name).await?;
    let actor = identity.0.user_id.clone().unwrap_or_default();
    local_svc
        .yank(&registry, &name, &version, &identity.0)
        .await
        .map_err(AppError::from)?;
    super::super::common::dispatch_notification(
        &notification_svc,
        NotificationEventType::PackageYanked,
        &registry,
        &name,
        Some(version),
        &actor,
    );
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// Unyank a previously yanked crate version.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/api/v1/crates/{name}/{version}/unyank",
    tag = "proxy/cargo",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Crate name"),
        ("version"  = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = "Unyanked", body = OkResponse),
        (status = 403, description = "Access denied"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/api/v1/crates/{name}/{version}/unyank")]
pub async fn cargo_unyank(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, name, version) = path.into_inner();
    require_cargo(&registry, &map)?;
    require_local_mode(&registry, &mode_map)?;
    let name = local_crate_key(&local_svc, &registry, &name).await?;
    let actor = identity.0.user_id.clone().unwrap_or_default();
    local_svc
        .unyank(&registry, &name, &version, &identity.0)
        .await
        .map_err(AppError::from)?;
    super::super::common::dispatch_notification(
        &notification_svc,
        NotificationEventType::PackageUnyanked,
        &registry,
        &name,
        Some(version),
        &actor,
    );
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// The README text a cargo publish request carried in its metadata.
///
/// `cargo publish` sends the full text in `readme` and the file it came from in
/// `readme_file`. Both are dropped by `metadata_to_index_entry`, which is
/// correct — they are not index data — so this reads them before that happens.
fn publish_readme(meta_json: &serde_json::Value) -> Option<String> {
    meta_json
        .get("readme")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_publish_metadatas_readme_is_read_before_the_index_entry_drops_it() {
        let meta = serde_json::json!({
            "name": "mylib", "vers": "1.0.0",
            "readme": "# mylib\n\nDoes a thing.",
            "readme_file": "README.md",
        });
        assert_eq!(
            publish_readme(&meta).as_deref(),
            Some("# mylib\n\nDoes a thing.")
        );
    }

    /// The workspace's own publish fixture sends `"readme": null`, which is a
    /// crate with no README rather than one with an empty document.
    #[test]
    fn a_null_or_empty_readme_is_no_readme() {
        for value in [
            serde_json::json!({ "readme": null }),
            serde_json::json!({ "readme": "" }),
            serde_json::json!({ "readme": "  \n " }),
            serde_json::json!({}),
        ] {
            assert_eq!(publish_readme(&value), None);
        }
    }

    /// `readme_file` names the file the text came from, so its extension is
    /// what declares the markup — an `.rst` README is shown as escaped source
    /// rather than parsed as markdown.
    #[test]
    fn readme_file_decides_the_markup() {
        assert_eq!(
            detect::format_from_filename("README.rst"),
            batlehub_core::entities::ReadmeFormat::Rst
        );
        // Absent, the conventional default is markdown — which is what cargo's
        // own `readme = true` shorthand means.
        assert_eq!(
            detect::format_from_filename("README.md"),
            batlehub_core::entities::ReadmeFormat::Markdown
        );
    }
}
