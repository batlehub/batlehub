//! IDE-facing JSON API: search, compatible-updates, per-plugin objects, and
//! the small auxiliary endpoints (`aggregation`, `getImplementations`,
//! comments).

use std::sync::Arc;

use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use serde::Deserialize;

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{Identity, PackageId, RegistryKind},
    error::CoreError,
    services::{
        validate_package_name, JetbrainsPluginVersion, LocalRegistryService, ProxyRequest,
        ProxyService,
    },
};

use super::cached_forward::{
    cached_forward_get, cached_forward_post_json, compatible_updates_cache_key, forward_cache_key,
    ForwardedBody,
};
use super::render::{plugin_json, search_hit_json, update_json, ExtraMeta, RenderEntry};
use super::{require_jbm, require_single_segment, STABLE_CHANNEL};
use crate::handlers::schemas::UpstreamDocument;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap, UpstreamMap};
use batlehub_core::entities::Action;

const DEFAULT_SEARCH_MAX: usize = 50;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// `/api/searchPlugins` uses `search`; `/api/search/plugins` also accepts
    /// `search`.
    pub search: Option<String>,
    pub max: Option<usize>,
    pub offset: Option<usize>,
}

/// Local search: newest stable version of every plugin whose xmlId or name
/// contains the query (case-insensitive).
async fn local_search_entries(
    local_svc: &Arc<LocalRegistryService>,
    registry: &str,
    query: &SearchQuery,
    identity: &Identity,
) -> Result<Vec<RenderEntry>, AppError> {
    let needle = query.search.clone().unwrap_or_default().to_lowercase();
    let plugins: Vec<JetbrainsPluginVersion> = local_svc
        .get_jetbrains_plugins(registry, None, STABLE_CHANNEL, identity)
        .await
        .map_err(AppError::from)?;
    let max = query.max.unwrap_or(DEFAULT_SEARCH_MAX).min(200);
    let offset = query.offset.unwrap_or(0);
    Ok(plugins
        .iter()
        .filter(|p| {
            needle.is_empty()
                || p.xml_id.to_lowercase().contains(&needle)
                || p.name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&needle))
        })
        .skip(offset)
        .take(max)
        .map(RenderEntry::from_local)
        .collect())
}

/// Forward a search-shaped GET through the cached-forward helper.
/// [`forward_search`], parsed, with every failure collapsed into `None`.
///
/// The hybrid merges below are best-effort by design: an unreachable
/// marketplace, or one that answers with something that is not JSON, must not
/// fail a search the local half has already answered.
async fn forwarded_search_json(
    svc: &Arc<ProxyService>,
    upstream_map: &UpstreamMap,
    client: &reqwest::Client,
    registry: &str,
    req: &HttpRequest,
    path: &str,
) -> Option<serde_json::Value> {
    let fwd = forward_search(svc, upstream_map, client, registry, req, path)
        .await
        .ok()?;
    serde_json::from_slice(&fwd.body).ok()
}

/// Append the upstream hits whose `xmlId` no local entry already claims.
///
/// Local wins: a plugin published here shadows the marketplace's copy of the
/// same id, rather than appearing twice.
fn merge_upstream_hits<'a>(
    hits: &mut Vec<serde_json::Value>,
    local_ids: &[&str],
    upstream: impl Iterator<Item = &'a serde_json::Value>,
    id_of: fn(&serde_json::Value) -> &str,
) {
    for hit in upstream {
        let id = id_of(hit);
        if !id.is_empty() && !local_ids.contains(&id) {
            hits.push(hit.clone());
        }
    }
}

/// The search endpoints name a hit's plugin with `xmlId`.
fn search_hit_id(hit: &serde_json::Value) -> &str {
    hit.get("xmlId").and_then(|v| v.as_str()).unwrap_or("")
}

