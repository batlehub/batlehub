//! `GET /api/v1/explore/packages` — the catalogue's listing and its search.
//!
//! Two searches, deliberately not blended into one relevance score. A **name**
//! match is what a reader means when they type a name, and it always outranks a
//! prose match however densely the prose repeats the word (RFC 0007-bis §4.3).
//! Every row says which it was, because a result that matches nothing the reader
//! can see reads as a bug.

use std::collections::HashMap;

use batlehub_core::entities::{resolve_state, ResolutionPolicy};
use batlehub_core::services::LocalRegistryService;
use chrono::Utc;

use super::{
    format_dt, get, web, AdminService, AppError, Arc, AuthIdentity, Deserialize, ExploreFilter,
    ExploreSortBy, IntoParams, PackageSource, ProxyService, Responder, Serialize, ToSchema,
};

// ── List packages (collapsed) ─────────────────────────────────────────────────

/// Where a search looks.
///
/// `name` is today's behaviour byte for byte, and it is the default: a
/// parameter's absence must not change what the endpoint already did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchScope {
    #[default]
    Name,
    Readme,
    Both,
}

impl SearchScope {
    fn searches_prose(self) -> bool {
        matches!(self, Self::Readme | Self::Both)
    }
    fn searches_names(self) -> bool {
        matches!(self, Self::Name | Self::Both)
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Readme => "readme",
            Self::Both => "both",
        }
    }
}

/// How many prose candidates one search reads.
///
/// A cap rather than full pagination through the index: prose search is a
/// discovery tool, and a reader who is on page eleven of a phrase search has
/// been failed by the query rather than by the limit. The response says when it
/// applied, because a silently truncated list reads as "that is all there is".
const PROSE_SEARCH_CAP: u64 = 200;

#[derive(Deserialize, IntoParams)]
pub struct ExploreQuery {
    pub registry: Option<String>,
    pub name: Option<String>,
    /// What to search for. `name` is the older parameter and still works; `q` is
    /// what the console sends, and the only one `in` applies to.
    #[serde(default)]
    pub q: Option<String>,
    /// `name` (default) | `readme` | `both`.
    ///
    /// Inlined into the parameter rather than referenced, for the reason
    /// `ReadmeQuery::format` gives: nothing returns this enum in a body, so
    /// utoipa emits no component and a `$ref` would reach the console as
    /// `unknown`.
    #[serde(rename = "in", default)]
    #[param(inline)]
    pub search_in: SearchScope,
    /// Sort order: `downloads` (default), `name`, `recent`, or `fetched`.
    ///
    /// `recent` is when a client last downloaded a package *from* this
    /// instance; `fetched` is when this instance last pulled one *from
    /// upstream*. The catalog is ordered on the latter.
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub page: u64,
    /// Rows in the answer. Absent means `[limits].packages_per_page`, which is
    /// also the most this may be — a larger ask is clamped down to it, and the
    /// applied value is reported back in `per_page`.
    ///
    /// The console deliberately sends nothing here: the catalog *is* this list,
    /// so the number the operator configured is the right one, and a console
    /// that asked for its own would make the setting inert on the one page it
    /// exists for.
    #[serde(default)]
    pub per_page: Option<u64>,
}

#[derive(Serialize, ToSchema)]
pub struct ExplorePackageListResponse {
    pub items: Vec<ExploreEntryDto>,
    pub total: usize,
    pub page: u64,
    pub per_page: u64,
    /// `true` when the upstream database was unreachable and no cached data was available.
    /// The result set will be empty; the UI should surface a warning to the user.
    pub upstream_unavailable: bool,
    /// The scan-status batch failed or timed out, so no row carries
    /// `scan_status`; the list is otherwise current (RFC 0044 §10.2).
    pub scan_status_unavailable: bool,
    /// Whether `[search] readmes` is on for this instance.
    ///
    /// So a client can tell "no package here says that" from "this instance does
    /// not search prose". `in=readme` with the feature off is accepted and
    /// answers as `in=name` does — a parameter that silently means something
    /// else is the failure this RFC family keeps finding — and this field is how
    /// the console says which happened (RFC 0007-bis §4.3).
    pub readme_search_enabled: bool,
    /// The scope actually applied, which is `name` when prose search is off
    /// however the request asked.
    pub searched_in: String,
    /// The prose search hit [`PROSE_SEARCH_CAP`] and there may be more.
    ///
    /// Reported rather than swallowed: a silently shortened list reads as "that
    /// is all there is", which is a lie about the catalogue.
    pub truncated: bool,
}

