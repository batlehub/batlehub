use super::{
    collect_payload, delete, extract_signature_headers, off_origin_checksum_urls, post, put,
    registry_public_base, require_local_mode, require_registry_type, require_segments,
    terraform_provider_binary_storage_key, terraform_set_yanked, web, AppError, Arc,
    ArtifactSignature, AuthIdentity, Digest, HttpRequest, HttpResponse, LocalRegistryService,
    NotificationService, PublishPolicyRequest, PublishRequest, RegistryMap, RegistryModeMap,
    Responder, Sha256, StorageMeta, TerraformYankRequest,
};
use crate::handlers::schemas::{ArtifactBytes, MessageResponse, OkResponse};
use batlehub_core::error::CoreError;
use batlehub_core::services::openpgp::PgpVerdict;
use batlehub_core::services::{terraform_provider_shasums_storage_key, TerraformShasumsFile};

/// Upload a Terraform provider version manifest (JSON describing version + platforms).
///
/// Only available when the registry is in `local` or `hybrid` mode.
/// Optionally accepts `X-Artifact-Signature` (base64) and `X-Signature-Type` headers.
/// Platform binaries are uploaded separately via `PUT /artifact/{os}/{arch}`.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
    ),
    responses(
        (status = 201, description = "Provider version manifest stored", body = MessageResponse),
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
#[post("/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions")]
pub async fn terraform_provider_upload(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    payload: web::Payload,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, ptype) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;
    require_local_mode(&registry, &mode_map)?;

    let bytes = collect_payload(payload).await?;

    let manifest: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))?;

    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::bad_request("manifest missing 'version' field"))?
        .to_owned();
    require_segments(&[
        ("namespace", &namespace),
        ("provider type", &ptype),
        ("version", &version),
    ])?;

    let checksum = hex::encode(Sha256::digest(&bytes));
    let mut index_metadata = manifest.clone();
    if let Some(obj) = index_metadata.as_object_mut() {
        obj.insert("sha256".to_owned(), serde_json::json!(checksum));
        obj.entry("yanked").or_insert(serde_json::json!(false));
    }

    let name = format!("providers/{namespace}/{ptype}");
    let (signature_bytes, signature_type) =
        ArtifactSignature::split(extract_signature_headers(&req)?);

    // A manifest may name checksum URLs on another host instead of uploading
    // `SHA256SUMS` and its signature here (`…/shasums`, issue #281); Terraform
    // refuses a provider whose checksum list it cannot get (measured, RFC 0012
    // §7.1). So this is told to the publisher rather than refused: that host will see every
    // `terraform init` for this provider, and an air-gapped install will reach
    // it. The URLs are never signed, whatever else happens to them.
    let off_origin = off_origin_checksum_urls(&manifest, &registry_public_base(&req, &registry));
    let message = if off_origin.is_empty() {
        MessageResponse::new("provider version published")
    } else {
        MessageResponse::new(format!(
            "provider version published — note: {} points at another host, so `terraform init` \
             fetches it from there rather than from this registry. It is never given a signed \
             URL, and an air-gapped install will reach that host.",
            off_origin.join(", ")
        ))
    };

    // Kept for the log line below, which must not fire until the publish has
    // actually landed: a 409, a 422 or a quota refusal would otherwise leave a
    // WARN naming a version this registry does not hold, and an operator
    // grepping for off-origin checksums would audit manifests that do not exist.
    let logged = (!off_origin.is_empty())
        .then(|| (registry.clone(), name.clone(), version.clone(), off_origin));

    let resp = super::super::super::common::publish_and_respond(
        &local_svc,
        &notification_svc,
        PublishRequest {
            unlisted: false,
            registry,
            name,
            version,
            artifact: bytes,
            checksum,
            index_metadata,
            publisher: identity.0,
            signature_bytes,
            signature_type,
        },
        actix_web::http::StatusCode::CREATED,
        message,
    )
    .await?;

    if let Some((registry, name, version, urls)) = logged {
        tracing::warn!(
            registry = %registry,
            package = %name,
            version = %version,
            urls = ?urls,
            "published manifest names checksum URLs on another host"
        );
    }
    Ok(resp)
}

/// Upload a platform binary for a locally-published Terraform provider.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
        ("version"   = String, Path, description = "Provider version"),
        ("os"        = String, Path, description = "Target OS"),
        ("arch"      = String, Path, description = "Target architecture"),
    ),
    responses(
        (status = 200, description = "Binary stored", body = OkResponse),
        (status = 400, description = "Malformed signature headers"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Registry not found or not in local/hybrid mode"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/artifact/{os}/{arch}")]