/// The compatible-updates endpoint has three spellings in the wild.
fn update_hit_id(hit: &serde_json::Value) -> &str {
    hit.get("pluginXmlId")
        .or_else(|| hit.get("xmlId"))
        .or_else(|| hit.get("pluginId"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

async fn forward_search(
    svc: &Arc<ProxyService>,
    upstream_map: &UpstreamMap,
    client: &reqwest::Client,
    registry: &str,
    req: &HttpRequest,
    path: &str,
) -> Result<ForwardedBody, AppError> {
    let key = forward_cache_key(registry, path, req.query_string());
    let path_and_query = if req.query_string().is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", req.query_string())
    };
    cached_forward_get(svc, upstream_map, client, registry, &path_and_query, &key).await
}

/// `/api/searchPlugins?search=&max=` — `{plugins, total}` shape.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/searchPlugins",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("search" = Option<String>, Query, description = "Search text"),
        ("max" = Option<usize>, Query, description = "Maximum hits"),
    ),
    responses(
        (status = 200, description = "Search results", body = UpstreamDocument),
        (status = 404, description = "Unknown or non-marketplace registry"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[get("/proxy/{registry}/api/searchPlugins")]
pub async fn jbm_search_plugins_ide(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SearchQuery>,
    identity: AuthIdentity,
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
        return forward_search(
            &svc,
            &upstream_map,
            &client,
            &registry,
            &req,
            "api/searchPlugins",
        )
        .await
        .map(ForwardedBody::into_response);
    }

    let local = local_search_entries(&local_svc, &registry, &query, &identity).await?;
    let mut hits: Vec<serde_json::Value> = local.iter().map(search_hit_json).collect();

    if mode == RegistryMode::Hybrid {
        if let Some(body) = forwarded_search_json(
            &svc,
            &upstream_map,
            &client,
            &registry,
            &req,
            "api/searchPlugins",
        )
        .await
        {
            let local_ids: Vec<&str> = local.iter().map(|e| e.xml_id.as_str()).collect();
            merge_upstream_hits(
                &mut hits,
                &local_ids,
                body.get("plugins")
                    .and_then(|p| p.as_array())
                    .into_iter()
                    .flatten(),
                search_hit_id,
            );
        }
    }

    let total = hits.len();
    Ok(HttpResponse::Ok().json(serde_json::json!({ "plugins": hits, "total": total })))
}

/// `/api/search/plugins?search=&build=` — array shape used by the IDE's
/// full-replacement search.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/search/plugins",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("search" = Option<String>, Query, description = "Search text"),
        ("max" = Option<usize>, Query, description = "Maximum hits"),
    ),
    responses(
        (status = 200, description = "Search results", body = UpstreamDocument),
        (status = 404, description = "Unknown or non-marketplace registry"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[get("/proxy/{registry}/api/search/plugins")]
pub async fn jbm_search_plugins(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SearchQuery>,
    identity: AuthIdentity,
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
        return forward_search(
            &svc,
            &upstream_map,
            &client,
            &registry,
            &req,
            "api/search/plugins",
        )
        .await
        .map(ForwardedBody::into_response);
    }

    let local = local_search_entries(&local_svc, &registry, &query, &identity).await?;
    let mut hits: Vec<serde_json::Value> = local.iter().map(search_hit_json).collect();

    if mode == RegistryMode::Hybrid {
        if let Some(body) = forwarded_search_json(
            &svc,
            &upstream_map,
            &client,
            &registry,
            &req,
            "api/search/plugins",
        )
        .await
        {
            let local_ids: Vec<&str> = local.iter().map(|e| e.xml_id.as_str()).collect();
            merge_upstream_hits(
                &mut hits,
                &local_ids,
                body.as_array().into_iter().flatten(),
                search_hit_id,
            );
        }
    }

    Ok(HttpResponse::Ok().json(hits))
}

#[derive(Debug, Deserialize)]
pub struct CompatibleUpdatesRequest {
    pub build: String,
    #[serde(default, alias = "pluginXmlIds", rename = "pluginXMLIds")]
    pub plugin_xml_ids: Vec<String>,
}

/// `POST /api/search/updates/compatible` — newest compatible update per
/// requested xmlId.
#[utoipa::path(
    post,
    path = "/proxy/{registry}/api/search/updates/compatible",
    tag = "proxy/jetbrains-marketplace",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Array of compatible updates", body = Vec<UpstreamDocument>),
        (status = 404, description = "Unknown or non-marketplace registry"),
    ),
    security(("bearer_token" = [])),
)]
#[allow(clippy::too_many_arguments)]
#[post("/proxy/{registry}/api/search/updates/compatible")]
pub async fn jbm_compatible_updates(
    path: web::Path<String>,
    body: web::Json<CompatibleUpdatesRequest>,
    identity: AuthIdentity,
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
    let body = body.into_inner();

    let forward_body = serde_json::json!({
        "build": body.build,
        "pluginXMLIds": body.plugin_xml_ids,
    });
    let cache_key = compatible_updates_cache_key(&registry, &body.build, &body.plugin_xml_ids);

    if mode == RegistryMode::Proxy {
        return cached_forward_post_json(
            &svc,
            &upstream_map,
            &client,
            &registry,
            "api/search/updates/compatible",
            &forward_body,
            &cache_key,
        )
        .await
        .map(ForwardedBody::into_response);
    }

    let local = local_svc
        .get_jetbrains_compatible_updates(
            &registry,
            &body.plugin_xml_ids,
            &body.build,
            STABLE_CHANNEL,
            &identity,
        )
        .await
        .map_err(AppError::from)?;
    let local_ids: Vec<&str> = local.iter().map(|v| v.xml_id.as_str()).collect();
    let mut updates: Vec<serde_json::Value> = local
        .iter()
        .map(|v| update_json(&RenderEntry::from_local(v)))
        .collect();

    if mode == RegistryMode::Hybrid {
        // Best-effort upstream merge for the ids without a local answer.
        let upstream = cached_forward_post_json(
            &svc,
            &upstream_map,
            &client,
            &registry,
            "api/search/updates/compatible",
            &forward_body,
            &cache_key,
        )
        .await
        .ok()
        .and_then(|fwd| serde_json::from_slice::<serde_json::Value>(&fwd.body).ok());
        if let Some(upstream) = upstream {
            merge_upstream_hits(
                &mut updates,
                &local_ids,
                upstream.as_array().into_iter().flatten(),
                update_hit_id,
            );
        }
    }

    Ok(HttpResponse::Ok().json(updates))
}