#[derive(Serialize, ToSchema, Clone)]
pub struct ExploreEntryDto {
    pub registry: String,
    pub name: String,
    pub version_count: u64,
    pub total_downloads: u64,
    /// When a client last downloaded this package from us (RFC 3339).
    pub last_accessed: Option<String>,
    /// `"proxied"` | `"local"` | `"both"`
    pub source: String,
    /// An administrator has blocked at least one version.
    pub has_blocked: bool,
    /// An owner has yanked at least one locally published version.
    ///
    /// Previously reported inside `has_blocked`, which conflated an operator's
    /// refusal with an owner's withdrawal.
    pub has_yanked: bool,
    /// This package's standing here, in DESIGN.md's resolution vocabulary:
    /// `"cached"` | `"stale"` | `"held"` | `"pending"` | `"yanked"` |
    /// `"blocked"`.
    ///
    /// Derived server-side rather than in the client because two of the six
    /// need configuration the browser has no business holding — the registry's
    /// artifact TTL, and the release-age gate's window and bypass roles — and
    /// because `"held"` depends on *who is asking*.
    pub state: String,
    /// How many versions this instance currently holds bytes for.
    pub cached_versions: u64,
    /// Bytes held across those versions. `null` means unknown, not zero: sizes
    /// were not recorded for artifacts cached before migration 004.
    pub cached_bytes: Option<u64>,
    /// When this instance last fetched any version from upstream (RFC 3339).
    pub last_fetched_at: Option<String>,
    /// The version most recently obtained, by `cached_at` for a proxied
    /// artifact or `published_at` for a local one. `null` when nothing is held.
    pub newest_version: Option<String>,
    /// `name` | `readme` | `both` — why this row is here.
    ///
    /// A row whose name has nothing to do with the query and whose README
    /// mentions it in passing is a *correct* result and an inexplicable one
    /// without the label (RFC 0007-bis §4.3).
    pub matched_in: String,
    /// The matched fragment of the README, as **plain text**, or `null`.
    ///
    /// Plain, and rendered as text. It never reaches `v-html`: the README
    /// panel's is a deliberate, tested, single-component boundary
    /// (RFC 0007 §6.5), and a search snippet is a second surface for
    /// package-authored content reached by a much cheaper path — no navigation,
    /// just a query (RFC 0007-bis §7.4).
    pub snippet: Option<String>,
    /// The scan status of `newest_version`, as last recorded (RFC 0044 §4.2).
    /// Absent on a registry without `[security]` and for a caller without
    /// `quarantine:read` alike, so the two cannot be told apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_status: Option<crate::handlers::security::ScanStatusDto>,
}