pub async fn terraform_provider_binary_upload(
    path: web::Path<(String, String, String, String, String, String)>,
    identity: AuthIdentity,
    payload: web::Payload,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, ptype, version, os, arch) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;
    require_local_mode(&registry, &mode_map)?;

    // Edge chokepoint: this handler builds a storage key directly from the path
    // components, so reject any traversal attempt with a clean 400 first.
    require_segments(&[
        ("namespace", &namespace),
        ("provider type", &ptype),
        ("version", &version),
    ])?;
    batlehub_core::services::validate_terraform_platform(&os, &arch).map_err(AppError::from)?;

    let bytes = collect_payload(payload).await?;

    // Stored directly rather than through the three-phase `publish()` (the
    // manifest upload already created the version row), so it must still go
    // through the same policy gate (role, namespace, ownership, quota, size
    // limit) that `terraform_provider_upload` gets via `publish_and_respond`.
    let name = format!("providers/{namespace}/{ptype}");
    let artifact_len = bytes.len() as u64;
    local_svc
        .enforce_publish_policy(
            &PublishPolicyRequest {
                registry: &registry,
                name: &name,
                version: &version,
                artifact_len,
                signature_bytes: None,
                signature_type: None,
                artifact_key: None,
            },
            &identity.0,
        )
        .await
        .map_err(AppError::from)?;

    let key =
        terraform_provider_binary_storage_key(&registry, &namespace, &ptype, &version, &os, &arch);
    if let Err(e) = local_svc
        .storage
        .store(
            &key,
            bytes,
            StorageMeta {
                content_type: Some("application/zip".to_owned()),
                size: None,
                checksum: None,
            },
        )
        .await
    {
        local_svc
            .revoke_publish_quota(&identity.0, &registry, artifact_len)
            .await;
        return Err(AppError::from(e));
    }

    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// The largest `SHA256SUMS` accepted: one line per platform archive, so a few
/// kilobytes in practice.
const MAX_SHASUMS_BYTES: usize = 1024 * 1024;
/// The largest detached signature accepted. A GPG signature is under 1 KiB.
const MAX_SHASUMS_SIG_BYTES: usize = 64 * 1024;

