use super::{
    artifact_storage_key, build_metadata_xml, content_type_for, maven_artifact_storage_key,
    AppError, AuthIdentity, HttpResponse, LocalRegistryService, MavenPathKind, RegistryMode,
};
use batlehub_core::entities::Action;

/// Try to serve a Maven request from local/hybrid storage.
/// Returns `Ok(Some(response))` on a local hit, `Ok(None)` to fall through to proxy.
pub async fn maven_local_response(
    local_svc: &LocalRegistryService,
    registry: &str,
    kind: &MavenPathKind,
    identity: &AuthIdentity,
    mode: RegistryMode,
) -> Result<Option<HttpResponse>, AppError> {
    match kind {
        MavenPathKind::Metadata { name } => {
            handle_maven_metadata(local_svc, registry, name, identity, mode).await
        }
        MavenPathKind::Artifact {
            name,
            version,
            filename,
        } => {
            handle_maven_artifact(local_svc, registry, name, version, filename, identity, mode)
                .await
        }
    }
}

pub async fn handle_maven_metadata(
    local_svc: &LocalRegistryService,
    registry: &str,
    name: &str,
    identity: &AuthIdentity,
    mode: RegistryMode,
) -> Result<Option<HttpResponse>, AppError> {
    match local_svc.get_maven_versions(registry, name, identity).await {
        Ok(versions) => {
            let group_id = versions
                .first()
                .and_then(|v| v.index_metadata.get("group_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            let artifact_id = versions
                .first()
                .and_then(|v| v.index_metadata.get("artifact_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            let xml = build_metadata_xml(&group_id, &artifact_id, &versions)?;
            Ok(Some(
                HttpResponse::Ok().content_type("application/xml").body(xml),
            ))
        }
        Err(batlehub_core::error::CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {
            Ok(None)
        }
        Err(batlehub_core::error::CoreError::NotFound(msg)) => Err(AppError::not_found(msg)),
        Err(e) => Err(AppError::from(e)),
    }
}

pub async fn handle_maven_artifact(
    local_svc: &LocalRegistryService,
    registry: &str,
    name: &str,
    version: &str,
    filename: &str,
    identity: &AuthIdentity,
    mode: RegistryMode,
) -> Result<Option<HttpResponse>, AppError> {
    // A Maven version is several files, so the key is built here rather than by
    // `get_artifact` — but the read goes through `get_artifact_at_key`, which
    // applies the same gate (rule chain, visibility, pre-release). Reading
    // `local_svc.storage` directly, as this did, skipped **both** the chain and
    // `check_visibility`: `maven-metadata.xml` refused a team-visibility
    // coordinate while the jar beside it was served to anyone (survey finding 6).
    let storage_key = if filename.ends_with(".pom") {
        artifact_storage_key(registry, name, version)
    } else {
        maven_artifact_storage_key(registry, name, version, filename)
    };
    // `filename` is what makes one `mvn` resolution count as one download
    // rather than four: the `.sha1`/`.md5`/`.asc` beside the jar are recorded as
    // metadata, not downloads. The proxy fall-through below passes the same
    // value through `with_artifact`, so a Hybrid registry's counts do not depend
    // on whether the artifact happened to be local.
    let pkg =
        batlehub_core::entities::PackageId::new(registry, name, version).with_artifact(filename);
    match local_svc
        .get_artifact_at_key(
            &pkg,
            &storage_key,
            Action::ReleasesRead,
            identity,
            &identity.1,
        )
        .await
    {
        Ok(Some(buf)) => Ok(Some(
            HttpResponse::Ok()
                .content_type(content_type_for(filename))
                .body(buf),
        )),
        Ok(None) if mode == RegistryMode::Hybrid => Ok(None),
        Ok(None) => Err(AppError::not_found(format!(
            "{name}@{version}/{filename} not found in local registry"
        ))),
        // A storage fault falls through upstream in hybrid, as before. An
        // authorization refusal must not: `AccessDenied` is an answer, not a
        // miss, and falling through would ask the upstream for a package this
        // caller has just been refused.
        Err(batlehub_core::error::CoreError::Storage(e)) if mode == RegistryMode::Hybrid => {
            tracing::warn!("local storage error, falling back to proxy: {e}");
            Ok(None)
        }
        Err(e) => Err(AppError::from(e)),
    }
}