/// Explore available packages (one entry per unique package name).
///
/// Returns packages from both the proxy cache and locally published packages,
/// collapsed to one entry per registry+name combination.
/// Only registries the caller is allowed to explore are included.
#[utoipa::path(
    get,
    path = "/api/v1/explore/packages",
    tag = "explore",
    params(ExploreQuery),
    responses(
        (status = 200, description = "Package explorer listing", body = ExplorePackageListResponse),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/explore/packages")]
pub async fn explore_packages(
    query: web::Query<ExploreQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    search_cfg: web::Data<crate::SearchConfigLock>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // RFC 0015 §4.2 — `catalogue:browse`, resolved from grants, in place of
    // `AccessConfig`'s explore sets. §10 rule 2's conjunction reproduces those
    // sets exactly (§13.5 measured the naive reading at 19 disagreements before
    // it was corrected), so this is a substitution between two computations
    // already known to agree — not a new policy.
    let accessible = batlehub_core::services::authz::browsable_registries(&hot, &identity).await;

    // `[limits].packages_per_page` is both the unasked-for default and the
    // ceiling — one key, because the question an operator has is one question
    // (see the config crate). `clamp_pagination` still bounds the page number;
    // its `per_page` half is superseded here by the configured ceiling, which is
    // the operator's number rather than a constant.
    //
    // The clamp to at least 1 matters beyond politeness: an unclamped
    // `per_page=0` collapses `filter`'s cache key onto `count_filter`'s (both
    // would be limit=0, offset=0).
    let configured_per_page = local_svc.hot.read().await.packages_per_page;
    let (page, _) = crate::handlers::clamp_pagination(query.page, configured_per_page);
    let per_page = query
        .per_page
        .unwrap_or(configured_per_page)
        .clamp(1, configured_per_page);

    let readme_search_enabled = *search_cfg.read().await;
    // With the feature off, `in=readme` answers exactly as `in=name` does, and
    // the response says so. Accepting the parameter and quietly meaning
    // something else is the failure this RFC family keeps finding.
    // `q` is what the console sends; `name` is the older parameter and still
    // filters names, so a client that has not been updated keeps working.
    let term = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    // Prose search needs two things: the feature on, and something to search
    // for. Without a query there is nothing to match, so `in=readme` is a
    // *listing* — not an empty result, which would read as "no package here says
    // that" about a question nobody asked.
    let scope = if readme_search_enabled && term.is_some() {
        query.search_in
    } else {
        SearchScope::Name
    };
    let name_filter = if scope.searches_names() {
        term.clone().or_else(|| query.name.clone())
    } else {
        // `in=readme` means prose *only*, so the name filter is not applied —
        // otherwise the two would AND and the scope would be a narrowing rather
        // than a choice.
        None
    };

    // An empty accessible set is **nothing**, not "no restriction".
    //
    // `ExploreFilter::registries` is a scope, and every implementation of it
    // reads an empty vector as unfiltered: `prepare_registries_param` binds
    // `NULL` and the SQL is `$3::text[] IS NULL OR ps.registry = ANY($3)`, the
    // in-memory repository is `filter.registries.is_empty() || contains(…)`.
    // So a caller with no browsable registry at all — an anonymous visitor on a
    // server with `rbac.explore.anonymous = false`, a role denied explore
    // everywhere — was handed the *entire* catalogue by the one endpoint whose
    // whole job is to scope it. Refused here, before the scope is built, rather
    // than by teaching four repositories to tell "all" from "none" apart.
    let denied_everywhere = accessible.is_empty();
    let named_registry_denied = query
        .registry
        .as_ref()
        .is_some_and(|reg| !accessible.contains(reg));
    if denied_everywhere || named_registry_denied {
        return Ok(web::Json(ExplorePackageListResponse {
            items: vec![],
            total: 0,
            page,
            per_page,
            upstream_unavailable: false,
            scan_status_unavailable: false,
            readme_search_enabled,
            searched_in: scope.as_str().to_owned(),
            truncated: false,
        }));
    }

    let registries: Vec<String> = if query.registry.is_none() {
        accessible.iter().cloned().collect()
    } else {
        vec![]
    };

    let sort_by = sort_by_from_query(query.sort.as_deref());

    // Snapshot the per-registry resolution inputs before any `await`, per the
    // hot-reload convention: clone out of the lock and let a config swap
    // mid-request finish against the values this request started with. The map
    // is a handful of small structs, one per configured registry.
    let resolution: HashMap<String, ResolutionPolicy> =
        local_svc.hot.read().await.resolution.clone();

    // Registry-level access (above) is the coarse gate; this is the per-package
    // one. Without it an `internal`/`team` package's name and version count are
    // listed to anyone who can explore the registry, even though the same caller
    // gets a 403 trying to download it.
    let viewer = crate::handlers::explore_viewer_for(&identity);

    // The prose half, when there is one. Run first because it decides how the
    // rest of the request is shaped: with prose hits the two result sets are
    // merged in memory, and without them this is byte for byte the old path.
    let prose = prose_for_query(
        &proxy_svc,
        scope,
        term.as_deref(),
        query.registry.as_deref(),
        &accessible,
    )
    .await;
    let truncated = prose.len() as u64 > PROSE_SEARCH_CAP;
    let prose: Vec<_> = prose.into_iter().take(PROSE_SEARCH_CAP as usize).collect();

    let filter = ExploreFilter {
        registry: query.registry.clone(),
        registries: registries.clone(),
        name_contains: name_filter.clone(),
        name_in: vec![],
        sort_by: sort_by.clone(),
        limit: per_page,
        offset: page * per_page,
        viewer: viewer.clone(),
    };
    let count_filter = ExploreFilter {
        registry: query.registry.clone(),
        registries: registries.clone(),
        name_contains: name_filter.clone(),
        name_in: vec![],
        sort_by: sort_by.clone(),
        limit: 0,
        offset: 0,
        viewer: viewer.clone(),
    };

    // One instant for the whole page, not one per row: `Utc::now()` inside the
    // loop can straddle a TTL or a quarantine boundary, so two packages fetched
    // in the same second could be graded against different "now"s.
    let now = Utc::now();

    if prose.is_empty() && !scope.searches_prose() {
        // The path that has always existed: SQL paginates, nothing is merged.
        let ((packages, pkg_unavailable), (total, count_unavailable)) = tokio::try_join!(
            admin_svc.explore_packages(filter),
            admin_svc.count_explore_packages(count_filter),
        )
        .map_err(AppError::from)?;

        let mut items: Vec<ExploreEntryDto> = packages
            .into_iter()
            .map(|e| to_dto(e, &resolution, &identity, now, "name", None))
            .collect();
        let scan_status_unavailable = attach_scan_status(&hot, &identity, &mut items).await;

        return Ok(web::Json(ExplorePackageListResponse {
            total: total as usize,
            items,
            page,
            per_page,
            upstream_unavailable: pkg_unavailable || count_unavailable,
            scan_status_unavailable,
            readme_search_enabled,
            searched_in: scope.as_str().to_owned(),
            truncated: false,
        }));
    }

    let MergedSearchResult {
        merged,
        upstream_unavailable,
        names_truncated,
    } = merged_search(MergedSearch {
        admin_svc: &admin_svc,
        filter,
        registries,
        sort_by,
        viewer,
        registry: query.registry.as_deref(),
        prose: &prose,
        scope,
        resolution: &resolution,
        identity: &identity,
        now,
    })
    .await?;
    let truncated = truncated || names_truncated;

    let total = merged.len();
    let start = (page * per_page).min(total as u64) as usize;
    let end = (start + per_page as usize).min(total);
    let mut items = merged[start..end].to_vec();
    let scan_status_unavailable = attach_scan_status(&hot, &identity, &mut items).await;

    Ok(web::Json(ExplorePackageListResponse {
        total,
        items,
        page,
        per_page,
        upstream_unavailable,
        scan_status_unavailable,
        readme_search_enabled,
        searched_in: scope.as_str().to_owned(),
        truncated,
    }))
}

/// Each row's `scan_status`, for its most recently cached version, in one
/// batch for the page. Returns whether the batch failed.
async fn attach_scan_status(
    hot: &batlehub_core::services::hot_config::HotConfigLock,
    identity: &AuthIdentity,
    items: &mut [ExploreEntryDto],
) -> bool {
    let rows: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].newest_version.is_some())
        .collect();
    let coords: Vec<batlehub_core::entities::PackageId> = rows
        .iter()
        .map(|&i| {
            let e = &items[i];
            batlehub_core::entities::PackageId::new(
                &e.registry,
                &e.name,
                e.newest_version.as_deref().unwrap_or_default(),
            )
        })
        .collect();
    let (statuses, unavailable) =
        crate::handlers::security::scan_status_for(hot, &identity.0, &coords).await;
    for (i, status) in rows.into_iter().zip(statuses) {
        items[i].scan_status = status;
    }
    unavailable
}