/// Upload a locally published provider version's `SHA256SUMS` (issue #281).
///
/// The publisher's checksum list, as goreleaser writes it
/// (`terraform-provider-{type}_{version}_SHA256SUMS`). Refused with `400` when
/// it does not list every platform of the version's manifest with the
/// manifest's hash. Its signature goes to `…/shasums.sig`, after it: a new
/// list drops the signature over the previous one.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
        ("version"   = String, Path, description = "Provider version"),
    ),
    request_body(content = String, content_type = "text/plain"),
    responses(
        (status = 200, description = "Checksum list stored; a signature over the previous one is dropped", body = MessageResponse),
        (status = 400, description = "Malformed, or disagrees with the manifest"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Ownership denied"),
        (status = 404, description = "Registry or version not found, or not in local/hybrid mode"),
        (status = 413, description = "Larger than 1 MiB"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums")]
pub async fn terraform_provider_shasums_upload(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    payload: web::Payload,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    upload_shasums_file(
        TerraformShasumsFile::Sums,
        path.into_inner(),
        &identity,
        payload,
        &local_svc,
        &map,
        &mode_map,
    )
    .await
}

/// Upload the detached GPG signature over a provider version's `SHA256SUMS`
/// (issue #281). Checked against the namespace's signing keys
/// (`namespace signing-key set`) and the stored list before it is stored; one
/// this instance cannot check is stored unverified, and Terraform still checks it.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums.sig",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
        ("version"   = String, Path, description = "Provider version"),
    ),
    request_body(content = ArtifactBytes, content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Signature stored, verified against the namespace's keys when they and it can be read", body = MessageResponse),
        (status = 400, description = "Empty, malformed, signed by a key the namespace does not register, or does not verify"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Ownership denied"),
        (status = 404, description = "Registry or version not found, or not in local/hybrid mode"),
        (status = 409, description = "No SHA256SUMS uploaded for this version yet"),
        (status = 413, description = "Larger than 64 KiB"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/proxy/{registry}/v1/providers/{namespace}/{ptype}/{version}/shasums.sig")]
pub async fn terraform_provider_shasums_sig_upload(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    payload: web::Payload,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    upload_shasums_file(
        TerraformShasumsFile::Signature,
        path.into_inner(),
        &identity,
        payload,
        &local_svc,
        &map,
        &mode_map,
    )
    .await
}

async fn upload_shasums_file(
    file: TerraformShasumsFile,
    (registry, namespace, ptype, version): (String, String, String, String),
    identity: &AuthIdentity,
    payload: web::Payload,
    local_svc: &LocalRegistryService,
    map: &RegistryMap,
    mode_map: &RegistryModeMap,
) -> Result<HttpResponse, AppError> {
    require_registry_type(&registry, "terraform", map)?;
    require_local_mode(&registry, mode_map)?;
    // The storage key is built from the path, so the edge refuses traversal.
    require_segments(&[
        ("namespace", &namespace),
        ("provider type", &ptype),
        ("version", &version),
    ])?;

    let bytes = collect_payload(payload).await?;
    let cap = match file {
        TerraformShasumsFile::Sums => MAX_SHASUMS_BYTES,
        TerraformShasumsFile::Signature => MAX_SHASUMS_SIG_BYTES,
    };
    if bytes.len() > cap {
        return Err(AppError::from(CoreError::PayloadTooLarge(format!(
            "{} exceeds the {cap}-byte limit",
            file.artifact()
        ))));
    }
    if bytes.is_empty() {
        return Err(AppError::bad_request(format!(
            "{} is empty",
            file.artifact()
        )));
    }

    // The same gate as the archives (role, namespace, ownership, quota): these
    // two files decide what `terraform init` accepts.
    let name = format!("providers/{namespace}/{ptype}");
    let artifact_len = bytes.len() as u64;
    local_svc
        .enforce_publish_policy(
            &PublishPolicyRequest {
                registry: &registry,
                name: &name,
                version: &version,
                artifact_len,
                signature_bytes: None,
                signature_type: None,
                artifact_key: None,
            },
            &identity.0,
        )
        .await
        .map_err(AppError::from)?;
    let checked = match file {
        TerraformShasumsFile::Sums => local_svc
            .check_terraform_provider_shasums(&registry, &name, &version, &bytes)
            .await
            .map(|()| "SHA256SUMS stored".to_owned()),
        TerraformShasumsFile::Signature => local_svc
            .check_terraform_provider_signature(&registry, &namespace, &ptype, &version, &bytes)
            .await
            .map(|verdict| match verdict {
                PgpVerdict::Verified { key_id } => format!("signature verified with key {key_id}"),
                PgpVerdict::Unsupported(why) => {
                    format!("signature stored unverified ({why}); terraform init still checks it")
                }
            }),
    };
    let message = match checked {
        Ok(message) => message,
        Err(e) => {
            local_svc
                .revoke_publish_quota(&identity.0, &registry, artifact_len)
                .await;
            return Err(AppError::from(e));
        }
    };

    let key = terraform_provider_shasums_storage_key(&registry, &namespace, &ptype, &version, file);
    let content_type = match file {
        TerraformShasumsFile::Sums => "text/plain",
        TerraformShasumsFile::Signature => "application/octet-stream",
    };
    if let Err(e) = local_svc
        .storage
        .store(
            &key,
            bytes,
            StorageMeta {
                content_type: Some(content_type.to_owned()),
                size: None,
                checksum: None,
            },
        )
        .await
    {
        local_svc
            .revoke_publish_quota(&identity.0, &registry, artifact_len)
            .await;
        return Err(AppError::from(e));
    }

    if file == TerraformShasumsFile::Sums {
        // A new list voids the signature over the old one: Terraform would
        // refuse the pair, and the next `.sig` upload is checked against this.
        let sig = terraform_provider_shasums_storage_key(
            &registry,
            &namespace,
            &ptype,
            &version,
            TerraformShasumsFile::Signature,
        );
        local_svc
            .storage
            .delete(&sig)
            .await
            .map_err(AppError::from)?;
    }

    Ok(HttpResponse::Ok().json(MessageResponse::new(message)))
}

/// Yank a Terraform provider version (local/hybrid registries only).
///
/// Yanked versions remain in storage but are hidden from version listings.
#[utoipa::path(
    delete,
    path = "/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
        ("version"   = String, Path, description = "Provider version to yank"),
    ),
    responses(
        (status = 200, description = "Version yanked", body = MessageResponse),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}")]
pub async fn terraform_provider_yank(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, ptype, version) = path.into_inner();
    let pkg_name = format!("providers/{namespace}/{ptype}");
    let display_name = format!("provider {namespace}/{ptype}");
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

/// Unyank a Terraform provider version (local/hybrid registries only).
#[utoipa::path(
    post,
    path = "/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}/unyank",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Provider namespace"),
        ("ptype"     = String, Path, description = "Provider type"),
        ("version"   = String, Path, description = "Provider version to unyank"),
    ),
    responses(
        (status = 200, description = "Version unyanked", body = MessageResponse),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Version not found"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/proxy/{registry}/v1/providers/{namespace}/{ptype}/versions/{version}/unyank")]
pub async fn terraform_provider_unyank(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, ptype, version) = path.into_inner();
    let pkg_name = format!("providers/{namespace}/{ptype}");
    let display_name = format!("provider {namespace}/{ptype}");
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
