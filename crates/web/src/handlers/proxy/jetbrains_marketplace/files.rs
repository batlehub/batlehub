//! `/files/*` blobs, plugin downloads, and the `pluginManager` endpoint.

use std::sync::Arc;

use actix_web::{get, http::header, web, HttpRequest, HttpResponse, Responder};
use serde::Deserialize;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    services::{
        validate_package_name, validate_path_safe, LocalRegistryService, ProxyRequest, ProxyService,
    },
};

use super::cached_forward::{cached_forward_get, forward_cache_key};
use super::render::{plugin_meta_json, update_meta_json, ExtraMeta, RenderEntry};
use super::{content_type_for, require_jbm, PLUGIN_ARTIFACT, STABLE_CHANNEL};
use crate::handlers::proxy::common::{
    proxy_stream, serve_local_or_proxy_artifact, LocalOrProxyArtifactOpts,
};
use crate::handlers::schemas::{ArtifactBytes, UpstreamDocument};
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap, UpstreamMap};
use batlehub_core::entities::Action;

/// Forward a fixed-URL blob through the cached-forward helper (the IDE fetches
/// these at startup — the highest-value offline case).
async fn forward_blob(
    svc: &Arc<ProxyService>,
    upstream_map: &UpstreamMap,
    client: &reqwest::Client,
    registry: &str,
    req: &HttpRequest,
    path: &str,
) -> Result<HttpResponse, AppError> {
    let key = forward_cache_key(registry, path, req.query_string());
    let path_and_query = if req.query_string().is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", req.query_string())
    };
    cached_forward_get(svc, upstream_map, client, registry, &path_and_query, &key)
        .await
        .map(super::cached_forward::ForwardedBody::into_response)
}

/// `files/pluginsXMLIds.json` — every known plugin xmlId.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/files/pluginsXMLIds.json",
    tag = "proxy/jetbrains-marketplace",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Array of plugin xmlIds", body = Vec<String>),
        (status = 404, description = "Unknown or non-marketplace registry"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[get("/proxy/{registry}/files/pluginsXMLIds.json")]
pub async fn jbm_plugins_xml_ids(
    req: HttpRequest,
    path: web::Path<String>,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    upstream_map: web::Data<UpstreamMap>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_jbm(&registry, &map)?;
    let mode = mode_map.get(&registry);

    if mode == RegistryMode::Proxy {
        // Pure proxy: the upstream list is authoritative — pass it (or its
        // cached/stale copy) through, and surface upstream failure honestly.
        return forward_blob(
            &svc,
            &upstream_map,
            &client,
            &registry,
            &req,
            "files/pluginsXMLIds.json",
        )
        .await;
    }

    let mut ids: Vec<String> = local_svc
        .backend
        .list_package_names(&registry)
        .await
        .map_err(AppError::from)?;

    if mode == RegistryMode::Hybrid {
        // Best-effort union with upstream; upstream loss keeps the local list.
        let key = forward_cache_key(&registry, "files/pluginsXMLIds.json", "");
        let upstream_ids = cached_forward_get(
            &svc,
            &upstream_map,
            &client,
            &registry,
            "files/pluginsXMLIds.json",
            &key,
        )
        .await
        .ok()
        .and_then(|fwd| serde_json::from_slice::<Vec<String>>(&fwd.body).ok());
        if let Some(upstream_ids) = upstream_ids {
            extend_unseen(&mut ids, upstream_ids);
        }
    }

    Ok(HttpResponse::Ok().json(ids))
}

/// Append the ids `ids` does not already hold, preserving local order.
///
/// Set-backed rather than `Vec::contains`: upstream carries tens of thousands
/// of ids, and a linear scan per id would make this union quadratic.
fn extend_unseen(ids: &mut Vec<String>, incoming: Vec<String>) {
    let mut seen: std::collections::HashSet<String> = ids.iter().cloned().collect();
    for id in incoming {
        if seen.insert(id.clone()) {
            ids.push(id);
        }
    }
}

// rustfmt is not idempotent on the utoipa attribute inside a macro body (it
// re-indents it on every run), which would make `fmt --check` flap.
#[rustfmt::skip]
macro_rules! fixed_blob_route {
    ($fn_name:ident, $route:literal, $upstream_path:literal, $doc:literal) => {
        #[doc = $doc]
        #[utoipa::path(
            get,
            path = $route,
            tag = "proxy/jetbrains-marketplace",
            params(("registry" = String, Path, description = "Registry name")),
            responses(
                (status = 200, description = "Blob content", body = ArtifactBytes, content_type = "application/octet-stream"),
                (status = 404, description = "Unknown or non-marketplace registry"),
            ),
            security(("bearer_token" = [])),
        )]
        #[get($route)]
        pub async fn $fn_name(
            req: HttpRequest,
            path: web::Path<String>,
            svc: web::Data<Arc<ProxyService>>,
            map: web::Data<RegistryMap>,
            mode_map: web::Data<RegistryModeMap>,
            upstream_map: web::Data<UpstreamMap>,
            client: web::Data<reqwest::Client>,
        ) -> Result<impl Responder, AppError> {
            let registry = path.into_inner();
            require_jbm(&registry, &map)?;
            if mode_map.get(&registry) == RegistryMode::Local {
                return Ok(HttpResponse::Ok()
                    .content_type("application/json")
                    .body("[]"));
            }
            forward_blob(
                &svc,
                &upstream_map,
                &client,
                &registry,
                &req,
                $upstream_path,
            )
            .await
        }
    };
}

