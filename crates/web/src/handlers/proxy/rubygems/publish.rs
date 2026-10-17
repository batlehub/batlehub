use super::{
    collect_payload, delete, extract_signature_headers, post, put, require_local_mode,
    require_registry_type, web, AppError, Arc, ArtifactSignature, AuthIdentity, Digest,
    HttpRequest, HttpResponse, LocalRegistryService, NotificationEventType, NotificationService,
    PublishRequest, RegistryMap, RegistryModeMap, Responder, Sha256,
};
use crate::handlers::schemas::MessageResponse;
use batlehub_core::error::CoreError;

#[derive(serde::Deserialize)]
pub struct GemYankQuery {
    pub gem_name: String,
    pub version: String,
}

/// Publish a gem (local/hybrid registries only).
///
/// Accepts the raw `.gem` file bytes in the request body.
/// Compatible with `gem push`.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/api/v1/gems",
    tag = "proxy/rubygems",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Gem published successfully", body = MessageResponse),
        (status = 400, description = "Malformed signature headers, or a gemspec field outside its grammar"),
        (status = 403, description = "Access denied or quota exceeded"),
        (status = 409, description = "Version already exists"),
        (status = 422, description = "Invalid gem file or versioning policy violation"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[post("/proxy/{registry}/api/v1/gems")]
pub async fn gem_publish(
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Payload,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;
    require_local_mode(&registry, &mode_map)?;

    let data = collect_payload(payload).await?;

    let gem_meta =
        batlehub_adapters::registry::rubygems::parse_gem_bytes(&data).map_err(|e| match e {
            // A field outside the gemspec grammar (a traversal, a delimiter) is
            // a bad coordinate: 400, like every other publisher's.
            CoreError::InvalidInput(_) => AppError::from(e),
            e => AppError::unprocessable(e.to_string()),
        })?;

    let checksum = hex::encode(Sha256::digest(&data));

    let index_metadata = serde_json::json!({
        "name": gem_meta.name,
        "version": gem_meta.version,
        "platform": gem_meta.platform,
        "summary": gem_meta.summary,
        "authors": gem_meta.authors,
        "sha": checksum,
        // The compact index carries dependencies inline, and it is the document
        // Bundler resolves from — a gem stored without them resolves as though
        // it needed nothing (RFC 0009 §12.15).
        "dependencies": gem_meta
            .dependencies
            .iter()
            .map(|d| serde_json::json!({ "name": d.name, "requirement": d.requirement }))
            .collect::<Vec<_>>(),
    });

    let name = gem_meta.name.clone();
    let version = gem_meta.version.clone();

    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);

    super::super::common::publish_and_respond(
        &local_svc,
        &notification_svc,
        PublishRequest {
            unlisted: false,
            registry,
            name: name.clone(),
            version: version.clone(),
            artifact: data,
            checksum,
            index_metadata,
            publisher: identity.0,
            signature_bytes,
            signature_type,
        },
        actix_web::http::StatusCode::OK,
        MessageResponse::new(format!("Successfully registered gem: {name} ({version})")),
    )
    .await
}

/// Yank a gem version (local/hybrid registries only).
#[utoipa::path(
    delete,
    path = "/proxy/{registry}/api/v1/gems/yank",
    tag = "proxy/rubygems",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("gem_name"  = String, Query, description = "Gem name"),
        ("version"   = String, Query, description = "Gem version to yank"),
    ),
    responses(
        (status = 200, description = "Gem yanked", body = MessageResponse),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Gem or version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/proxy/{registry}/api/v1/gems/yank")]
pub async fn gem_yank(
    path: web::Path<String>,
    query: web::Query<GemYankQuery>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;
    require_local_mode(&registry, &mode_map)?;

    let actor = identity.0.user_id.clone().unwrap_or_default();
    local_svc
        .yank(&registry, &query.gem_name, &query.version, &identity.0)
        .await
        .map_err(AppError::from)?;

    super::super::common::dispatch_notification(
        &notification_svc,
        NotificationEventType::PackageYanked,
        &registry,
        &query.gem_name,
        Some(query.version.clone()),
        &actor,
    );

    Ok(HttpResponse::Ok().json(MessageResponse::new(format!(
        "Successfully yanked gem: {} ({})",
        query.gem_name, query.version
    ))))
}

/// Unyank a gem version (local/hybrid registries only).
#[utoipa::path(
    put,
    path = "/proxy/{registry}/api/v1/gems/unyank",
    tag = "proxy/rubygems",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("gem_name"  = String, Query, description = "Gem name"),
        ("version"   = String, Query, description = "Gem version to unyank"),
    ),
    responses(
        (status = 200, description = "Gem unyanked", body = MessageResponse),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Gem or version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/api/v1/gems/unyank")]
pub async fn gem_unyank(
    path: web::Path<String>,
    query: web::Query<GemYankQuery>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_registry_type(&registry, "rubygems", &map)?;
    require_local_mode(&registry, &mode_map)?;

    let actor = identity.0.user_id.clone().unwrap_or_default();
    local_svc
        .unyank(&registry, &query.gem_name, &query.version, &identity.0)
        .await
        .map_err(AppError::from)?;

    super::super::common::dispatch_notification(
        &notification_svc,
        NotificationEventType::PackageUnyanked,
        &registry,
        &query.gem_name,
        Some(query.version.clone()),
        &actor,
    );

    Ok(HttpResponse::Ok().json(MessageResponse::new(format!(
        "Successfully unyanked gem: {} ({})",
        query.gem_name, query.version
    ))))
}
