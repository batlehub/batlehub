use super::{
    collect_payload, delete, extract_signature_headers, post, require_local_mode,
    require_registry_type, require_segments, terraform_set_yanked, web, AppError, Arc,
    ArtifactSignature, AuthIdentity, Digest, HttpRequest, LocalRegistryService,
    NotificationService, PublishRequest, RegistryMap, RegistryModeMap, Responder, Sha256,
    TerraformYankRequest,
};
use crate::handlers::schemas::MessageResponse;

/// Upload a Terraform module tarball to the local registry.
///
/// Accepts a `.tar.gz` archive as the request body.
/// Only available when the registry is in `local` or `hybrid` mode.
/// Optionally accepts `X-Artifact-Signature` (base64) and `X-Signature-Type` headers.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Module namespace"),
        ("name"      = String, Path, description = "Module name"),
        ("provider"  = String, Path, description = "Module provider"),
        ("version"   = String, Path, description = "Module version"),
    ),
    responses(
        (status = 201, description = "Module uploaded", body = MessageResponse),
        (status = 400, description = "Malformed signature headers"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Quota exceeded or ownership denied"),
        (status = 404, description = "Registry not found or not in local/hybrid mode"),
        (status = 409, description = "Version already published"),
        (status = 422, description = "Versioning policy violation"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[post("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}")]
pub async fn terraform_module_upload(
    req: HttpRequest,
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    payload: web::Payload,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, provider, version) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;
    require_local_mode(&registry, &mode_map)?;
    require_segments(&[
        ("namespace", &namespace),
        ("name", &name),
        ("provider", &provider),
        ("version", &version),
    ])?;

    let bytes = collect_payload(payload).await?;
    let checksum = hex::encode(Sha256::digest(&bytes));
    let index_metadata = serde_json::json!({
        "namespace": namespace,
        "name": name,
        "provider": provider,
        "version": version,
        "sha256": checksum,
        "yanked": false,
    });

    let pkg_name = format!("modules/{namespace}/{name}/{provider}");
    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);

    super::super::super::common::publish_and_respond(
        &local_svc,
        &notification_svc,
        PublishRequest {
            unlisted: false,
            registry,
            name: pkg_name,
            version,
            artifact: bytes,
            checksum,
            index_metadata,
            publisher: identity.0,
            signature_bytes,
            signature_type,
        },
        actix_web::http::StatusCode::CREATED,
        MessageResponse::new("module uploaded"),
    )
    .await
}

/// Yank a Terraform module version (local/hybrid registries only).
///
/// Yanked versions remain in storage but are hidden from version listings.
#[utoipa::path(
    delete,
    path = "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Module namespace"),
        ("name"      = String, Path, description = "Module name"),
        ("provider"  = String, Path, description = "Module provider"),
        ("version"   = String, Path, description = "Module version to yank"),
    ),
    responses(
        (status = 200, description = "Version yanked", body = MessageResponse),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}")]
pub async fn terraform_module_yank(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, provider, version) = path.into_inner();
    let pkg_name = format!("modules/{namespace}/{name}/{provider}");
    let display_name = format!("module {namespace}/{name}/{provider}");
    terraform_set_yanked(
        TerraformYankRequest {
            registry: &registry,
            map: &map,
            mode_map: &mode_map,
            pkg_name: &pkg_name,
            version: &version,
            display_name: &display_name,
            yanked: true,
        },
        &identity,
        &local_svc,
        &notification_svc,
    )
    .await
}

/// Unyank a Terraform module version (local/hybrid registries only).
#[utoipa::path(
    post,
    path = "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}/unyank",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Module namespace"),
        ("name"      = String, Path, description = "Module name"),
        ("provider"  = String, Path, description = "Module provider"),
        ("version"   = String, Path, description = "Module version to unyank"),
    ),
    responses(
        (status = 200, description = "Version unyanked", body = MessageResponse),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions/{version}/unyank")]
pub async fn terraform_module_unyank(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, provider, version) = path.into_inner();
    let pkg_name = format!("modules/{namespace}/{name}/{provider}");
    let display_name = format!("module {namespace}/{name}/{provider}");
    terraform_set_yanked(
        TerraformYankRequest {
            registry: &registry,
            map: &map,
            mode_map: &mode_map,
            pkg_name: &pkg_name,
            version: &version,
            display_name: &display_name,
            yanked: false,
        },
        &identity,
        &local_svc,
        &notification_svc,
    )
    .await
}