/// The catalogue's sort order, defaulting to downloads for anything unknown.
///
/// An unrecognised `sort=` is the default rather than a `400`: the parameter
/// names an ordering, and every ordering answers the same question.
fn sort_by_from_query(sort: Option<&str>) -> ExploreSortBy {
    match sort {
        Some("name") => ExploreSortBy::Name,
        Some("recent") => ExploreSortBy::Recent,
        Some("fetched") => ExploreSortBy::Fetched,
        _ => ExploreSortBy::Downloads,
    }
}

/// The prose half of the answer, when this request has one.
///
/// Run before the rest because it decides how the request is shaped: with prose
/// hits the two result sets are merged in memory, and without them the handler
/// is byte for byte the old path.
async fn prose_for_query(
    proxy_svc: &ProxyService,
    scope: SearchScope,
    term: Option<&str>,
    registry: Option<&str>,
    accessible: &std::collections::HashSet<String>,
) -> Vec<batlehub_core::ports::ReadmeSearchHit> {
    if !scope.searches_prose() {
        return Vec::new();
    }
    let Some(term) = term else {
        return Vec::new();
    };
    let searched: Vec<String> = match registry {
        Some(one) => vec![one.to_owned()],
        None => accessible.iter().cloned().collect(),
    };
    prose_hits(proxy_svc, &searched, term).await
}

