use super::fetch::FetchOfferDto;
use super::{
    get, web, AdminService, AppError, Arc, AuthIdentity, Deserialize, ExploreFilter, ExploreSortBy,
    IntoParams, ProxyService, Responder, Serialize, ToSchema,
};

// ── Registry stats ─────────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct RegistryStatDto {
    pub registry: String,
    pub package_count: u64,
    pub total_downloads: u64,
    /// Bytes held for this registry. `null` means unknown, not zero: sizes were
    /// not recorded for artifacts cached before migration 004.
    pub cached_bytes: Option<u64>,
}

#[derive(Serialize, ToSchema)]
pub struct ExploreRegistryStatsResponse {
    pub registries: Vec<RegistryStatDto>,
    /// `true` when the upstream database was unreachable and no cached data was available.
    pub upstream_unavailable: bool,
}

/// Per-registry package counts and download totals for the explorer sidebar.
#[utoipa::path(
    get,
    path = "/api/v1/explore/registries",
    tag = "explore",
    responses(
        (status = 200, description = "Registry statistics", body = ExploreRegistryStatsResponse),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/explore/registries")]
pub async fn explore_registry_stats(
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // RFC 0015 §4.2 — `catalogue:browse`, resolved from grants, in place of
    // `AccessConfig`'s explore sets. §10 rule 2's conjunction reproduces those
    // sets exactly (§13.5 measured the naive reading at 19 disagreements before
    // it was corrected), so this is a substitution between two computations
    // already known to agree — not a new policy.
    let accessible: Vec<String> =
        batlehub_core::services::authz::browsable_registries(&hot, &identity)
            .await
            .into_iter()
            .collect();

    // An empty accessible set is **nothing**, not "no restriction" — the same
    // rule `explore_packages` states at length beside its own scope. The
    // repository closes this too, so neither layer is the only thing standing
    // between a caller with no browsable registry and the whole estate's
    // numbers; survey finding 2 shipped because one layer was.
    if accessible.is_empty() {
        return Ok(web::Json(ExploreRegistryStatsResponse {
            registries: vec![],
            upstream_unavailable: false,
        }));
    }

    // RFC 0015 §4.4 — every number below is an aggregate over packages, so it is
    // computed over the ones this caller may see. The same viewer the listing
    // beside it uses, so the tile and the page agree.
    let viewer = crate::handlers::explore_viewer_for(&identity);
    let (stats, upstream_unavailable) = admin_svc
        .registry_explore_stats(&accessible, &viewer)
        .await
        .map_err(AppError::from)?;

    let registries: Vec<RegistryStatDto> = stats
        .into_iter()
        .map(|s| RegistryStatDto {
            registry: s.registry,
            package_count: s.package_count,
            total_downloads: s.total_downloads,
            cached_bytes: s.cached_bytes,
        })
        .collect();

    Ok(web::Json(ExploreRegistryStatsResponse {
        registries,
        upstream_unavailable,
    }))
}

// ── Upstream package search ────────────────────────────────────────────────────

#[derive(Deserialize, IntoParams)]
pub struct UpstreamSearchQuery {
    pub name: String,
    /// Specific registry to search. When absent, searches all accessible registries.
    pub registry: Option<String>,
    /// Results asked of each registry searched: 10 by default, 100 at most.
    /// A larger value is not an error, it is simply not honoured.
    #[serde(default = "default_upstream_limit")]
    pub limit: usize,
}

fn default_upstream_limit() -> usize {
    10
}

/// The most results one registry is asked for, whatever the caller asked.
///
/// This is a ceiling on the work a single request can ask of the database, not
/// a product decision about page size: the search fans out across every
/// registry the caller may browse, and each hit becomes up to two coordinates
/// in the held-set query's `name_in` array and one unit of its `LIMIT`. Left
/// open, one request could bind an arbitrarily large array.
///
/// Invisible today, and deliberately chosen to be: every client that honours
/// the limit already clamps it lower before it reaches the upstream — NuGet and
/// cargo at 100, npm, Composer, OpenVSX, Maven and JetBrains at 50, Terraform
/// at 25 — so this changes no answer any registry currently gives. It is the
/// bound that stops a future adapter passing the number through, or a local
/// search answering it exactly.
pub const MAX_UPSTREAM_SEARCH_LIMIT: usize = 100;

#[derive(Serialize, ToSchema)]
pub struct UpstreamSearchResponse {
    pub items: Vec<UpstreamPackageDto>,
}

#[derive(Serialize, ToSchema)]
pub struct UpstreamPackageDto {
    pub registry: String,
    pub name: String,
    pub latest_version: String,
    pub description: Option<String>,
    /// `true` when this package already exists in the proxy cache or local registry.
    pub already_cached: bool,
    /// Whether the catalogue may offer **Fetch** on this row (RFC 0007-bis
    /// §11 q3).
    ///
    /// Per row rather than per response: a listing with no registry filter fans
    /// out across every registry the caller may browse, and `console_fetch` and
    /// the kind's own answer are both per registry — so one flag for the page
    /// would be wrong for some of its rows.
    pub fetch: FetchOfferDto,
}

/// Search upstream registries for packages not yet in the proxy.
///
/// Queries each accessible registry's upstream search API. Results include a
/// `already_cached` flag so the UI can distinguish new discoveries from known packages.
#[utoipa::path(
    get,
    path = "/api/v1/explore/upstream",
    tag = "explore",
    params(UpstreamSearchQuery),
    responses(
        (status = 200, description = "Upstream search results", body = UpstreamSearchResponse),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/explore/upstream")]
pub async fn explore_upstream_search(
    query: web::Query<UpstreamSearchQuery>,
    identity: AuthIdentity,
    proxy_svc: web::Data<Arc<ProxyService>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
    registry_map: web::Data<crate::RegistryMap>,
) -> Result<impl Responder, AppError> {
    // §4.2 — `catalogue:browse`, as in `explore_registry_stats` above.
    let accessible = batlehub_core::services::authz::browsable_registries(&hot, &identity).await;

    tracing::info!(
        name = %query.name,
        accessible_registries = ?accessible,
        "upstream search: resolving clients"
    );

    // Collect registry clients to search (snapshot from hot config)
    let clients_to_search: Vec<(String, Arc<dyn batlehub_core::ports::RegistryClient>)> = {
        let hot = proxy_svc.hot.read().await;
        hot.registries
            .iter()
            .filter(|(name, _)| {
                if let Some(ref reg) = query.registry {
                    name.as_str() == reg && accessible.contains(name.as_str())
                } else {
                    accessible.contains(name.as_str())
                }
            })
            .map(|(name, client)| (name.clone(), Arc::clone(client)))
            .collect()
    };

    tracing::info!(
        clients = ?clients_to_search.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "upstream search: clients selected"
    );

    // Fan out search across all matching registry clients concurrently
    let search_futures: Vec<_> = clients_to_search
        .iter()
        .map(|(reg_name, client)| {
            let reg = reg_name.clone();
            let q = query.name.clone();
            let lim = query.limit.min(MAX_UPSTREAM_SEARCH_LIMIT);
            let client = Arc::clone(client);
            async move {
                let results = match client.search_packages(&q, lim).await {
                    Ok(r) => {
                        tracing::info!(registry = %reg, count = r.len(), "upstream search: got results");
                        r
                    }
                    Err(e) => {
                        tracing::warn!(registry = %reg, error = %e, "upstream search: client error");
                        vec![]
                    }
                };
                results
                    .into_iter()
                    .map(move |p| (reg.clone(), p))
                    .collect::<Vec<_>>()
            }
        })
        .collect();

    let hits: Vec<_> = futures::future::join_all(search_futures)
        .await
        .into_iter()
        .flatten()
        .collect();

    // Which of those we already hold, asked **by the names that came back**.
    //
    // This used to ask `name_contains = <the query>`, which cannot answer the
    // question: an upstream search is a relevance search, so a query of
    // `left-pad` returns `pad-left`, `lpad` and `@stdlib/string-left-pad`, and
    // not one of them contains the query as a substring. Every such row was
    // reported `already_cached: false` however many times the instance had
    // pulled it — and the catalogue's Fetch button made that visible: press it,
    // watch the version arrive, and the row goes on offering to fetch it, with
    // a second press answering `409 fetch.already-held` (RFC 0007-bis §14.11).
    //
    // `name_in` is exact and takes the registry with the name, so it also
    // cannot credit a package in one registry for a namesake in another. It
    // goes through the same visibility gate as before: marking a package the
    // caller may not see as "already cached" would disclose its existence just
    // as surely as listing it.
    //
    // Asked in **both** spellings, and answered against the canonical one.
    // Three kinds hold a package under a name their own search does not return:
    // NuGet's search says `Newtonsoft.Json` where `dotnet restore` stored
    // `newtonsoft.json`, PyPI's says `Pillow` where the simple index stored
    // `pillow`, and pkg.go.dev says `github.com/BurntSushi/toml` where the `go`
    // client stored `github.com/!burnt!sushi/toml`. Comparing the two
    // spellings exactly is §14.11's symptom surviving in exactly the
    // ecosystems that spell a name two ways: the row reports
    // `already_cached: false` for a package the instance holds, and the Fetch
    // button beside it answers `409 fetch.already-held`.
    //
    // `RegistryKind::canonical_package_name` is the read path's own rule — the
    // NuGet and PyPI adapters' normalisers now delegate to it — so the flag
    // agrees with what the package manager will actually find. The raw
    // spelling is asked for as well, and dropped when it is the canonical one:
    // `name_in` is an exact match, so a row stored before its handler
    // normalised (or by a kind whose upstream is looser than its read path)
    // would otherwise go unseen. At most two coordinates per hit, and the
    // canonical comparison below credits either.
    let canonical = |registry: &str, name: &str| -> String {
        registry_map
            .type_of(registry)
            .and_then(|t| t.parse::<batlehub_core::entities::RegistryKind>().ok())
            .map(|kind| kind.canonical_package_name(name).into_owned())
            .unwrap_or_else(|| name.to_string())
    };
    let mut asked: Vec<(String, String)> = Vec::with_capacity(hits.len());
    let mut asked_seen: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for (reg, pkg) in &hits {
        for spelling in [canonical(reg, &pkg.name), pkg.name.clone()] {
            let coordinate = (reg.clone(), spelling);
            if asked_seen.insert(coordinate.clone()) {
                asked.push(coordinate);
            }
        }
    }
    let known_filter = ExploreFilter {
        registry: query.registry.clone(),
        registries: if query.registry.is_none() {
            accessible.into_iter().collect()
        } else {
            vec![]
        },
        name_contains: None,
        // The limit is the number of coordinates asked about, not a round
        // number: `name_in` restricts the answer to at most one row per
        // `(registry, name)` pair, so anything smaller silently drops rows —
        // and the drop is by name order, so it is the tail of the page that
        // goes back to reporting `already_cached: false`, which is the bug
        // this filter was written to fix. `limit` on the query above has no
        // ceiling and the search fans out across every accessible registry,
        // so `hits` is not bounded by any constant that could be written here.
        limit: asked.len() as u64,
        name_in: asked,
        sort_by: ExploreSortBy::Name,
        offset: 0,
        viewer: crate::handlers::explore_viewer_for(&identity),
    };
    // Nothing came back, so there is nothing to ask about — and an empty
    // `name_in` is "no restriction", which would fetch the whole catalogue to
    // annotate zero rows.
    //
    // **Uncached**, and deliberately: `explore_packages` writes every answer to
    // the ten-minute explore cache, which frees entries only on an explicit
    // invalidation, and this filter's key carries the exact set of names a
    // third-party relevance search happened to return. Two such keys match only
    // if the upstream answered identically for the same viewer, so the entry
    // would be written, never read and never freed — a caller varying `name`
    // would grow the map for nothing. `explore_packages_uncached` says the rest.
    //
    // A repository error leaves the set empty rather than failing the search:
    // the third-party hits are what the reader asked for and the flag only
    // decorates them, and the safe direction is the conservative one — a Fetch
    // button offered on a package the instance already holds answers
    // `409 fetch.already-held`, where a button withheld hides a fetch that
    // would have worked.
    let known_set: std::collections::HashSet<(String, String)> = if known_filter.name_in.is_empty()
    {
        std::collections::HashSet::new()
    } else {
        admin_svc
            .explore_packages_uncached(known_filter)
            .await
            .inspect_err(|e| {
                tracing::warn!(error = %e, "upstream search: the held-set lookup failed; every row will say it is not held");
            })
            .unwrap_or_default()
            .iter()
            .map(|e| (e.registry.clone(), canonical(&e.registry, &e.name)))
            .collect()
    };

    let results: Vec<_> = hits
        .into_iter()
        .map(|(registry, pkg)| {
            let already_cached =
                known_set.contains(&(registry.clone(), canonical(&registry, &pkg.name)));
            (registry, pkg, already_cached)
        })
        .collect();

    // One answer per registry, not per row: `fetch_offer` takes the hot-config
    // read lock, and a page of fifty hits from one registry would otherwise take
    // it fifty times to compute the same thing.
    let mut offers: std::collections::HashMap<String, FetchOfferDto> =
        std::collections::HashMap::new();
    for (registry, ..) in &results {
        if !offers.contains_key(registry) {
            let offer = super::fetch::fetch_offer(&hot, &registry_map, registry, &identity.0).await;
            offers.insert(registry.clone(), offer);
        }
    }

    let items = results
        .into_iter()
        .map(|(registry, pkg, already_cached)| {
            let fetch = offers.get(&registry).cloned().unwrap_or(FetchOfferDto {
                offered: false,
                reason: None,
            });
            UpstreamPackageDto {
                registry,
                name: pkg.name,
                latest_version: pkg.latest_version,
                description: pkg.description,
                already_cached,
                fetch,
            }
        })
        .collect();

    Ok(web::Json(UpstreamSearchResponse { items }))
}
