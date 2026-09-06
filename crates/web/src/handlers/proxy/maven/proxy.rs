use super::{
    collect_payload, content_type_for, get, maven_artifact_storage_key, maven_local_response,
    parse_maven_path, parse_pom, proxy_document, proxy_stream, put, require_local_mode,
    require_registry_type, web, AppError, Arc, AuthIdentity, Digest, HttpResponse,
    LocalRegistryService, MavenPathKind, NotificationService, PackageId, ProxyService,
    PublishPolicyRequest, PublishRequest, RegistryMap, RegistryMode, RegistryModeMap, Responder,
    Sha256, StorageMeta,
};
use crate::handlers::proxy::common::fetch_proxy_document;
use crate::handlers::schemas::{ArtifactBytes, OkResponse};
use batlehub_core::entities::Action;

/// Proxy or serve a Maven repository request.
///
/// In `Local`/`Hybrid` mode:
/// - `maven-metadata.xml` is generated dynamically from published versions in the DB.
/// - Artifact files are served from local storage; Hybrid falls back to upstream if not found.
///
/// In `Proxy` mode (default): forwards to the configured upstream.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/maven2/{path}",
    tag = "proxy/maven",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path"     = String, Path, description = "Maven repository path"),
    ),
    responses(
        (status = 200, description = "Maven artifact or metadata", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Invalid Maven path"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Artifact not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/maven2/{path:.*}")]
pub async fn maven_get(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, maven_path) = path.into_inner();
    require_registry_type(&registry, "maven", &map)?;

    let mode = mode_map.get(&registry);
    let kind = parse_maven_path(&registry, &maven_path)?;

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        authorize_local_read(&svc, &registry, &kind, &identity).await?;
        if let Some(resp) =
            maven_local_response(&local_svc, &registry, &kind, &identity, mode).await?
        {
            return Ok(resp);
        }
    }

    // Proxy fallback (Proxy mode or Hybrid miss).
    //
    // `maven-metadata.xml` is a *listing*, and every other path here is an
    // artifact. The split matters: a listing goes through `proxy_document`,
    // which parses the XML and removes administratively blocked versions before
    // Maven resolves a range, `LATEST` or `RELEASE` against it. Streamed
    // through `proxy_stream` it would arrive with the blocked version still in
    // `<versions>`, and the build would pick it and fail at download.
    // A checksum of `maven-metadata.xml`: when the document is composed from
    // the held set there is no upstream `.sha1` to fetch, and Maven retries
    // the `503` for minutes (RFC 0008-bis §13.4). The composed bytes answer
    // their own digest; a held document keeps upstream's file, below.
    if let Some(resp) = composed_metadata_checksum(&svc, &registry, &maven_path, &identity).await {
        return Ok(resp);
    }

    match &kind {
        MavenPathKind::Metadata { name } => {
            proxy_document(
                svc,
                PackageId::new(&registry, name.clone(), "maven-metadata.xml"),
                identity,
                Action::ReleasesRead,
                batlehub_core::ports::DocumentKind::Versions,
                String::new(),
            )
            .await
        }
        MavenPathKind::Artifact {
            name,
            version,
            filename,
        } => {
            proxy_stream(
                svc,
                PackageId::new(&registry, name.clone(), version.as_str())
                    .with_artifact(filename.as_str()),
                identity,
                Action::ReleasesRead,
                Some(content_type_for(filename)),
            )
            .await
        }
    }
}

/// Enforce registry RBAC before any local read.
///
/// `maven_local_response` reads generated metadata and artifact bytes straight
/// from local storage without running the registry rule chain (only the proxy
/// fall-through does), so a local hit would otherwise bypass
/// `[registries.rbac]`.
async fn authorize_local_read(
    svc: &ProxyService,
    registry: &str,
    kind: &MavenPathKind,
    identity: &AuthIdentity,
) -> Result<(), AppError> {
    let (name, version) = match kind {
        MavenPathKind::Metadata { name } => (name.clone(), "maven-metadata.xml".to_owned()),
        MavenPathKind::Artifact { name, version, .. } => (name.clone(), version.clone()),
    };
    svc.authorize_read(
        &PackageId::new(registry, name, version),
        &identity.0,
        Action::ReleasesRead,
    )
    .await
    .map_err(AppError::from)
}

/// A checksum of `maven-metadata.xml` for a document this instance *composed*.
///
/// When the document is composed from the held set there is no upstream
/// `.sha1` to fetch, and Maven retries the `503` for minutes (RFC 0008-bis
/// §13.4). The composed bytes answer their own digest; a held document keeps
/// upstream's file, so this returns `None` and the request falls through.
async fn composed_metadata_checksum(
    svc: &web::Data<Arc<ProxyService>>,
    registry: &str,
    maven_path: &str,
    identity: &AuthIdentity,
) -> Option<HttpResponse> {
    let (name, algo) = super::routing::metadata_checksum_of(maven_path)?;
    let doc = fetch_proxy_document(
        svc.clone(),
        PackageId::new(registry, name, "maven-metadata.xml"),
        AuthIdentity(identity.0.clone()),
        Action::ReleasesRead,
        batlehub_core::ports::DocumentKind::Versions,
        String::new(),
    )
    .await
    .ok()?;
    doc.synthesised.as_ref()?;
    let bytes = match &doc.body {
        batlehub_core::ports::DocumentBody::Text(t) => t.as_bytes().to_vec(),
        batlehub_core::ports::DocumentBody::Json(v) => serde_json::to_vec(v).unwrap_or_default(),
    };
    let digest = digest_hex(algo, &bytes);
    let mut builder = HttpResponse::Ok();
    builder.content_type("text/plain; charset=utf-8");
    crate::handlers::proxy::common::listing_headers(&mut builder, &doc);
    Some(builder.body(digest))
}