/// Everything the merged name+prose path reads, in one place — the same shape
/// `detail.rs`'s `LinkInput` uses, and for the same reason: this answer is drawn
/// from four sources and threading them positionally makes the call site
/// unreadable.
struct MergedSearch<'a> {
    admin_svc: &'a AdminService,
    /// The listing filter the plain path would have used; the name half reuses
    /// it with its own limit and offset.
    filter: ExploreFilter,
    registries: Vec<String>,
    sort_by: ExploreSortBy,
    viewer: batlehub_core::entities::ExploreViewer,
    registry: Option<&'a str>,
    prose: &'a [batlehub_core::ports::ReadmeSearchHit],
    scope: SearchScope,
    resolution: &'a HashMap<String, ResolutionPolicy>,
    identity: &'a AuthIdentity,
    now: chrono::DateTime<Utc>,
}

/// The merged list, and what the response has to disclose about how it was read.
struct MergedSearchResult {
    merged: Vec<ExploreEntryDto>,
    upstream_unavailable: bool,
    names_truncated: bool,
}

/// Name matches first, then prose-only hits in rank order.
///
/// Both halves are read whole, up to the cap, because "name first, then prose"
/// cannot be expressed as one SQL ordering without a weight — and a weight is
/// exactly what lets a sufficiently dense README out-score a package that *is*
/// called the thing (RFC 0007-bis §4.3).
async fn merged_search(input: MergedSearch<'_>) -> Result<MergedSearchResult, AppError> {
    let MergedSearch {
        admin_svc,
        filter,
        registries,
        sort_by,
        viewer,
        registry,
        prose,
        scope,
        resolution,
        identity,
        now,
    } = input;

    let (name_rows, name_unavailable) = if scope.searches_names() {
        admin_svc
            .explore_packages(ExploreFilter {
                // One more than the cap, so "there were more" is distinguishable
                // from "there were exactly this many" — the same trick the prose
                // half needs and the reason `truncated` counts both.
                limit: PROSE_SEARCH_CAP + 1,
                offset: 0,
                ..filter
            })
            .await
            .map_err(AppError::from)?
    } else {
        (Vec::new(), false)
    };
    // The name half is capped too, and until now nothing said so: `truncated`
    // was computed from the prose half alone, so `?q=a&in=both` on a catalogue
    // with 5 000 matching names reported `total: 200, truncated: false` — a
    // silently shortened list reading as "that is all there is", which is
    // precisely what the field's own doc comment says it exists to prevent.
    let names_truncated = name_rows.len() as u64 > PROSE_SEARCH_CAP;

    let mut merged: Vec<ExploreEntryDto> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for entry in name_rows.into_iter().take(PROSE_SEARCH_CAP as usize) {
        let key = (entry.registry.clone(), entry.name.clone());
        // A row that matched both ways says so, and keeps the snippet — the
        // reader gets the label *and* the reason.
        let also = prose
            .iter()
            .find(|h| h.registry == key.0 && h.name == key.1);
        let matched_in = if also.is_some() { "both" } else { "name" };
        let snippet = also.map(|h| h.snippet.clone());
        seen.insert(key);
        merged.push(to_dto(
            entry, resolution, identity, now, matched_in, snippet,
        ));
    }

    // Prose-only hits, in rank order, after every name match.
    let prose_only: Vec<&batlehub_core::ports::ReadmeSearchHit> = prose
        .iter()
        .filter(|h| !seen.contains(&(h.registry.clone(), h.name.clone())))
        .collect();
    append_prose_only_rows(AppendProseOnly {
        merged: &mut merged,
        prose_only: &prose_only,
        admin_svc,
        registry,
        registries,
        sort_by,
        viewer,
        resolution,
        identity,
        now,
    })
    .await?;

    Ok(MergedSearchResult {
        merged,
        upstream_unavailable: name_unavailable,
        names_truncated,
    })
}

/// What [`append_prose_only_rows`] needs, for the same reason [`MergedSearch`]
/// is a struct.
struct AppendProseOnly<'a> {
    merged: &'a mut Vec<ExploreEntryDto>,
    prose_only: &'a [&'a batlehub_core::ports::ReadmeSearchHit],
    admin_svc: &'a AdminService,
    registry: Option<&'a str>,
    registries: Vec<String>,
    sort_by: ExploreSortBy,
    viewer: batlehub_core::entities::ExploreViewer,
    resolution: &'a HashMap<String, ResolutionPolicy>,
    identity: &'a AuthIdentity,
    now: chrono::DateTime<Utc>,
}

