//! The search routes that had none: npm, cargo and Composer.
//!
//! RFC 0009 §7.7. Three protocols, three response shapes, one path — every one
//! of them renders from [`ProxyService::search`], so they cannot come to
//! disagree about what this registry contains or about which versions a block
//! has hidden.
//!
//! NuGet's `/v3/query` and the `vsx` gallery are the other two callers; they
//! live with their own protocols because both had a route already (NuGet's
//! returning a hardcoded empty result, which is what §5.1 is about).

use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};
use serde::Deserialize;

use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::PackageId;
use batlehub_core::services::{LocalRegistryService, ProxyService, SearchMode, SearchResults};

use crate::handlers::proxy::common::require_registry_type;
use crate::handlers::schemas::ProtocolDocument;
use crate::{error::AppError, extractors::AuthIdentity, RegistryMap, RegistryModeMap};
use batlehub_core::entities::Action;

/// Map a registry mode onto the search sources it may use.
pub(crate) fn search_mode(mode: RegistryMode) -> SearchMode {
    match mode {
        RegistryMode::Local => SearchMode::Local,
        RegistryMode::Hybrid => SearchMode::Hybrid,
        RegistryMode::Proxy => SearchMode::Proxy,
    }
}

#[derive(Debug, Deserialize)]
pub struct NpmSearchQuery {
    #[serde(default)]
    pub text: String,
    #[serde(default = "default_size")]
    pub size: usize,
    /// Offset into the result set. npm sends `from` on every search — measured
    /// in RFC 0009 §12.1 — and reading only `text` and `size` makes every page
    /// the first one, the same defect §12.4 found on the NuGet side.
    #[serde(default)]
    pub from: usize,
}

#[derive(Debug, Deserialize)]
pub struct CargoSearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_size")]
    pub per_page: usize,
}

#[derive(Debug, Deserialize)]
pub struct ComposerListQuery {
    #[serde(default)]
    pub filter: String,
}

#[derive(Debug, Deserialize)]
pub struct ComposerSearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_size")]
    pub per_page: usize,
}

fn default_size() -> usize {
    20
}

/// Check the registry speaks `kind`, then search it.
///
/// The four handlers below differ at two ends — the protocol they answer for,
/// and the shape they render — and agree on everything in between. That middle
/// is here so they cannot drift apart on which sources a search draws from,
/// which is the property the module doc claims.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_and_search(
    registry: &str,
    kind: &str,
    query: &str,
    limit: usize,
    identity: &AuthIdentity,
    svc: &ProxyService,
    local_svc: &LocalRegistryService,
    map: &RegistryMap,
    mode_map: &RegistryModeMap,
) -> Result<SearchResults, AppError> {
    require_registry_type(registry, kind, map)?;

    // The registry's own RBAC, before anything is read. The comment that used to
    // stand here claimed "a hit names only what the listing filters already
    // allow, so there is nothing further to authorise" — and neither half held:
    // the local half went through `list_package_names`, which applies no filter
    // of any kind, and the chain was never consulted, so a registry that denies
    // anonymous reads outright still answered these routes with every private
    // package name it held (survey finding 11).
    //
    // A search is a listing, so only the identity-keyed rule runs: the gate
    // rules judge a concrete version and a search result set names many. The
    // coordinate is the registry itself — there is no one package to name.
    svc.authorize_listing(
        &PackageId::registry_document(registry, "_search", "latest"),
        &identity.0,
        // §4.2 — this document names many packages and no single version, which
        // is `releases:list`'s definition.
        Action::ReleasesList,
    )
    .await
    .map_err(AppError::from)?;

    // Bounded before anything reads: `search_local` walks every published name
    // matching the query and evaluates the rule chain per candidate, and it
    // stops early only once it has `limit` *hits* — so a caller who may see
    // nothing walks the whole registry, at whatever `limit` the client asked
    // for. `ProxyService::search` clamps to the same range a moment later, so
    // this costs no reachable result: a page past 250 was never served.
    let limit = limit.clamp(1, 250);

    // Published packages, filtered to what this caller may see. Supplied by the
    // web layer rather than read inside `ProxyService::search` because published
    // packages live in `LocalRegistryBackend`, a different store from the
    // `PackageRepository` the proxy's held set comes from — the first records
    // what was published here, the second what was fetched through here. A
    // local-mode registry has only the first, which is why a search that read
    // only the second returned nothing for a package it had just accepted.
    let local = local_svc
        .search_local(registry, query, limit, &identity.0)
        .await;
    svc.search(
        registry,
        query,
        limit,
        search_mode(mode_map.get(registry)),
        local,
    )
    .await
    .map_err(AppError::from)
}

/// `npm search` / `npm search --json`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/-/v1/search",
    tag = "proxy/npm",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("text"     = Option<String>, Query, description = "Search text"),
        ("size"     = Option<usize>, Query, description = "Maximum results"),
        ("from"     = Option<usize>, Query, description = "Offset into the result set"),
    ),
    responses(
        (status = 200, description = "npm search results", body = ProtocolDocument),
        (status = 403, description = "Access denied by the registry's rule chain"),
        (status = 404, description = "Unknown or non-npm registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/-/v1/search")]