fixed_blob_route!(
    jbm_jb_plugins_xml_ids,
    "/proxy/{registry}/files/jbPluginsXMLIds.json",
    "files/jbPluginsXMLIds.json",
    "`files/jbPluginsXMLIds.json` — JetBrains-authored plugin ids."
);
fixed_blob_route!(
    jbm_broken_plugins,
    "/proxy/{registry}/files/brokenPlugins.json",
    "files/brokenPlugins.json",
    "`files/brokenPlugins.json` — the IDE's known-broken plugin list."
);
fixed_blob_route!(
    jbm_ide_extensions,
    "/proxy/{registry}/files/IDE/extensions.json",
    "files/IDE/extensions.json",
    "`files/IDE/extensions.json` — IDE extension descriptors."
);

/// Load render entries for one plugin: local first in Local/Hybrid, cached
/// proxy metadata otherwise (`Ok(None)` = fall through happened but nothing
/// found upstream either).
async fn load_entries(
    svc: &Arc<ProxyService>,
    local_svc: &Arc<LocalRegistryService>,
    mode: RegistryMode,
    registry: &str,
    xml_id: &str,
    identity: AuthIdentity,
) -> Result<Vec<RenderEntry>, AppError> {
    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        match local_svc
            .get_jetbrains_versions(registry, xml_id, &identity)
            .await
        {
            Ok(versions) => {
                return Ok(versions.iter().map(RenderEntry::from_local).collect());
            }
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }
    let proxy_req = ProxyRequest {
        package_id: PackageId::new(registry, xml_id, "latest"),
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let meta = svc
        .resolve_metadata_for(&proxy_req)
        .await
        .map_err(AppError::from)?;
    let extra = ExtraMeta::from_extra(&meta.extra);
    Ok(extra
        .versions
        .iter()
        .map(|v| RenderEntry::from_extra(xml_id, &extra, v))
        .collect())
}

/// `files/{plugin}/meta.json` — plugin-level metadata blob.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/files/{plugin}/meta.json",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("plugin" = String, Path, description = "Plugin xmlId"),
    ),
    responses(
        (status = 200, description = "Plugin metadata", body = UpstreamDocument),
        (status = 404, description = "Unknown registry or plugin"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/files/{plugin}/meta.json")]
pub async fn jbm_plugin_meta(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, xml_id) = path.into_inner();
    require_jbm(&registry, &map)?;
    validate_package_name(&xml_id).map_err(AppError::from)?;

    let mode = mode_map.get(&registry);
    let entries = load_entries(&svc, &local_svc, mode, &registry, &xml_id, identity).await?;
    Ok(HttpResponse::Ok().json(plugin_meta_json(&xml_id, &entries)))
}

/// `files/{plugin}/{update}/meta.json` — update-level metadata blob.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/files/{plugin}/{update}/meta.json",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("plugin" = String, Path, description = "Plugin xmlId"),
        ("update" = String, Path, description = "Plugin version"),
    ),
    responses(
        (status = 200, description = "Update metadata", body = UpstreamDocument),
        (status = 404, description = "Unknown registry, plugin, or version"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/files/{plugin}/{update}/meta.json")]
pub async fn jbm_update_meta(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, xml_id, version) = path.into_inner();
    require_jbm(&registry, &map)?;
    validate_package_name(&xml_id).map_err(AppError::from)?;
    validate_path_safe("version", &version).map_err(AppError::from)?;

    let mode = mode_map.get(&registry);
    let entries = load_entries(&svc, &local_svc, mode, &registry, &xml_id, identity).await?;
    let entry = entries
        .iter()
        .find(|e| e.version == version)
        .ok_or_else(|| {
            AppError::not_found(format!("version '{version}' of '{xml_id}' not found"))
        })?;
    Ok(HttpResponse::Ok().json(update_meta_json(entry)))
}

/// `files/{plugin}/{update}/{fileName}` — the IDE `/files/` artifact
/// passthrough. On the proxy path `plugin`/`update` carry the upstream ids
/// verbatim (artifact `"file/{fileName}"`).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/files/{plugin}/{update}/{file_name}",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("plugin" = String, Path, description = "Plugin xmlId (or upstream plugin id)"),
        ("update" = String, Path, description = "Plugin version (or upstream update id)"),
        ("file_name" = String, Path, description = "Artifact file name"),
    ),
    responses(
        (status = 200, description = "Artifact bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 404, description = "Unknown registry or artifact"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/files/{plugin}/{update}/{file_name}")]