/// Append the packages whose README matched but whose name did not.
///
/// Their catalogue rows are read through the same visibility gate the listing
/// applies — a package hidden from the listing must not become visible by
/// quoting a phrase from its README (RFC 0007-bis §7.3).
async fn append_prose_only_rows(input: AppendProseOnly<'_>) -> Result<(), AppError> {
    if input.prose_only.is_empty() {
        return Ok(());
    }
    let names: Vec<(String, String)> = input
        .prose_only
        .iter()
        .map(|h| (h.registry.clone(), h.name.clone()))
        .collect();
    let (rows, _) = input
        .admin_svc
        .explore_packages(ExploreFilter {
            registry: input.registry.map(str::to_owned),
            registries: input.registries,
            name_contains: None,
            name_in: names,
            sort_by: input.sort_by,
            limit: PROSE_SEARCH_CAP,
            offset: 0,
            viewer: input.viewer,
        })
        .await
        .map_err(AppError::from)?;
    // Ordered by the search's ranking, not by the catalogue's sort: the reader
    // asked a question and these are the answers in order of how well they
    // answer it.
    for hit in input.prose_only {
        // No row means the package's README is stored but the catalogue does not
        // show it to this caller. Dropped silently, which is the point — a `404`
        // would confirm it exists.
        let Some(entry) = rows
            .iter()
            .find(|e| e.registry == hit.registry && e.name == hit.name)
        else {
            continue;
        };
        input.merged.push(to_dto(
            entry.clone(),
            input.resolution,
            input.identity,
            input.now,
            "readme",
            Some(hit.snippet.clone()),
        ));
    }
    Ok(())
}

/// The prose search, or nothing when this instance cannot run one.
///
/// A search failure is not an error the reader should see: the name half of the
/// answer is still correct and still useful, and a `500` on the catalogue's
/// front page because the FTS index is mid-rebuild would be a worse outcome than
/// a shorter list.
async fn prose_hits(
    proxy_svc: &ProxyService,
    registries: &[String],
    term: &str,
) -> Vec<batlehub_core::ports::ReadmeSearchHit> {
    let Some(readme_svc) = proxy_svc.readme.as_ref() else {
        return Vec::new();
    };
    // One more than the cap, for the same reason the name half asks for one
    // more: `truncated` is answered by comparing the count to the cap, and with
    // a limit *equal* to the cap a search that found exactly `PROSE_SEARCH_CAP`
    // hits and one that found ten thousand are the same number. The extra row
    // is dropped by the caller; only its existence is used.
    match readme_svc
        .repo
        .search(registries, term, PROSE_SEARCH_CAP + 1)
        .await
    {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(error = %e, "explore: README search failed; answering on names alone");
            Vec::new()
        }
    }
}

/// One catalogue row as the console sees it.
fn to_dto(
    e: batlehub_core::entities::ExploreEntry,
    resolution: &HashMap<String, ResolutionPolicy>,
    identity: &AuthIdentity,
    now: chrono::DateTime<Utc>,
    matched_in: &str,
    snippet: Option<String>,
) -> ExploreEntryDto {
    // A registry with no entry gets the default policy: no TTL, no age gate.
    // That is the same answer as a registry configured with neither, which is
    // what an unknown registry effectively is here.
    let policy = resolution.get(&e.registry).cloned().unwrap_or_default();
    let state = resolve_state(&e, &policy, &identity.0.role, now);
    ExploreEntryDto {
        registry: e.registry,
        name: e.name,
        version_count: e.version_count,
        total_downloads: e.total_downloads,
        last_accessed: e.last_accessed.map(format_dt),
        source: match e.source {
            PackageSource::Proxied => "proxied".to_string(),
            PackageSource::Local => "local".to_string(),
            PackageSource::Both => "both".to_string(),
        },
        has_blocked: e.has_blocked,
        has_yanked: e.has_yanked,
        state: state.as_str().to_string(),
        cached_versions: e.cached_versions,
        cached_bytes: e.cached_bytes,
        last_fetched_at: e.last_fetched_at.map(format_dt),
        newest_version: e.newest_version,
        matched_in: matched_in.to_owned(),
        snippet,
        scan_status: None,
    }
}