/// Load all versions of one plugin as render entries (local-first, cached
/// proxy metadata fallback) — shared by the per-plugin JSON endpoints.
async fn plugin_entries(
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
            Ok(versions) => return Ok(versions.iter().map(RenderEntry::from_local).collect()),
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
    let mut entries: Vec<RenderEntry> = extra
        .versions
        .iter()
        .map(|v| RenderEntry::from_extra(xml_id, &extra, v))
        .collect();

    // The one chokepoint every JetBrains Marketplace listing resolves through —
    // `updatePlugins.xml`, `/plugins/list` and `/api/plugins/{id}/updates` all
    // render from this list. Filtering here rather than per document is the
    // same shape as the local path's `load_visible_versions`, which already
    // runs `filter_blocked` for the branch above.
    //
    // Without it an IDE reads the custom-repository XML, offers the operator's
    // blocked plugin build as an available update, and the install fails at
    // download.
    let blocked = svc
        .blocked_versions_for(registry, xml_id, RegistryKind::JetbrainsMarketplace)
        .await;
    if !blocked.is_empty() {
        let before = entries.len();
        entries.retain(|e| !blocked.contains(&e.version));
        if entries.len() < before {
            tracing::debug!(
                registry = %registry,
                plugin = %xml_id,
                removed = before - entries.len(),
                "hid blocked plugin builds from the marketplace listings"
            );
        }
    }
    Ok(entries)
}