pub async fn jbm_file_download(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let (registry, xml_id, version, file_name) = path.into_inner();
    require_jbm(&registry, &map)?;
    validate_package_name(&xml_id).map_err(AppError::from)?;
    validate_path_safe("version", &version).map_err(AppError::from)?;
    validate_path_safe("file name", &file_name).map_err(AppError::from)?;
    super::require_single_segment("file name", &file_name)?;

    let artifact = format!("file/{file_name}");
    serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        &xml_id,
        &version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: &artifact,
            local_content_type: content_type_for(&file_name),
            proxy_content_type: None,
            action: Action::ReleasesRead,
            check_prerelease: false,
            append_signature: true,
        },
    )
    .await
}

/// `Content-Disposition` value naming the plugin archive. The canonical
/// download URLs (`plugin/download?pluginId=…`, `pluginManager?action=…`) have
/// no filename in their path, and IntelliJ's `PluginDownloader` rejects the
/// response with "Invalid filename returned by a server" unless this header
/// provides one.
///
/// `xml_id`/`version` only went through `validate_path_safe`, which still
/// admits `"` and `/` — sanitise so the quoted-string stays well-formed and
/// the name is a single path segment.
fn plugin_attachment_value(xml_id: &str, version: &str) -> Result<header::HeaderValue, AppError> {
    let file_name = crate::handlers::sanitize_filename(&format!("{xml_id}-{version}.zip"));
    header::HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\""))
        .map_err(|e| AppError::bad_request(format!("invalid plugin file name: {e}")))
}

#[derive(Debug, Deserialize)]
pub struct PluginDownloadQuery {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub version: String,
    pub channel: Option<String>,
}

/// `plugin/download?pluginId=&version=[&channel=]` — the canonical download
/// URL (also what our generated `updatePlugins.xml` points at).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/plugin/download",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("pluginId" = String, Query, description = "Plugin xmlId"),
        ("version" = String, Query, description = "Plugin version"),
        ("channel" = Option<String>, Query, description = "Release channel"),
    ),
    responses(
        (status = 200, description = "Plugin archive bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 404, description = "Unknown registry or plugin"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/plugin/download")]
pub async fn jbm_plugin_download(
    path: web::Path<String>,
    query: web::Query<PluginDownloadQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_jbm(&registry, &map)?;
    validate_package_name(&query.plugin_id).map_err(AppError::from)?;
    validate_path_safe("version", &query.version).map_err(AppError::from)?;

    let artifact = match query.channel.as_deref().filter(|c| !c.is_empty()) {
        Some(channel) => {
            validate_path_safe("channel", channel).map_err(AppError::from)?;
            format!("{PLUGIN_ARTIFACT}@{channel}")
        }
        None => PLUGIN_ARTIFACT.to_owned(),
    };
    let mut resp = serve_local_or_proxy_artifact(
        svc,
        local_svc,
        &mode_map,
        &registry,
        &query.plugin_id,
        &query.version,
        identity,
        LocalOrProxyArtifactOpts {
            artifact_suffix: &artifact,
            local_content_type: "application/octet-stream",
            proxy_content_type: None,
            action: Action::ReleasesRead,
            check_prerelease: false,
            append_signature: true,
        },
    )
    .await?;
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        plugin_attachment_value(&query.plugin_id, &query.version)?,
    );
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct PluginManagerQuery {
    pub action: String,
    pub id: String,
    pub build: Option<String>,
}

