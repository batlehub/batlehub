//! XML-facing endpoints: the `updatePlugins.xml` custom-repository document
//! (additive `idea.plugin.hosts` flow, local content only) and the classic
//! `/plugins/list` plugin-repository lookup.

use std::sync::Arc;

use actix_web::{get, web, HttpRequest, HttpResponse, Responder};
use serde::Deserialize;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    services::{validate_package_name, LocalRegistryService, ProxyRequest, ProxyService},
};

use super::render::{plugin_repository_xml, update_plugins_xml, ExtraMeta, RenderEntry};
use super::{registry_public_base, require_jbm, STABLE_CHANNEL};
use crate::handlers::proxy::common::require_local_mode;
use crate::handlers::schemas::ProtocolDocument;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

#[derive(Debug, Deserialize)]
pub struct UpdatePluginsQuery {
    /// IDE build for compatibility filtering (e.g. `IU-241.14494`).
    pub build: Option<String>,
    pub channel: Option<String>,
}

/// `updatePlugins.xml` — the custom-plugin-repository document the IDE polls
/// when this proxy is added via *Manage Plugin Repositories*. Local content
/// only (404 in pure proxy mode).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/updatePlugins.xml",
    tag = "proxy/jetbrains-marketplace",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Custom repository XML", body = ProtocolDocument, content_type = "application/xml"),
        (status = 404, description = "Unknown registry, wrong type, or proxy-only registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/updatePlugins.xml")]
pub async fn jbm_update_plugins_xml(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<UpdatePluginsQuery>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_jbm(&registry, &map)?;
    require_local_mode(&registry, &mode_map)?;

    let channel = query.channel.as_deref().unwrap_or(STABLE_CHANNEL);
    let plugins = local_svc
        .get_jetbrains_plugins(&registry, query.build.as_deref(), channel, &identity)
        .await
        .map_err(AppError::from)?;

    let base = registry_public_base(&req, &registry);
    let entries: Vec<RenderEntry> = plugins
        .iter()
        .map(|p| {
            let mut e = RenderEntry::from_local(p);
            e.download_url = Some(format!(
                "{base}/plugin/download?pluginId={}&version={}",
                p.xml_id, p.version
            ));
            e
        })
        .collect();

    Ok(HttpResponse::Ok()
        .content_type("application/xml")
        .body(update_plugins_xml(&entries)?))
}

#[derive(Debug, Deserialize)]
pub struct PluginsListQuery {
    #[serde(rename = "pluginId")]
    pub plugin_id: Option<String>,
}

const EMPTY_REPOSITORY: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plugin-repository/>";

fn xml_response(body: String) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/xml")
        .body(body)
}

/// `/plugins/list?pluginId=` — classic plugin-repository XML: every version of
/// one plugin. Unknown plugins answer an empty `<plugin-repository/>` (200),
/// matching upstream semantics.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/plugins/list",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("pluginId" = Option<String>, Query, description = "Plugin xmlId"),
    ),
    responses(
        (status = 200, description = "Plugin repository XML", body = ProtocolDocument, content_type = "application/xml"),
        (status = 404, description = "Unknown or non-marketplace registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/plugins/list")]
pub async fn jbm_plugins_list(
    path: web::Path<String>,
    query: web::Query<PluginsListQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    require_jbm(&registry, &map)?;
    let mode = mode_map.get(&registry);

    let Some(xml_id) = query.plugin_id.as_deref().filter(|s| !s.is_empty()) else {
        // Without a pluginId the upstream lists the whole marketplace; locally
        // we list every published plugin, and in pure proxy mode we decline.
        if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
            let plugins = local_svc
                .get_jetbrains_plugins(&registry, None, STABLE_CHANNEL, &identity)
                .await
                .map_err(AppError::from)?;
            let entries: Vec<RenderEntry> = plugins.iter().map(RenderEntry::from_local).collect();
            return Ok(xml_response(plugin_repository_xml(&entries)?));
        }
        return Err(AppError::bad_request(
            "pluginId query parameter is required".to_owned(),
        ));
    };
    validate_package_name(xml_id).map_err(AppError::from)?;

    if matches!(mode, RegistryMode::Local | RegistryMode::Hybrid) {
        match local_svc
            .get_jetbrains_versions(&registry, xml_id, &identity)
            .await
        {
            Ok(versions) => {
                let entries: Vec<RenderEntry> =
                    versions.iter().map(RenderEntry::from_local).collect();
                return Ok(xml_response(plugin_repository_xml(&entries)?));
            }
            Err(CoreError::NotFound(_)) if mode == RegistryMode::Hybrid => {}
            Err(CoreError::NotFound(_)) => {
                return Ok(xml_response(EMPTY_REPOSITORY.to_owned()));
            }
            Err(e) => return Err(AppError::from(e)),
        }
    }

    // Proxy (or hybrid miss): render from the cached metadata — offline-capable
    // once seen.
    let proxy_req = ProxyRequest {
        package_id: PackageId::new(&registry, xml_id, "latest"),
        identity: identity.0,
        action: Action::ReleasesRead.to_owned(),
        ip_address: identity.1.ip.clone(),
        user_agent: identity.1.user_agent.clone(),
    };
    match svc.resolve_metadata_for(&proxy_req).await {
        Ok(meta) => {
            let extra = ExtraMeta::from_extra(&meta.extra);
            let entries: Vec<RenderEntry> = extra
                .versions
                .iter()
                .map(|v| RenderEntry::from_extra(xml_id, &extra, v))
                .collect();
            Ok(xml_response(plugin_repository_xml(&entries)?))
        }
        Err(CoreError::NotFound(_)) => Ok(xml_response(EMPTY_REPOSITORY.to_owned())),
        Err(e) => Err(AppError::from(e)),
    }
}