/// `/api/plugins/{id}` — plugin object (id = xmlId).
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/plugins/{id}",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id" = String, Path, description = "Plugin xmlId"),
    ),
    responses(
        (status = 200, description = "Plugin object", body = UpstreamDocument),
        (status = 404, description = "Unknown registry or plugin"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/plugins/{id}")]
pub async fn jbm_plugin_info(
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
    let entries = plugin_entries(&svc, &local_svc, mode, &registry, &xml_id, identity).await?;
    let newest = entries
        .iter()
        .max_by_key(|e| e.date_ms.unwrap_or(i64::MIN))
        .ok_or_else(|| AppError::not_found(format!("plugin '{xml_id}' not found")))?;
    Ok(HttpResponse::Ok().json(plugin_json(newest)))
}

/// `/api/plugins/{id}/updates` — every version, newest first.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/plugins/{id}/updates",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id" = String, Path, description = "Plugin xmlId"),
    ),
    responses(
        (status = 200, description = "Array of updates", body = Vec<UpstreamDocument>),
        (status = 404, description = "Unknown registry or plugin"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/plugins/{id}/updates")]
pub async fn jbm_plugin_updates(
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
    let mut entries = plugin_entries(&svc, &local_svc, mode, &registry, &xml_id, identity).await?;
    entries.sort_by_key(|e| std::cmp::Reverse(e.date_ms.unwrap_or(i64::MIN)));
    let updates: Vec<serde_json::Value> = entries.iter().map(update_json).collect();
    Ok(HttpResponse::Ok().json(updates))
}

/// Auxiliary endpoints that are empty locally and cached-forwarded otherwise.
#[allow(clippy::too_many_arguments)]
async fn empty_or_forward(
    svc: &Arc<ProxyService>,
    upstream_map: &UpstreamMap,
    client: &reqwest::Client,
    registry: &str,
    map: &RegistryMap,
    mode_map: &RegistryModeMap,
    req: &HttpRequest,
    upstream_path: &str,
) -> Result<HttpResponse, AppError> {
    require_jbm(registry, map)?;
    if mode_map.get(registry) == RegistryMode::Local {
        return Ok(HttpResponse::Ok()
            .content_type("application/json")
            .body("[]"));
    }
    forward_search(svc, upstream_map, client, registry, req, upstream_path)
        .await
        .map(ForwardedBody::into_response)
}

/// `/api/search/aggregation/{field}` — facet values for the marketplace UI.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/search/aggregation/{field}",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("field" = String, Path, description = "Aggregation field"),
    ),
    responses((status = 200, description = "Aggregation values", body = UpstreamDocument)),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/search/aggregation/{field}")]
pub async fn jbm_aggregation(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    upstream_map: web::Data<UpstreamMap>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let (registry, field) = path.into_inner();
    batlehub_core::services::validate_package_name(&field).map_err(AppError::from)?;
    require_single_segment("aggregation field", &field)?;
    empty_or_forward(
        &svc,
        &upstream_map,
        &client,
        &registry,
        &map,
        &mode_map,
        &req,
        &format!("api/search/aggregation/{field}"),
    )
    .await
}

/// `/feature/getImplementations` — feature-implementation lookup.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/feature/getImplementations",
    tag = "proxy/jetbrains-marketplace",
    params(("registry" = String, Path, description = "Registry name")),
    responses((status = 200, description = "Feature implementations", body = Vec<UpstreamDocument>)),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/feature/getImplementations")]
pub async fn jbm_feature_implementations(
    req: HttpRequest,
    path: web::Path<String>,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    upstream_map: web::Data<UpstreamMap>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    empty_or_forward(
        &svc,
        &upstream_map,
        &client,
        &registry,
        &map,
        &mode_map,
        &req,
        "feature/getImplementations",
    )
    .await
}

/// `/api/products/intellij/plugins/{id}/comments` — plugin comments.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/products/intellij/plugins/{id}/comments",
    tag = "proxy/jetbrains-marketplace",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("id" = String, Path, description = "Plugin xmlId"),
    ),
    responses((status = 200, description = "Comments", body = Vec<UpstreamDocument>)),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/products/intellij/plugins/{id}/comments")]
pub async fn jbm_comments(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    svc: web::Data<Arc<ProxyService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    upstream_map: web::Data<UpstreamMap>,
    client: web::Data<reqwest::Client>,
) -> Result<impl Responder, AppError> {
    let (registry, id) = path.into_inner();
    validate_package_name(&id).map_err(AppError::from)?;
    require_single_segment("plugin id", &id)?;
    empty_or_forward(
        &svc,
        &upstream_map,
        &client,
        &registry,
        &map,
        &mode_map,
        &req,
        &format!("api/products/intellij/plugins/{id}/comments"),
    )
    .await
}