pub async fn npm_search(
    path: web::Path<String>,
    query: web::Query<NpmSearchQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    let query_text = query.text.clone();
    // The window is offset + page, and the page is sliced out of it below: a
    // search limited to `size` with the offset applied afterwards answers the
    // second page with nothing.
    let window = query.from.saturating_add(query.size);

    let results = resolve_and_search(
        &registry,
        "npm",
        &query_text,
        window,
        &identity,
        &svc,
        &local_svc,
        &map,
        &mode_map,
    )
    .await?;

    let objects: Vec<serde_json::Value> = results
        .hits
        .iter()
        .skip(query.from)
        .take(query.size)
        .map(|h| {
            serde_json::json!({
                "package": {
                    "name": h.name,
                    "version": h.version,
                    "description": h.description.clone().unwrap_or_default(),
                    // npm dereferences `maintainers` without a guard —
                    // `data.maintainers.map(m => m.username)` in
                    // `lib/utils/format-search-stream.js` — so omitting it does
                    // not degrade the output, it crashes the client with
                    // "Cannot read properties of undefined (reading 'map')"
                    // against a `200` carrying the right hits. Empty because
                    // this layer has no maintainer list, not as a placeholder:
                    // `keywords` and `date` are guarded there and are left out
                    // rather than invented (RFC 0009 §12.16).
                    "maintainers": [],
                }
            })
        })
        .collect();

    // `X-BatleHub-Cache` makes a degraded answer visible rather than silently
    // short: `stale` means the upstream was unreachable and this came from the
    // cache or from the packages we hold.
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .insert_header(("X-BatleHub-Cache", results.freshness.header_value()))
        .json(serde_json::json!({
            "objects": objects,
            "total": results.total,
            "time": chrono::Utc::now().to_rfc3339(),
        })))
}

/// `cargo search`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/api/v1/crates",
    tag = "proxy/cargo",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("q"        = Option<String>, Query, description = "Search text"),
        ("per_page" = Option<usize>, Query, description = "Maximum results"),
    ),
    responses(
        (status = 200, description = "cargo search results", body = ProtocolDocument),
        (status = 403, description = "Access denied by the registry's rule chain"),
        (status = 404, description = "Unknown or non-cargo registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/api/v1/crates")]
pub async fn cargo_search(
    path: web::Path<String>,
    query: web::Query<CargoSearchQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    let (query_text, limit) = (query.q.clone(), query.per_page);

    let results = resolve_and_search(
        &registry,
        "cargo",
        &query_text,
        limit,
        &identity,
        &svc,
        &local_svc,
        &map,
        &mode_map,
    )
    .await?;

    let crates: Vec<serde_json::Value> = results
        .hits
        .iter()
        .map(|h| {
            serde_json::json!({
                "name": h.name,
                "max_version": h.version,
                "description": h.description.clone().unwrap_or_default(),
            })
        })
        .collect();

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .insert_header(("X-BatleHub-Cache", results.freshness.header_value()))
        .json(serde_json::json!({
            "crates": crates,
            "meta": { "total": results.total },
        })))
}

/// `composer` bulk package enumeration — `list.json`.
///
/// Names only, no versions: it is the Composer equivalent of RubyGems' `/names`
/// and carries the same consequence — a block has nothing in it to hide, and
/// removing a partly-blocked package would report it as nonexistent.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/list.json",
    tag = "proxy/composer",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("filter"   = Option<String>, Query, description = "Optional name filter"),
    ),
    responses(
        (status = 200, description = "Package name list", body = ProtocolDocument),
        (status = 403, description = "Access denied by the registry's rule chain"),
        (status = 404, description = "Unknown or non-composer registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/list.json")]
pub async fn composer_list(
    path: web::Path<String>,
    query: web::Query<ComposerListQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    let (query_text, limit) = (query.filter.clone(), 250usize);

    let results = resolve_and_search(
        &registry,
        "composer",
        &query_text,
        limit,
        &identity,
        &svc,
        &local_svc,
        &map,
        &mode_map,
    )
    .await?;

    let names: Vec<String> = results.hits.iter().map(|h| h.name.clone()).collect();

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .insert_header(("X-BatleHub-Cache", results.freshness.header_value()))
        .json(serde_json::json!({ "packageNames": names })))
}

/// `composer search`.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/search.json",
    tag = "proxy/composer",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("q"        = Option<String>, Query, description = "Search text"),
        ("per_page" = Option<usize>, Query, description = "Maximum results"),
    ),
    responses(
        (status = 200, description = "Composer search results", body = ProtocolDocument),
        (status = 403, description = "Access denied by the registry's rule chain"),
        (status = 404, description = "Unknown or non-composer registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/search.json")]
pub async fn composer_search(
    path: web::Path<String>,
    query: web::Query<ComposerSearchQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    let (query_text, limit) = (query.q.clone(), query.per_page);

    let results = resolve_and_search(
        &registry,
        "composer",
        &query_text,
        limit,
        &identity,
        &svc,
        &local_svc,
        &map,
        &mode_map,
    )
    .await?;

    let items: Vec<serde_json::Value> = results
        .hits
        .iter()
        .map(|h| {
            serde_json::json!({
                "name": h.name,
                "description": h.description.clone().unwrap_or_default(),
            })
        })
        .collect();

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .insert_header(("X-BatleHub-Cache", results.freshness.header_value()))
        .json(serde_json::json!({
            "results": items,
            "total": results.total,
        })))
}
