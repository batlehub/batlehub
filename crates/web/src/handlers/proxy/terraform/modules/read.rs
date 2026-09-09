use super::{
    append_signature_headers, get, proxy_stream, registry_public_base, require_registry_type,
    terraform_versions_response, web, AppError, Arc, AuthIdentity, HttpRequest, HttpResponse,
    LocalRegistryService, ProxyService, RegistryMap, RegistryMode, RegistryModeMap, Responder,
};
use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use batlehub_core::entities::Action;

/// List available versions for a Terraform module.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Module namespace"),
        ("name"      = String, Path, description = "Module name"),
        ("provider"  = String, Path, description = "Module provider"),
    ),
    responses(
        (status = 200, description = "Module versions JSON", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Module not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/versions")]
pub async fn terraform_module_versions(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, provider) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;

    let pkg_name = format!("modules/{namespace}/{name}/{provider}");
    let mode = mode_map.get(&registry);

    let local_result = if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        Some(
            local_svc
                .get_terraform_module_versions_response(&registry, &pkg_name, &identity)
                .await,
        )
    } else {
        None
    };

    terraform_versions_response(&registry, pkg_name, identity, svc, mode, local_result).await
}

/// Get the download URL for a specific Terraform module version.
///
/// In local/hybrid mode: returns `204 No Content` with `X-Terraform-Get` pointing at the
/// local artifact endpoint. In proxy mode: forwards to upstream.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/download",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Module namespace"),
        ("name"      = String, Path, description = "Module name"),
        ("provider"  = String, Path, description = "Module provider"),
        ("version"   = String, Path, description = "Module version"),
    ),
    responses(
        (status = 204, description = "X-Terraform-Get header contains the archive download URL"),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Module not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/download")]
pub async fn terraform_module_download(
    path: web::Path<(String, String, String, String, String)>,
    req: HttpRequest,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, provider, version) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;

    // One answer in every mode: our own artifact route.
    //
    // Proxy mode used to forward the upstream's `X-Terraform-Get` verbatim,
    // which handed the client a URL to fetch **directly** — no bytes passed
    // through this proxy, so the rule chain never ran on them and a blocked
    // module version stayed downloadable by anyone who could read the listing
    // (RFC 0006 §13.6, closed by RFC 0009 §7.2).
    //
    // The artifact route goes through `ProxyService::handle` — gate, cache,
    // audit — and its adapter resolves the upstream pointer server-side, so the
    // client never learns where upstream keeps the tarball and never reaches it
    // unmediated.
    let base_url = registry_public_base(&req, &registry);
    let artifact_url =
        format!("{base_url}/v1/modules/{namespace}/{name}/{provider}/{version}/artifact");
    Ok(HttpResponse::NoContent()
        .insert_header(("X-Terraform-Get", artifact_url))
        .finish())
}

/// `GET /v1/modules/{ns}/{name}/{provider}/{version}` — one module version's
/// metadata.
///
/// Part of the registry protocol and missing until RFC 0009 §7.2. Terraform
/// reads it to learn a module's inputs, outputs and submodules before planning.
///
/// A blocked version is a `404` rather than a filtered document: this names
/// exactly one version, so there is nothing to remove from it and no list to
/// repair it against — the same treatment the mirror's `{version}.json` gets.
#[utoipa::path(
    get,
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
        (status = 200, description = "Module version metadata", body = UpstreamDocument),
        (status = 403, description = "Access denied"),
        (status = 404, description = "Unknown module, or this version is blocked"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}")]
pub async fn terraform_module_metadata(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, provider, version) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;

    let pkg_name = format!("modules/{namespace}/{name}/{provider}");

    // Answered from the *filtered* version list rather than from a separate
    // upstream call, so a blocked version cannot be visible here while hidden
    // in `/versions` — one source, one answer.
    let req = batlehub_core::services::ProxyRequest {
        package_id: batlehub_core::entities::PackageId::new(&registry, &pkg_name, "versions"),
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let doc = svc
        .version_document(&req, batlehub_core::ports::DocumentKind::Versions, "")
        .await
        .map_err(AppError::from)?;

    let listed = doc
        .body
        .as_json()
        .and_then(|j| j.get("modules"))
        .and_then(|m| m.as_array())
        .map(|mods| {
            mods.iter()
                .filter_map(|m| m.get("versions").and_then(|v| v.as_array()))
                .flatten()
                .filter_map(|v| v.get("version").and_then(|v| v.as_str()))
                .any(|v| v == version)
        })
        .unwrap_or(false);

    if !listed {
        return Err(AppError::not_found(format!(
            "module {namespace}/{name}/{provider} {version} is not available"
        )));
    }

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .json(serde_json::json!({
            "id": format!("{namespace}/{name}/{provider}/{version}"),
            "owner": namespace,
            "namespace": namespace,
            "name": name,
            "version": version,
            "provider": provider,
            "source": "",
            "root": { "path": "", "inputs": [], "outputs": [], "dependencies": [] },
            "submodules": [],
        })))
}

/// Download the tarball for a locally-published Terraform module.
///
/// This is the target of the `X-Terraform-Get` redirect issued by `terraform_module_download`
/// in local/hybrid mode. Returns `X-Artifact-Signature` and `X-Signature-Type` headers
/// if the version was uploaded with a signature.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/artifact",
    tag = "proxy/terraform",
    params(
        ("registry"  = String, Path, description = "Registry name"),
        ("namespace" = String, Path, description = "Module namespace"),
        ("name"      = String, Path, description = "Module name"),
        ("provider"  = String, Path, description = "Module provider"),
        ("version"   = String, Path, description = "Module version"),
    ),
    responses(
        (status = 200, description = "Module tarball", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 404, description = "Module not found"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/v1/modules/{namespace}/{name}/{provider}/{version}/artifact")]
pub async fn terraform_module_artifact(
    path: web::Path<(String, String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, namespace, name, provider, version) = path.into_inner();
    require_registry_type(&registry, "terraform", &map)?;

    let pkg_name = format!("modules/{namespace}/{name}/{provider}");
    let mode = mode_map.get(&registry);

    // Proxy mode reaches here because `terraform_module_download` now points
    // `X-Terraform-Get` at this route rather than at upstream. `proxy_stream`
    // runs the rule chain and caches, and the adapter follows the upstream
    // `X-Terraform-Get` on the server side.
    if mode == RegistryMode::Proxy {
        let pkg = batlehub_core::entities::PackageId::new(&registry, &pkg_name, &version)
            .with_artifact("download");
        return proxy_stream(
            svc,
            pkg,
            identity,
            Action::ReleasesRead,
            Some("application/octet-stream"),
        )
        .await;
    }
    // Terraform is local-only (no proxy fall-through), so the registry rule
    // chain would otherwise never run for these reads. `get_artifact` runs it
    // against the resource type named here.
    local_svc
        .check_prerelease_access(&registry, &version, &identity)
        .await
        .map_err(AppError::from)?;

    let bytes = local_svc
        .get_artifact(
            &registry,
            &pkg_name,
            &version,
            Action::ReleasesRead,
            &identity,
            &identity.1,
        )
        .await
        .map_err(AppError::from)?;

    let mut resp = HttpResponse::Ok();
    append_signature_headers(&mut resp, &local_svc, &registry, &pkg_name, &version).await;
    Ok(resp.content_type("application/gzip").body(bytes))
}