/// Upload a Maven artifact to the local registry.
///
/// Accepts any Maven 2 repository path:
/// - `.pom` files trigger the three-phase publish, storing version metadata.
/// - All other files (`.jar`, checksums, etc.) are stored directly and accessible via GET.
/// - Client-uploaded `maven-metadata.xml` is accepted but ignored (generated dynamically).
///
/// Only available when the registry is configured in `local` or `hybrid` mode.
#[utoipa::path(
    put,
    path = "/proxy/{registry}/maven2/{path}",
    tag = "proxy/maven",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("path"     = String, Path, description = "Maven repository path"),
    ),
    responses(
        (status = 200, description = "Accepted (maven-metadata.xml silently ignored)", body = OkResponse),
        (status = 201, description = "Artifact stored", body = OkResponse),
        (status = 400, description = "Invalid Maven path or malformed POM"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "Registry not found or not in local/hybrid mode"),
        (status = 409, description = "Version already published"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[put("/proxy/{registry}/maven2/{path:.*}")]
pub async fn maven_put(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    payload: web::Payload,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    notification_svc: web::Data<Option<Arc<NotificationService>>>,
) -> Result<impl Responder, AppError> {
    let (registry, maven_path) = path.into_inner();
    require_registry_type(&registry, "maven", &map)?;
    require_local_mode(&registry, &mode_map)?;

    let kind = parse_maven_path(&registry, &maven_path)?;

    match kind {
        MavenPathKind::Metadata { .. } => {
            // Silently accept and ignore client-uploaded metadata.xml — generated dynamically.
            Ok(HttpResponse::Ok().json(OkResponse::new()))
        }
        MavenPathKind::Artifact {
            name,
            version,
            filename,
        } => {
            let bytes = collect_payload(payload).await?;

            if filename == "maven-metadata.xml" {
                return Ok(HttpResponse::Ok().json(OkResponse::new()));
            }

            if !filename.ends_with(".pom") {
                // Non-POM artifact (jar, sources, checksums, etc.): stored directly
                // rather than through the three-phase `publish()`, so it must go
                // through the same policy gate (role, versioning, signing, quota,
                // size limit) that the .pom branch gets via `publish_and_respond`.
                let artifact_len = bytes.len() as u64;
                let storage_key = maven_artifact_storage_key(&registry, &name, &version, &filename);
                local_svc
                    .enforce_publish_policy(
                        &PublishPolicyRequest {
                            registry: &registry,
                            name: &name,
                            version: &version,
                            artifact_len,
                            signature_bytes: None,
                            signature_type: None,
                            // RFC 0015 §4.5: a Maven coordinate is several files
                            // PUT one at a time, so `immutable` has to ask
                            // whether *this file* is being replaced. The version
                            // row exists from the `.pom` onward, and deciding on
                            // it would make an `immutable = "always"` namespace
                            // refuse the jar of the publish that just created
                            // it — permanence turned into impossibility.
                            artifact_key: Some(&storage_key),
                        },
                        &identity.0,
                    )
                    .await
                    .map_err(AppError::from)?;

                if let Err(e) = local_svc
                    .storage
                    .store(
                        &storage_key,
                        bytes,
                        StorageMeta {
                            content_type: Some(content_type_for(&filename).to_owned()),
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
                return Ok(HttpResponse::Created().json(OkResponse::new()));
            }

            // .pom file: parse XML + run three-phase publish. The URL path is
            // the canonical Maven coordinate (it's what a later GET uses), so
            // a POM that declares a different version is rejected rather than
            // silently publishing under whatever the body claims.
            let pom = parse_pom(&bytes)?;
            if !pom.version.is_empty() && pom.version != version {
                return Err(AppError::bad_request(format!(
                    "POM declares version '{}' but URL path specifies '{}'",
                    pom.version, version
                )));
            }
            let resolved_version = version.clone();

            let checksum = hex::encode(Sha256::digest(&bytes));
            let index_metadata = serde_json::json!({
                "group_id": pom.group_id,
                "artifact_id": pom.artifact_id,
                "version": resolved_version,
                "packaging": pom.packaging,
                "description": pom.description,
                "sha256": checksum,
                "yanked": false,
            });

            super::super::common::publish_and_respond(
                &local_svc,
                &notification_svc,
                PublishRequest {
                    unlisted: false,
                    registry: registry.clone(),
                    name: name.clone(),
                    version: resolved_version.clone(),
                    artifact: bytes,
                    checksum,
                    index_metadata,
                    publisher: identity.0,
                    signature_bytes: None,
                    signature_type: None,
                },
                actix_web::http::StatusCode::CREATED,
                serde_json::json!({
                    "message": format!("Successfully published {name} {resolved_version}")
                }),
            )
            .await
        }
    }
}

/// The hex digest Maven expects in a checksum file, for the algorithm its
/// suffix names.
fn digest_hex(algo: &str, bytes: &[u8]) -> String {
    use md5::Digest as _;
    match algo {
        "md5" => hex::encode(md5::Md5::digest(bytes)),
        "sha256" => hex::encode(sha2::Sha256::digest(bytes)),
        "sha512" => hex::encode(sha2::Sha512::digest(bytes)),
        _ => hex::encode(sha1::Sha1::digest(bytes)),
    }
}