/// `pluginManager?action=download&id=&build=` — resolve the newest
/// build-compatible version, then serve its artifact. Cached by construction:
/// the version resolves from the (stale-capable) metadata and the artifact
/// streams through the storage cache.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/pluginManager",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("action" = String, Query, description = "Only 'download' is supported"),
        ("id" = String, Query, description = "Plugin xmlId"),
        ("build" = Option<String>, Query, description = "IDE build for compatibility"),
    ),
    responses(
        (status = 200, description = "Plugin archive bytes", body = ArtifactBytes, content_type = "application/octet-stream"),
        (status = 400, description = "Unsupported action"),
        (status = 404, description = "Unknown registry, plugin, or no compatible version"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/pluginManager")]
pub async fn jbm_plugin_manager(
    path: web::Path<String>,
    query: web::Query<PluginManagerQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_jbm(&registry, &map)?;
    if query.action != "download" {
        return Err(AppError::bad_request(format!(
            "unsupported action '{}'; expected 'download'",
            query.action
        )));
    }
    validate_package_name(&query.id).map_err(AppError::from)?;
    let build = query.build.as_deref();

    let mode = mode_map.get(&registry);
    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        match local_svc
            .get_jetbrains_versions(&registry, &query.id, &identity)
            .await
        {
            Ok(versions) => {
                let best = versions
                    .iter()
                    .filter(|v| v.channel == STABLE_CHANNEL)
                    .filter(|v| build.is_none_or(|b| v.compatible_with(b)))
                    .max_by_key(|v| v.published_at)
                    .ok_or_else(|| {
                        AppError::not_found(format!(
                            "no version of '{}' is compatible with build '{}'",
                            query.id,
                            build.unwrap_or("any")
                        ))
                    })?;
                let bytes = local_svc
                    .get_artifact(
                        &registry,
                        &query.id,
                        &best.version,
                        Action::ReleasesRead,
                        &identity,
                    )
                    .await
                    .map_err(AppError::from)?;
                return Ok(HttpResponse::Ok()
                    .content_type("application/octet-stream")
                    .insert_header((
                        header::CONTENT_DISPOSITION,
                        plugin_attachment_value(&query.id, &best.version)?,
                    ))
                    .body(bytes));
            }
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(e) => return Err(AppError::from(e)),
        }
    }

    // Proxy (or hybrid miss): resolve the compatible version from cached
    // metadata, then stream through the artifact cache.
    let proxy_req = ProxyRequest {
        package_id: PackageId::new(&registry, &query.id, "latest"),
        identity: identity.0.clone(),
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    let meta = svc
        .resolve_metadata_for(&proxy_req)
        .await
        .map_err(AppError::from)?;
    let extra = ExtraMeta::from_extra(&meta.extra);
    let best = extra.newest_compatible(build).ok_or_else(|| {
        AppError::not_found(format!(
            "no version of '{}' is compatible with build '{}'",
            query.id,
            build.unwrap_or("any")
        ))
    })?;
    let version = best
        .version
        .clone()
        .ok_or_else(|| AppError::not_found("upstream version list is empty".to_owned()))?;

    let pkg = PackageId::new(&registry, &query.id, &version).with_artifact(PLUGIN_ARTIFACT);
    let mut resp = proxy_stream(svc, pkg, identity, Action::ReleasesRead, None).await?;
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        plugin_attachment_value(&query.id, &version)?,
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::plugin_attachment_value;

    #[test]
    fn attachment_value_names_the_archive() {
        let v = plugin_attachment_value("org.demo.a", "1.0.0").unwrap();
        assert_eq!(
            v.to_str().unwrap(),
            r#"attachment; filename="org.demo.a-1.0.0.zip""#
        );
    }

    #[test]
    fn attachment_value_sanitizes_quotes_slashes_and_non_ascii() {
        // `validate_path_safe` admits all three of these; the quoted-string
        // and the single-segment filename must survive anyway.
        let v = plugin_attachment_value(r#"org."evil"#, "1/0é").unwrap();
        assert_eq!(
            v.to_str().unwrap(),
            r#"attachment; filename="org._evil-1_0_.zip""#
        );
    }
}
