use super::{
    format_dt, get, web, AdminService, AppError, Arc, AuthIdentity, Deserialize, IntoParams,
    LocalRegistryService, PackageFilter, PackageStatus, Responder, Serialize, ToSchema,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{absent_readme_state_for, Identity, ReadmeState, RegistryKind, Role},
    services::{proxy::Freshness, ProxyService, SbomService},
};

use crate::RegistryModeMap;

use crate::badges::socket_badge_url;
use crate::handlers::back_office::packages::detail::VulnerabilityDto;
use crate::RegistryMap;
use batlehub_core::entities::Action;

/// Which SBOM formats a version actually has here, so the console can say *no
/// SBOM* instead of offering a download that can only 404.
///
/// The formats are per registry (`[registries.sbom].formats`), so a console that
/// assumed both drew a CycloneDX button on an SPDX-only registry and a pair of
/// them on every upstream-only row — three clicks, three spinners, three
/// failures, and the reader left to guess whether the SBOM was missing or the
/// server was.
///
/// A tri-state for the same reason [`ReadmeState`] is one: *we hold none* and
/// *we have not looked* are different answers, and rendering the second as the
/// first is a definite claim with nothing behind it (RFC 0007 §4.2).
#[derive(Clone, Serialize, ToSchema)]
pub struct SbomDto {
    /// `available` — [`Self::formats`] lists what can be downloaded;
    /// `none` — this instance holds no SBOM for the version;
    /// `unknown` — nothing has opened these bytes yet (an upstream-only row),
    /// or the lookup itself failed.
    pub state: String,
    /// The formats actually held, `"spdx"` before `"cyclonedx"`. Empty unless
    /// `state` is `available`.
    pub formats: Vec<String>,
}

impl SbomDto {
    /// The value every row starts at, before [`enrich_page`] answers for the
    /// rows that survive to the page.
    fn unknown() -> Self {
        Self {
            state: "unknown".to_owned(),
            formats: Vec::new(),
        }
    }
}

/// What one version's SBOM availability is, for a row this instance holds bytes
/// of.
///
/// A lookup failure reads as `unknown` rather than as `none`, and rather than
/// propagating: the SBOM is one control on a page whose job is showing versions,
/// and an outage should neither error the page nor let it state that a version
/// has no SBOM when we simply could not ask.
async fn sbom_state_for(
    sbom_svc: &Option<web::Data<Arc<SbomService>>>,
    registry: &str,
    name: &str,
    version: &str,
) -> SbomDto {
    let Some(svc) = sbom_svc.as_ref() else {
        // No SBOM service wired at all: nothing generates them here, so there is
        // definitively nothing to download — a statement, not an unknown.
        return SbomDto {
            state: "none".to_owned(),
            formats: Vec::new(),
        };
    };
    match svc.formats_for_coordinate(registry, name, version).await {
        Ok(formats) if formats.is_empty() => SbomDto {
            state: "none".to_owned(),
            formats: Vec::new(),
        },
        Ok(formats) => SbomDto {
            state: "available".to_owned(),
            formats: formats.iter().map(|f| f.as_str().to_owned()).collect(),
        },
        Err(_) => SbomDto::unknown(),
    }
}

/// The recorded licence for one version, or `None` when it is not known.
///
/// A lookup failure reads as unknown rather than propagating: the licence is
/// one field on a page whose job is showing versions, and an SBOM outage should
/// not turn the package page into an error. `LicenseGateRule` is where a failed
/// lookup carries weight, and it logs its own.
async fn license_for(
    sbom_svc: &Option<web::Data<Arc<SbomService>>>,
    registry: &str,
    name: &str,
    version: &str,
) -> Option<String> {
    let svc = sbom_svc.as_ref()?;
    svc.repo
        .get_license_for_coordinate(registry, name, version)
        .await
        .unwrap_or(None)
}

// ── Package detail ─────────────────────────────────────────────────────────────

#[derive(Deserialize, IntoParams)]
pub struct PackageDetailPath {
    pub registry: String,
    pub name: String,
}

/// Whether the discovery read may run for this request.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamMode {
    /// Ask upstream when this instance holds nothing of the package and the
    /// registry allows it. The default, and what the package page uses.
    #[default]
    Auto,
    /// Answer from local rows only. For callers that want the cheap answer —
    /// the admin panels that only care about held versions — and for anyone who
    /// wants exactly the shape this endpoint had before RFC 0007.
    Skip,
}

/// Whether pre-release versions are in the answer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PrereleaseMode {
    /// Every version, pre-release or not.
    ///
    /// The default because it is what this endpoint has always answered:
    /// dropping rows out of an existing caller's response on an upgrade is not
    /// a compatible change, however sensible the narrower list is for a reader.
    #[default]
    Show,
    /// Releases only — what the console asks for, since a release candidate is
    /// not what a reader is looking for by default. The version named by
    /// `version=` is kept whatever this says.
    Hide,
}

#[derive(Deserialize, IntoParams)]
pub struct PackageDetailQuery {
    #[serde(default)]
    #[param(inline)]
    pub upstream: UpstreamMode,
    /// 0-based, like every other paginated endpoint here.
    ///
    /// Absent is *not* the same as `0`: absent lets `version=` choose the page,
    /// which is how a link to one version opens on the page that holds it.
    /// Past the end is clamped to the last page rather than answered empty.
    #[serde(default)]
    pub page: Option<u64>,
    /// Rows in the answer. Absent means `[limits].versions_per_page`, which is
    /// also the most this may be — a larger ask is clamped down to it, and the
    /// applied value is reported back in `versions_page.per_page`.
    #[serde(default)]
    pub per_page: Option<u64>,
    /// Case-insensitive substring on the version string — "is 4.0.2 in here",
    /// which is the question a reader has in front of a 169-version list.
    ///
    /// It filters the *whole* list, which is the reason it is here rather than
    /// in the console: a filter that only searched the page it was handed would
    /// answer "no" about versions this server knows perfectly well it has.
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    #[param(inline)]
    pub prereleases: PrereleaseMode,
    /// A version the caller is pointing at.
    ///
    /// Two effects, both serving "a link named this version": it survives
    /// `prereleases=hide`, so the console never marks a row it was not given;
    /// and when no `page` is asked for, the page returned is the one holding it.
    /// It does **not** survive `q` — a filter is a question the reader typed,
    /// and answering it with a row that does not match would be a different lie.
    #[serde(default)]
    pub version: Option<String>,
}

/// What the discovery read did, so the page can say which rung answered rather
/// than presenting a degraded answer as a complete one.
#[derive(Serialize, ToSchema)]
pub struct UpstreamReadDto {
    /// `false` for a `local`-mode registry, a kind with no upstream to ask, a
    /// `?upstream=skip` caller, a registry with the read disabled, or a package
    /// published here — a private name is never sent to a public index.
    pub attempted: bool,
    /// `cached` | `fresh` | `stale`, when the read answered.
    pub freshness: Option<String>,
    /// How many upstream-only versions came back.
    pub version_count: usize,
    /// `max_versions` shortened the list. A silently shortened list is a lie
    /// about the registry.
    pub truncated: bool,
    /// Set when the read was attempted and every rung failed. The page says the
    /// upstream could not be reached rather than showing an empty table as an
    /// answer.
    pub error: Option<String>,
}

/// Where the returned rows sit in the whole version list.
///
/// Every count here is over the *whole* list rather than the page, because each
/// one answers a question the console asks out loud — `42 of 44 shown`, `Show 3
/// pre-releases`, how many pages there are — and a count taken from the page
/// would make each of those sentences false as soon as there was more than one.
#[derive(Serialize, ToSchema)]
pub struct VersionPageDto {
    /// 0-based, and the page **actually returned**: the one holding `version=`
    /// when the caller named one and asked for no page, or the last page when
    /// the ask was past the end.
    pub page: u64,
    /// The page size actually applied, after the `[limits].versions_per_page`
    /// ceiling. A caller that asked for more gets this, not what it asked for.
    pub per_page: u64,
    /// Versions matching `q` and the pre-release mode, across every page.
    pub total: u64,
    /// Versions this endpoint knows of, before either filter.
    pub unfiltered_total: u64,
    /// Pre-releases this package has, whatever the mode — the number behind
    /// *Show 3 pre-releases*.
    pub prerelease_total: u64,
    /// How many the pre-release mode is currently removing. Not the same number
    /// as `prerelease_total`: `version=` keeps its own, so a page opened on a
    /// release candidate hides one fewer than it has.
    pub hidden_prereleases: u64,
}

#[derive(Serialize, ToSchema)]
pub struct ExplorePackageDetailResponse {
    pub registry: String,
    pub name: String,
    pub gate: GateDto,
    /// One page of them — see `versions_page`.
    pub versions: Vec<ExploreVersionDto>,
    pub versions_page: VersionPageDto,
    /// The version a reader who has asked for none should be shown: the newest
    /// stable version this instance **holds**, falling back to the newest stable
    /// and then to whatever is held (RFC 0007 §4.2).
    ///
    /// Answered here because it is a fact about the whole list, and the console
    /// is now given one page of it: a client deriving this rule from the rows it
    /// received would pick the newest stable version *on page one*, which on a
    /// package held only at 2.1.0 is an upstream row we serve nothing of.
    /// `None` only for a package with no versions at all.
    pub default_version: Option<String>,
    /// The version asked for by `version=`, echoed back **only if this package
    /// has it**. `None` for a typo or a version yanked since the link was sent,
    /// which is the caller's signal to fall back to `default_version` rather
    /// than mark nothing.
    pub selected_version: Option<String>,

    /// The selected version's row, whatever page it is on.
    ///
    /// `selected_version` names it; this *is* it. The console renders its subject
    /// region from this rather than from whatever happens to be in `versions`,
    /// because the two are routinely different: the page writes `?version=X&page=N`
    /// into the URL itself, and X is on page 1 while N is 3.
    pub selected: Option<ExploreVersionDto>,
    /// `true` when the upstream database was unreachable and this package has no cached data.
    pub upstream_unavailable: bool,
    /// What the discovery read did (RFC 0007 §4.2).
    pub upstream: UpstreamReadDto,
    /// Whether the console may offer **Fetch this version** on an
    /// upstream-only row (RFC 0007-bis §4.4).
    pub fetch: FetchOfferDto,
    /// Where this package's code and site live, as its own metadata names them.
    ///
    /// `None` whenever we cannot say: a registry kind whose client does not read
    /// these fields, a package the metadata cache has never held, or a locally
    /// published one (asking a public index about a private name on a page view
    /// would leak that the software exists — the same suppression the derived
    /// README applies).
    pub links: Option<PackageLinksDto>,
}

/// A package's own links, normalised to something a browser can open.
///
/// Every URL here was written by whoever published the package and lands in an
/// `href` on an authenticated console page, so it has been through
/// `batlehub_core::entities::normalize_url` — an allow-list of `http`/`https`
/// applied *after* the ecosystem-specific rewrites (`git+…`, `.git`,
/// `github:o/r`, scp-like addresses). Anything that did not survive is absent
/// rather than rendered, because a link that cannot be opened is worse than no
/// link.
#[derive(Serialize, ToSchema)]
pub struct PackageLinksDto {
    /// Where the source code lives.
    pub repository: Option<String>,
    /// The package's own site, when it declared one.
    pub homepage: Option<String>,
}

/// Whether the fetch button is offered here, and why not when it is not.
///
/// Answered by the server because both halves are the server's to know: whether
/// the operator turned `console_fetch` off, and whether "fetch this version" has
/// a single meaning for this registry kind. A console that guessed would offer a
/// button that always fails on Maven, which is the "disabled control with no
/// explanation" §4.4 refuses.
#[derive(Serialize, ToSchema)]
pub struct FetchOfferDto {
    pub offered: bool,
    /// The kind's own reason, verbatim, when `offered` is `false` and the reason
    /// is about the registry type rather than about the switch. `null` when the
    /// operator simply turned it off — that is not a fact about the package and
    /// the page says nothing rather than explaining the operator to themselves.
    pub reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct GateDto {
    /// Whether the caller is a beta-channel member for this registry.
    pub beta_member: bool,
}

// `registry_accessible` used to sit above `beta_member` here, reporting whether
// the caller's role could proxy from this registry. It was never enforced — the
// page was served either way — and now that the handler refuses a registry the
// caller may not browse, it cannot be `false`: the explore set is the proxy set
// intersected with the role's explore permission, so passing the gate implies
// proxy access. A flag that is always `true` describes nothing; the console's
// access-check card went with it.

#[derive(Clone, Serialize, ToSchema)]
pub struct ExploreVersionDto {
    pub version: String,
    /// `"proxied"` | `"local"` | `"upstream"`
    ///
    /// `upstream` means this instance holds no bytes for the version and knows
    /// about it only because it asked. Every cell that would be a fact about
    /// what we hold is `null` on such a row rather than `0`.
    pub source: String,
    pub firewall: FirewallDto,
    /// `null` on an upstream-only row — **not** `0`.
    ///
    /// `0` would be a definite answer with nothing behind it: nobody has
    /// downloaded it *through here*, which is not the same as nobody having
    /// downloaded it, and the row exists precisely because this instance has
    /// never held the version (RFC 0007 §4.2).
    pub download_count: Option<u64>,
    pub last_accessed: Option<String>,
    pub published_at: Option<String>,
    pub is_prerelease: bool,
    /// Known vulnerabilities for this version (from the periodic SBOM re-scan).
    pub vulnerabilities: Vec<VulnerabilityDto>,
    /// The licence this version's own manifest declared, verbatim.
    ///
    /// Null means *unknown*, never "unlicensed": it is read out of the archive
    /// when the version is cached or published, so it is absent for anything
    /// fetched before extraction existed and for the registry types with no
    /// manifest parser (RFC 0004-bis §13.1).
    pub license: Option<String>,
    /// socket.dev badge URL when enabled for this registry; else null.
    pub socket_badge_url: Option<String>,
    /// Flagged as deprecated (still downloadable). Local versions only.
    pub deprecated: bool,
    /// Optional deprecation message.
    pub deprecation_message: Option<String>,
    /// Hidden from registry-protocol listings but still downloadable by exact
    /// coordinate. Local versions only.
    pub unlisted: bool,
    /// `available` | `none` | `unknown` — whether this version has a README.
    ///
    /// A tri-state rather than a boolean, because the answer genuinely has three
    /// values and the third is the common one for an archive-borne registry
    /// kind: a version this instance holds no bytes for has a README that cannot
    /// be read yet. `false` rendered for *"we have not looked"* would be a
    /// definite-looking answer with nothing behind it (RFC 0007 §4.2).
    pub readme: ReadmeState,
    /// Which SBOM formats this version has here — the answer the console needs
    /// *before* it draws a download control. See [`SbomDto`].
    pub sbom: SbomDto,
    /// Whether this version has ever been scanned for vulnerabilities.
    ///
    /// `vulnerabilities: []` means *scanned and clear* only when this is `true`.
    /// On a version nothing has ever opened it means *never scanned*, and the
    /// two must not render identically — a green row on a package this instance
    /// has never held is a claim we cannot support.
    pub vulnerabilities_scanned: bool,

    /// `cached` | `pending` | `yanked` | `blocked` — the row's state in
    /// DESIGN.md's resolution vocabulary, graded here rather than in the
    /// console.
    ///
    /// The console used to derive this from `firewall` alone, over three values,
    /// and the mapping said *held and verified* — a full 3×3 matrix in ink — for
    /// a version this instance holds **no bytes of**: an upstream-only candidate
    /// carries `FirewallDto::Clear`, because the firewall has nothing to refuse
    /// about a version nobody can download from here yet. The signature device of
    /// the whole design system was therefore lying on precisely the rows the
    /// fetch button exists for. Grading it on the side that knows what is held
    /// is the same correction `ExploreEntry::state` already applies to the
    /// catalogue's rows.
    ///
    /// Two of the six states are deliberately absent. `stale` needs the
    /// artifact's own fetch time against the registry's TTL, and `held` needs the
    /// release-age gate evaluated for this viewer — neither is per-version data
    /// this endpoint holds, and inventing either would be the same class of
    /// claim this field exists to stop.
    pub state: String,
}

#[derive(Clone, Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum FirewallDto {
    Clear,
    Blocked {
        reason: String,
        blocked_by: String,
        blocked_at: String,
    },
    Yanked,
}

/// Package detail view: all known versions with gate and firewall status.
#[utoipa::path(
    get,
    path = "/api/v1/explore/packages/{registry}/{name}",
    tag = "explore",
    params(PackageDetailPath, PackageDetailQuery),
    responses(
        (status = 200, description = "Package detail", body = ExplorePackageDetailResponse),
        (status = 404, description = "The registry is not browsable by this caller, or the \
                                     package is not visible to them"),
    ),
    security(("bearer_token" = [])),
)]
// Eight extractors, one per thing the page needs to answer for: the coordinate,
// the caller, the catalogue, the local backend, the registry's type, the access
// policy, the recorded licences and the README store. Actix injects each by
// type, so collapsing them into a bag would hide what the handler reads.
#[allow(clippy::too_many_arguments)]
#[get("/api/v1/explore/packages/{registry}/{name}")]
pub async fn explore_package_detail(
    path: web::Path<PackageDetailPath>,
    query: web::Query<PackageDetailQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    registry_map: web::Data<RegistryMap>,
    sbom_svc: Option<web::Data<Arc<SbomService>>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    mode_map: web::Data<RegistryModeMap>,
) -> Result<impl Responder, AppError> {
    let registry = &path.registry;
    let name = &path.name;

    // Two settings out of one read: the socket.dev badge flag (per registry, by
    // feature flag) and the page size (global). Both are copied out of the guard
    // before the first `await` below, the rule every handler here follows.
    let (socket_badge_enabled, versions_per_page, upstream_detail_enabled) = {
        let hot = local_svc.hot.read().await;
        (
            hot.feature_flags
                .get(registry)
                .is_none_or(|f| f.socket_badge),
            hot.versions_per_page,
            // Absent means on, the same reading `build_upstream_detail_map`
            // gives an absent `[registries.upstream_detail]` block.
            hot.upstream_detail.get(registry).is_none_or(|d| d.enabled),
        )
    };
    let registry_type = registry_map.type_of(registry);
    let badge_for = |version: &str| -> Option<String> {
        if !socket_badge_enabled {
            return None;
        }
        registry_type
            .as_deref()
            .and_then(|t| socket_badge_url(t, name, version))
    };

    enforce_detail_gates(&hot, &local_svc, registry, name, &identity).await?;

    // Gate: beta channel membership
    let beta_member = beta_member_for(&local_svc, registry, &identity).await;

    // Proxied versions from package_statuses
    let proxied_filter = PackageFilter {
        registry: Some(registry.clone()),
        registries: vec![],
        name_exact: Some(name.clone()),
        name_contains: None,
        blocked_only: false,
        limit: 500,
        offset: 0,
    };
    let (proxied_summaries, upstream_unavailable) =
        match admin_svc.list_packages(proxied_filter).await {
            Ok(summaries) => (summaries, false),
            Err(_) => (vec![], true),
        };

    // Local versions from local_packages.
    //
    // The read is kept as a `Result` because two different things are drawn from
    // it and they must fail in opposite directions — see `published_locally`
    // below.
    let local_versions_read = local_svc.backend.get_versions(registry, name).await;
    if let Err(ref e) = local_versions_read {
        tracing::warn!(
            registry = %registry, package = %name, error = %e,
            "explore detail: local version read failed; treating the package as hosted here"
        );
    }
    let local_versions = local_versions_read.as_deref().unwrap_or_default().to_vec();

    // Which versions have a stored README, in **one** query rather than a probe
    // per row: the table can be hundreds of versions long and a lookup each
    // would be that many round trips for one page load.
    let readme_versions: std::collections::HashSet<String> = match proxy_svc.readme.as_ref() {
        Some(svc) => svc
            .repo
            .list_versions_with_readme(registry, name)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect(),
        None => Default::default(),
    };
    let kind = registry_type
        .as_deref()
        .and_then(|t| t.parse::<RegistryKind>().ok());
    // A version we hold and that has no stored README is *unknown* rather than
    // *none* when the kind reads its README out of the archive: the text exists,
    // it just has not been read yet — for a `from_archive = false` registry, or
    // for a version cached before this feature existed.
    let absent_state = absent_readme_state_for(kind);
    let readme_state = |version: &str| -> ReadmeState {
        if readme_versions.contains(version) {
            ReadmeState::Available
        } else {
            absent_state
        }
    };

    // Whether this package is *published here*, which is what suppresses the
    // discovery read — not whether we happen to hold some of its versions.
    // Holding three versions out of forty is exactly the case where the missing
    // rows are worth showing; the suppression is about provenance (§4.2).
    //
    // **A failed read counts as published.** `unwrap_or_default()` read a pool
    // timeout or a Postgres restart as "not published here", which lifts the
    // suppression — so one transient backend error was enough to send an
    // internal package's name to the public upstream index on the next page
    // view, which is precisely the disclosure §4.4/§7.7 exist to prevent. The
    // cost of failing the other way is a page that shows fewer upstream rows
    // while the database is down, and that is the right trade: we cannot tell
    // whether the package is private, so we must not act as though it is not.
    let published_locally = local_versions_read.map_or(true, |rows| !rows.is_empty());

    let blocked_versions = blocked_version_map(&admin_svc, registry, name).await;

    // Build version entries.
    //
    // ── What is deliberately *not* built here ──────────────────────────────
    //
    // Three fields — `vulnerabilities`, `license`, `socket_badge_url` — are left
    // at their empty value and filled in by `enrich_page` after the list has
    // been filtered and sliced. They used to be built in these loops, which cost
    // one vulnerability read and one SBOM read *per version of the package*:
    // 169 of each for `@babel/plugin-transform-runtime`, to serve a page showing
    // 25 rows. Paginating the answer while still enriching every row would have
    // moved the bytes and left the cost, which is most of the point.
    //
    // Nothing above the slice may depend on them: the sort is by pre-release and
    // version string, and the default selection is by source. Both hold.
    let mut versions = held_version_rows(proxied_summaries, local_versions, &readme_state);

    // ── The discovery read ────────────────────────────────────────────────
    //
    // Everything above is exactly the code this endpoint had before RFC 0007,
    // and the merge below only *adds* rows — so a bug in the new path cannot
    // change what the page says about a version this instance holds (§6.4).
    let suppress_upstream = published_locally;
    let upstream = discovery_read(
        &proxy_svc,
        &mode_map,
        registry,
        name,
        &identity,
        query.upstream,
        // A package the local backend hosts is never asked about upstream, on
        // any mode: on a hybrid registry a private package shares a namespace
        // with a public index, and sending its name there on every page view
        // would leak the existence of internal software to a third party — the
        // same disclosure `sumdb_url = ""` exists for. It would also invite a
        // dependency-confusion answer, where the page shows upstream's versions
        // of a name that means something else here (§4.4, §7.7).
        //
        // A caller who may not read this registry at all is in the same
        // position, and *this path cannot check it for itself*:
        // `upstream_detail` goes through `request_prelude`, which validates the
        // coordinate and snapshots the hot config and does **not** evaluate the
        // registry's rules — `RbacRule` is applied in `handle`/
        // `resolve_metadata_for`, neither of which is on this path. So the
        // `action: "releases:read"` and the identity it threads through
        // are never consulted, and an anonymous `GET` against a registry with
        // `rbac.anonymous = []` made this server fetch upstream on their behalf
        // and hand back the full version list, publish dates and deprecation
        // strings. What stops that is the gate at the top of this handler: an
        // unreachable registry is a `404` long before here, and the explore set
        // it checks is a subset of the proxy set.
        suppress_upstream,
    )
    .await;

    merge_upstream_rows(
        &mut versions,
        upstream.detail.as_ref(),
        &blocked_versions,
        absent_state,
    );

    // Sort: stable versions first, then pre-release; within each group newest
    // first.
    //
    // The comparator said `b.is_prerelease.cmp(&a.is_prerelease)`, which orders
    // `false` after `true` and put every pre-release *above* every release —
    // the opposite of the line above it, which has described the intent since
    // this endpoint was written. It went unnoticed while the console received
    // the whole list and sorted its own view of it; it stops being invisible
    // the moment the answer is a page, because the order is then what decides
    // which versions page one *is*. On a package like `chalk`, whose betas
    // outnumber its releases, page one was entirely release candidates.
    //
    // The second key is a *version* comparison, not a string one, for the same
    // reason the first was wrong: `"5.6.2" > "5.10.0"` lexicographically, so
    // `chalk` led with 5.6.2 and `default_selection` below named it as the
    // version the console should open on. That was invisible while the console
    // received the whole list and sorted it; it is the answer now.
    versions.sort_by(|a, b| {
        a.is_prerelease
            .cmp(&b.is_prerelease)
            .then_with(|| super::newest_first(&a.version, &b.version))
    });

    let VersionPage {
        mut versions,
        page,
        per_page,
        total,
        unfiltered_total,
        prerelease_total,
        hidden_prereleases,
        default_version,
        selected_version,
        subject_version,
        subject_row,
    } = paginate_versions(versions, &query, versions_per_page);

    enrich_page(
        &mut versions,
        &admin_svc,
        &sbom_svc,
        registry,
        name,
        &badge_for,
    )
    .await;

    // The subject's row goes through the same enrichment as the page. When it is
    // on the page there is nothing to do but take the enriched copy; when it is
    // not, it is enriched alone — one extra vulnerability read and one licence
    // read, for the one row the reader is actually looking at.
    let selected = match subject_row {
        Some(row) => match versions.iter().find(|r| r.version == row.version) {
            Some(on_page) => Some(on_page.clone()),
            None => {
                let mut one = vec![row];
                enrich_page(&mut one, &admin_svc, &sbom_svc, registry, name, &badge_for).await;
                Some(one.remove(0))
            }
        },
        None => None,
    };

    // The row the reader is actually looking at, which is what its links should
    // describe: npm carries `repository` per version, and a package that changed
    // forge between releases named the old one in the old version.
    let selected_version_for_links = subject_version;

    Ok(web::Json(ExplorePackageDetailResponse {
        registry: registry.clone(),
        name: name.clone(),
        gate: GateDto { beta_member },
        versions,
        versions_page: VersionPageDto {
            page,
            per_page,
            total,
            unfiltered_total,
            prerelease_total,
            hidden_prereleases,
        },
        default_version,
        selected_version,
        selected,
        upstream_unavailable,
        upstream: upstream.dto,
        fetch: fetch_offer(&local_svc, &registry_map, registry.as_str(), &identity.0).await,
        links: package_links(LinkInput {
            proxy_svc: &proxy_svc,
            local_svc: &local_svc,
            mode_map: &mode_map,
            registry,
            name,
            version: selected_version_for_links.as_deref(),
            kind,
            // The document the version table above was built from. Reading it
            // again costs nothing and is the difference between a link that is
            // there and one that depends on what else has been requested.
            listing: upstream.detail.as_ref().and_then(|d| d.links.as_ref()),
            identity: &identity.0,
            // The two switches the discovery read honours, honoured here too.
            //
            // `package_links` makes its own upstream request when the listing
            // cannot carry the answer, and it was consulting neither: so
            // `?upstream=skip` — documented as "answer from local rows only …
            // the cheap answer" — and an operator who set
            // `[registries.upstream_detail] enabled = false` both still paid one
            // outbound request per page view on every PyPI, cargo, NuGet, Maven,
            // RubyGems, Terraform and gallery registry. A switch that half the
            // page ignores is not a switch.
            may_ask_upstream: query.upstream != UpstreamMode::Skip && upstream_detail_enabled,
        })
        .await,
    }))
}

/// Every blocked version of the package, by version string.
///
/// Read once for the whole merge, so an upstream-only row shows `Blocked` with
/// its reason rather than as installable.
async fn blocked_version_map(
    admin_svc: &AdminService,
    registry: &str,
    name: &str,
) -> std::collections::HashMap<String, (String, String, String)> {
    admin_svc
        .list_packages(PackageFilter {
            registry: Some(registry.to_owned()),
            registries: vec![],
            name_exact: Some(name.to_owned()),
            name_contains: None,
            blocked_only: true,
            limit: 500,
            offset: 0,
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|summary| match summary.status {
            PackageStatus::Blocked {
                reason,
                blocked_by,
                blocked_at,
            } => Some((
                summary.package_id.version,
                (reason, blocked_by, blocked_at.to_rfc3339()),
            )),
            PackageStatus::Available => None,
        })
        .collect()
}

/// The rows for versions this instance holds bytes of — proxied, then local.
///
/// ── What is deliberately *not* built here ─────────────────────────────────
///
/// Three fields — `vulnerabilities`, `license`, `socket_badge_url` — are left at
/// their empty value and filled in by [`enrich_page`] after the list has been
/// filtered and sliced. They used to be built in these loops, which cost one
/// vulnerability read and one SBOM read *per version of the package*: 169 of
/// each for `@babel/plugin-transform-runtime`, to serve a page showing 25 rows.
/// Paginating the answer while still enriching every row would have moved the
/// bytes and left the cost, which is most of the point.
///
/// Nothing above the slice may depend on them: the sort is by pre-release and
/// version string, and the default selection is by source. Both hold.
fn held_version_rows(
    proxied_summaries: Vec<batlehub_core::entities::PackageSummary>,
    local_versions: Vec<batlehub_core::entities::PublishedPackage>,
    readme_state: &impl Fn(&str) -> ReadmeState,
) -> Vec<ExploreVersionDto> {
    let mut versions: Vec<ExploreVersionDto> = Vec::new();

    // Track which versions came from local to avoid duplicating proxied entries.
    let local_version_set: std::collections::HashSet<&str> =
        local_versions.iter().map(|v| v.version.as_str()).collect();

    for summary in &proxied_summaries {
        // Skip versions also present in local (they'll appear as "local").
        if local_version_set.contains(summary.package_id.version.as_str()) {
            continue;
        }
        let firewall = match &summary.status {
            PackageStatus::Available => FirewallDto::Clear,
            PackageStatus::Blocked {
                reason,
                blocked_by,
                blocked_at,
            } => FirewallDto::Blocked {
                reason: reason.clone(),
                blocked_by: blocked_by.clone(),
                blocked_at: blocked_at.to_rfc3339(),
            },
        };
        versions.push(ExploreVersionDto {
            readme: readme_state(&summary.package_id.version),
            // Every version in this list is one this instance has pulled
            // through, so the scanner has had the bytes and the empty list means
            // "clear" rather than "never looked".
            vulnerabilities_scanned: true,
            // Graded in `enrich_page`, the one funnel every rendered row passes
            // through — see `version_state`.
            state: String::new(),
            // RFC 0015 §4.5's one definition, not `contains('-')`. The crude
            // rule was defended by this table's own consistency — two lists in
            // one table must grade a row the same way — and one shared function
            // serves that better than two matching approximations do. It is a
            // visible change: `1.0-SNAPSHOT` is now labelled a pre-release,
            // correctly, and `2.0.0+build-1` stops being one.
            is_prerelease: batlehub_core::services::version_order::is_prerelease(
                &summary.package_id.version,
            ),
            version: summary.package_id.version.clone(),
            source: "proxied".to_string(),
            firewall,
            download_count: Some(summary.access_count),
            last_accessed: summary.last_accessed.map(format_dt),
            published_at: None,
            // Filled by `enrich_page`, for the rows that survive to the answer.
            vulnerabilities: Vec::new(),
            license: None,
            sbom: SbomDto::unknown(),
            socket_badge_url: None,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
        });
    }

    for pkg in local_versions {
        let firewall = if pkg.yanked {
            FirewallDto::Yanked
        } else {
            FirewallDto::Clear
        };
        versions.push(ExploreVersionDto {
            readme: readme_state(&pkg.version),
            vulnerabilities_scanned: true,
            state: String::new(),
            // The same definition as the proxied rows above, which is the
            // property this table needed all along.
            is_prerelease: batlehub_core::services::version_order::is_prerelease(&pkg.version),
            version: pkg.version,
            source: "local".to_string(),
            firewall,
            download_count: Some(0),
            last_accessed: None,
            published_at: Some(pkg.published_at.to_rfc3339()),
            vulnerabilities: Vec::new(),
            license: None,
            sbom: SbomDto::unknown(),
            socket_badge_url: None,
            deprecated: pkg.deprecated,
            deprecation_message: pkg.deprecation_message,
            unlisted: pkg.unlisted,
        });
    }

    versions
}

/// Add the versions only upstream knows about.
///
/// Local rows win every collision: a version we hold is described by what we
/// know about it, and the upstream document cannot overwrite any of it. The
/// merge only *adds* rows the local sources did not have — the same precedence
/// `SearchMode::Hybrid` already uses, and for the same reason. That is also what
/// makes this path safe by construction: a bug here cannot change what the page
/// says about a version this instance holds (RFC 0007 §6.4).
fn merge_upstream_rows(
    versions: &mut Vec<ExploreVersionDto>,
    detail: Option<&batlehub_core::services::UpstreamDetail>,
    blocked_versions: &std::collections::HashMap<String, (String, String, String)>,
    absent_state: ReadmeState,
) {
    let Some(detail) = detail else {
        return;
    };
    let held: std::collections::HashSet<&str> =
        versions.iter().map(|v| v.version.as_str()).collect();
    let extra: Vec<ExploreVersionDto> = detail
        .versions
        .iter()
        .filter(|candidate| !held.contains(candidate.version.as_str()))
        .map(|candidate| {
            // Rules are evaluated for *display*, never for permission: the
            // blocked set is consulted so an administrator's block shows as
            // `Blocked` with its reason rather than as installable. The gate
            // that matters still runs on the download.
            let firewall = match blocked_versions.get(candidate.version.as_str()) {
                Some((reason, by, at)) => FirewallDto::Blocked {
                    reason: reason.clone(),
                    blocked_by: by.clone(),
                    blocked_at: at.clone(),
                },
                None if candidate.yanked => FirewallDto::Yanked,
                None => FirewallDto::Clear,
            };
            ExploreVersionDto {
                readme: if detail.readmes.contains_key(&candidate.version) {
                    ReadmeState::Available
                } else {
                    absent_state
                },
                // Nothing has ever opened these bytes, so an empty vulnerability
                // list here means *never scanned* — and a green row on a package
                // this instance has never held is a claim we cannot support.
                vulnerabilities_scanned: false,
                state: String::new(),
                version: candidate.version.clone(),
                source: "upstream".to_owned(),
                firewall,
                download_count: None,
                last_accessed: None,
                published_at: candidate.published_at.map(|t| t.to_rfc3339()),
                is_prerelease: candidate.is_prerelease,
                vulnerabilities: Vec::new(),
                license: None,
                // Left `unknown`: `enrich_page` skips upstream-only rows, and it
                // must — nothing has ever opened these bytes, so there is no
                // SBOM to find and *none* would read as a fact about a version
                // this instance has never held.
                sbom: SbomDto::unknown(),
                socket_badge_url: None,
                deprecated: candidate.deprecated.is_some(),
                deprecation_message: candidate.deprecated.clone(),
                unlisted: false,
            }
        })
        .collect();
    versions.extend(extra);
}

/// One page of versions, and every count taken over the whole list before the
/// slice.
struct VersionPage {
    versions: Vec<ExploreVersionDto>,
    page: u64,
    per_page: u64,
    total: u64,
    unfiltered_total: u64,
    prerelease_total: u64,
    hidden_prereleases: u64,
    default_version: Option<String>,
    selected_version: Option<String>,
    /// The version the subject region is about — the one asked for, else the
    /// default. Also what the package's links are resolved for.
    subject_version: Option<String>,
    subject_row: Option<ExploreVersionDto>,
}

/// Filter the sorted version list, then slice it.
///
/// Every count is taken against the whole list before anything is sliced: each
/// number the console says out loud — `42 of 44 shown`, `Show 3 pre-releases` —
/// is a statement about the *package*, and a count taken after the slice would
/// make each of them a statement about page one wearing the package's name.
fn paginate_versions(
    mut versions: Vec<ExploreVersionDto>,
    query: &PackageDetailQuery,
    versions_per_page: u64,
) -> VersionPage {
    let unfiltered_total = versions.len() as u64;
    let prerelease_total = versions.iter().filter(|v| v.is_prerelease).count() as u64;
    let default_version = default_selection(&versions);
    // Whether the version asked for is one this package actually has — a typo,
    // or one yanked since the link was sent, is the case this answers. The
    // caller cannot work it out from a page the version is not on, and a console
    // that guessed would either mark no row at all or claim a version we do not
    // list. Judged against the unfiltered list: a version excluded by the
    // caller's own `q` still exists.
    let selected_version = query
        .version
        .as_deref()
        .filter(|asked| versions.iter().any(|v| v.version == *asked))
        .map(str::to_owned);

    // ── The row the subject renders ───────────────────────────────────────────
    //
    // Captured here, from the whole sorted list, *before* the pre-release filter,
    // `q` and the page slice get to it — because the console's subject region is
    // about one version and none of those three is.
    //
    // Without it, a link the page produces itself — it writes both `version` and
    // `page` into the URL — could name a version that is not on the page it also
    // names, and the console had nothing to render the subject from: state,
    // refusal reason, advisories, install line and fetch button all disappeared
    // while the README below still rendered that version's text, so the page
    // looked loaded. Sending the row costs one clone and removes the whole class.
    let subject_version = selected_version.clone().or_else(|| default_version.clone());
    let subject_row = subject_version
        .as_deref()
        .and_then(|asked| versions.iter().find(|c| c.version == asked).cloned());

    // The version the caller is pointing at survives the pre-release filter:
    // a link to `?version=5.0.0-beta.2` must not answer with a list its own
    // subject is missing from. It does not survive `q` — see the field's doc.
    //
    // `default_version` survives it too, and for the same reason one step
    // earlier: on a package that has never cut a stable release — an 0.x
    // library, a pre-release-only plugin — the row the caller is *about* to be
    // pointed at is itself a pre-release, and hiding it answers a package that
    // has forty versions with an empty table.
    let pinned = query.version.as_deref();
    if query.prereleases == PrereleaseMode::Hide {
        versions.retain(|v| {
            !v.is_prerelease
                || Some(v.version.as_str()) == pinned
                || Some(&v.version) == default_version.as_ref()
        });
    }
    let hidden_prereleases = unfiltered_total - versions.len() as u64;

    if let Some(needle) = query
        .q
        .as_deref()
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty())
    {
        versions.retain(|v| v.version.to_lowercase().contains(&needle));
    }
    let total = versions.len() as u64;

    // `unwrap_or` then `min`, not `clamp` against the config value alone: the
    // ceiling and the default are the same key, so asking for more than the
    // operator allows quietly gets the operator's number rather than an error.
    // Zero is a caller mistake and reads as "one row" rather than as an empty
    // answer with no way to tell it from a package with no versions.
    let per_page = query
        .per_page
        .unwrap_or(versions_per_page)
        .clamp(1, versions_per_page);
    let last_page = total.div_ceil(per_page).saturating_sub(1);
    let page = match query.page {
        Some(asked) => asked,
        // No page asked for: open on the one holding the version named, which is
        // what makes a link to a version sixty rows down land on it.
        None => pinned
            .and_then(|v| versions.iter().position(|c| c.version == v))
            .map(|index| index as u64 / per_page)
            .unwrap_or(0),
    }
    .min(last_page);

    let start = (page * per_page) as usize;
    VersionPage {
        versions: versions
            .into_iter()
            .skip(start)
            .take(per_page as usize)
            .collect(),
        page,
        per_page,
        total,
        unfiltered_total,
        prerelease_total,
        hidden_prereleases,
        default_version,
        selected_version,
        subject_version,
        subject_row,
    }
}

/// The two refusals that decide whether this package is answerable at all.
///
/// **The catalogue's own registry list**, the same set `list.rs` filters its
/// rows by, `stats.rs` counts over and `resolve_readme` refuses on. This
/// endpoint used to only *report* the answer, in `gate.registry_accessible`, and
/// serve the page either way — so a registry an operator took out of
/// `rbac.explore` had no rows in the listing, no counts in the stats and a full
/// detail page for anyone who typed the URL. The README and its images are
/// refused; refusing them while this still answered left the same facts
/// reachable one endpoint over, which is not a gate but a speed bump.
///
/// Note it is *narrower* than the proxy access `gate.registry_accessible` used
/// to report: the set is proxy access intersected with the role's explore
/// permission. Anyone who gets past here can necessarily proxy from the registry
/// too, which is why the gate card has nothing left to say and the field is gone
/// from the response.
///
/// That intersection is no longer computed by `AccessConfig`. RFC 0015 §10 rule
/// 2 translated it into the `catalogue:browse` grant —
/// `(has_role || has_group) && rbac.explore.<role>` — and
/// [`browsable_registries`](batlehub_core::services::authz::browsable_registries)
/// resolves it, which is what this endpoint and its three siblings call.
/// `AccessConfig::explore_accessible_registries_for` computed the same set
/// before that translation and has had no caller since.
///
/// **Per-package visibility.** The listing filters `internal`/`team` packages
/// out entirely, so the detail view has to agree — otherwise the name is hidden
/// from the index while remaining readable to anyone who guesses or is told the
/// URL, and the filter buys nothing.
///
/// Both answer `404`, never `403`: a 403 confirms the package exists, which is
/// the fact a non-public package is trying not to disclose. Denied and absent
/// look identical from outside.
async fn enforce_detail_gates(
    // The lock, passed rather than taken from `local_svc.hot`.
    //
    // They are not always the same lock: `cli/tests/integration.rs` builds one
    // for `LocalRegistryService` and another for `ProxyService`, and reading the
    // service's own copy resolved `catalogue:browse` against an empty grant map.
    // §13.4 records the general form — *"two holders of the same ports is how the
    // two answers drift apart"* — and an authorization check is the worst place
    // to discover which copy you got.
    hot: &batlehub_core::services::hot_config::HotConfigLock,
    local_svc: &LocalRegistryService,
    registry: &str,
    name: &str,
    identity: &AuthIdentity,
) -> Result<(), AppError> {
    let not_found = || {
        AppError::not_found(format!(
            "package '{name}' not found in registry '{registry}'"
        ))
    };

    if !batlehub_core::services::authz::browsable_registries(hot, identity)
        .await
        .contains(registry)
    {
        tracing::debug!(
            registry = %registry, package = %name,
            "explore detail: registry not browsable by this caller"
        );
        return Err(not_found());
    }

    if let Err(e) = local_svc.check_visibility(registry, name, identity).await {
        tracing::debug!(
            registry = %registry, package = %name, error = %e,
            "explore detail: hidden by package visibility"
        );
        return Err(not_found());
    }

    Ok(())
}

/// Whether this caller is in the registry's beta channel, for the gate card.
///
/// A registry with no channel configured has no members, and a membership
/// lookup that fails is not one: neither is an error the page reports, because
/// the channel decides what a *download* may do and this is a badge.
async fn beta_member_for(
    local_svc: &LocalRegistryService,
    registry: &str,
    identity: &AuthIdentity,
) -> bool {
    let beta_port = local_svc
        .hot
        .read()
        .await
        .beta_channel
        .get(registry)
        .cloned();
    let Some(port) = beta_port else {
        return false;
    };
    port.is_member(registry, identity).await.unwrap_or(false)
}

/// Everything [`package_links`] consults, in one place — the `ResolveInput`
/// shape the README path already uses, for the same reason: this answer is drawn
/// from four sources and threading them as positional arguments makes the call
/// site unreadable.
struct LinkInput<'a> {
    proxy_svc: &'a ProxyService,
    local_svc: &'a LocalRegistryService,
    mode_map: &'a RegistryModeMap,
    registry: &'a str,
    name: &'a str,
    /// The row the reader is looking at. `None` on a package with no versions at
    /// all, where there is no coordinate to ask about.
    version: Option<&'a str>,
    /// Decides whether the listing's silence is an answer or an absence — see
    /// `listing_carries_links`.
    kind: Option<RegistryKind>,
    /// What the discovery read's own document said, already paid for.
    listing: Option<&'a batlehub_core::entities::MetadataLinks>,
    /// The reader, because the resolve below is rule-evaluated like any other.
    identity: &'a Identity,
    /// Whether this request is allowed to cause egress at all — `?upstream=` and
    /// the registry's `[registries.upstream_detail] enabled`, as the discovery
    /// read reads them. `false` confines this to the cache and the listing.
    may_ask_upstream: bool,
}

/// The package's own links: the cache, then the listing, then — only where
/// neither can answer — **one** resolve for the version selected.
///
/// The order is cheapest-and-most-precise first. The selected version's entry in
/// the metadata cache is the exact answer when something has already resolved
/// it. The discovery read's listing document is free, because the page has
/// already read it to build the version table, and for npm and Composer it is
/// complete. Only when [`listing_carries_links`] says the listing cannot know is
/// a request made, and then for one coordinate.
///
/// **Why a request is made here at all.** The rule this endpoint is held to is
/// not "no upstream request on a page view" — the discovery read above is one.
/// It is the rule `explore_upstream_detail.rs` states in its own words:
/// *filling it for every row would be N upstream requests per page view*. The
/// forbidden thing is the N. One resolve for the single row the reader has
/// selected is the same O(1) shape as the listing read already on this path, and
/// it is cached afterwards for the registry's `metadata_ttl`.
///
/// For PyPI, OpenVSX and the galleries it is not even a new request: the README
/// panel resolves that exact coordinate for the same page. It was simply landing
/// *after* this handler had answered, which is what made the link appear on the
/// next reload and vanish when the entry expired.
///
/// Every failure is `None`: a kind whose client writes no `links`, a listing
/// that carries none, a resolve the registry's rules deny, an upstream that is
/// down. A missing link is the correct rendering of "we do not know", and no
/// part of this page should fail because a package did not declare a repository.
async fn package_links(input: LinkInput<'_>) -> Option<PackageLinksDto> {
    let LinkInput {
        proxy_svc,
        local_svc,
        mode_map,
        registry,
        name,
        version,
        kind,
        listing,
        identity,
        may_ask_upstream,
    } = input;

    // A local-only registry has no upstream to ask, and a locally published
    // package must not be named to a public index on a page view — that would
    // leak the existence of internal software to a third party (§4.4, §7.7).
    if mode_map.get(registry) == RegistryMode::Local {
        return None;
    }
    // The same suppression, failing the same way: a read that errored is treated
    // as "this package is hosted here", so a transient backend fault cannot turn
    // a private package's link resolution into an upstream request naming it.
    match local_svc.backend.get_versions(registry, name).await {
        Ok(rows) if rows.is_empty() => {}
        Ok(_) => return None,
        Err(e) => {
            tracing::warn!(
                registry = %registry, package = %name, error = %e,
                "explore links: local version read failed; not asking upstream"
            );
            return None;
        }
    }

    // What a legitimate resolve already put there — a package manager's request,
    // the README panel, an earlier view of this page.
    let package_id =
        version.map(|version| batlehub_core::entities::PackageId::new(registry, name, version));
    let cached = match &package_id {
        Some(package_id) => proxy_svc
            .cached_metadata_for(package_id)
            .await
            .as_ref()
            .and_then(|metadata| {
                batlehub_core::entities::MetadataLinks::from_extra(&metadata.extra)
            }),
        None => None,
    };

    let links = match cached.or_else(|| listing.cloned()) {
        Some(links) => Some(links),
        // The one request, and only where the listing is known not to hold the
        // answer. `resolve_metadata_for` is cache-first, so this is a *miss* on
        // the same key `cached_metadata_for` just read — the double read costs a
        // second cache lookup and buys the guarantee that nothing here fetches
        // what the page already has.
        None => match (package_id, kind) {
            (Some(package_id), Some(kind))
                if may_ask_upstream
                    && !batlehub_core::services::upstream_detail::listing_carries_links(kind) =>
            {
                let req = batlehub_core::services::proxy::ProxyRequest {
                    package_id,
                    identity: identity.clone(),
                    action: Action::ReleasesRead.to_owned(),
                    // Left `None` deliberately, unlike every other
                    // `ProxyRequest`: this read is `_uncaptured_` (below) and
                    // writes no audit row, so there is nothing for a caller
                    // address to enrich. A page view is not a download.
                    ip_address: None,
                    user_agent: None,
                };
                // Denied by a rule, unreachable, unparseable: all the same
                // answer. This is one field on a page about versions.
                //
                // `_uncaptured_`: a page view must write nothing.
                // `resolve_metadata_for` records whatever README the document
                // carried, without checking that this instance holds the
                // version — so opening the page for an upstream-only version of
                // a kind whose listing carries no links left a `package_readmes`
                // row nothing ever deletes, and told the next load that version
                // has a stored README.
                proxy_svc
                    .resolve_metadata_uncaptured_for(&req)
                    .await
                    .ok()
                    .as_ref()
                    .and_then(|metadata| {
                        batlehub_core::entities::MetadataLinks::from_extra(&metadata.extra)
                    })
            }
            _ => None,
        },
    }?;

    // The listing's links have been through `MetadataLinks::new` in the reader,
    // and every other route through `from_extra`, which re-normalises on the way
    // out. All three arrive here already allow-listed to `http`/`https`.
    Some(PackageLinksDto {
        repository: links.repository,
        homepage: links.homepage,
    })
}

/// The version a reader who has asked for none should be shown: **the newest
/// stable version this instance holds** — stable first, held second, and the
/// pre-releases only when a package has nothing else (RFC 0007 §4.2).
///
/// It used to be the console's rule, over the whole list. The console is now
/// given one page of that list, and the rule does not survive the move: "the
/// first held stable row" read off page one of a package held only at 2.1.0
/// picks an upstream row, which is the exact defect §4.2 exists to have fixed.
///
/// The list arrives sorted stable-before-pre-release and newest-first within
/// each group, so *the first held row is the newest held one* — this reads the
/// order already established rather than inventing a second one.
fn default_selection(versions: &[ExploreVersionDto]) -> Option<String> {
    let held = |v: &&ExploreVersionDto| v.source != "upstream";
    let stable = || versions.iter().filter(|v| !v.is_prerelease);

    stable()
        .find(held)
        .or_else(|| stable().next())
        // Nothing stable at all — an 0.x-only package, or one that has never cut
        // a release. Something has to be selected, and preferring what we hold
        // is the right rule for that case too.
        .or_else(|| versions.iter().find(held))
        .or_else(|| versions.first())
        .map(|v| v.version.clone())
}

/// Fill in the per-row fields that cost a query each, for the rows that
/// actually made it into the answer.
///
/// Upstream-only rows are skipped for the ones that read the SBOM store, and not
/// as an optimisation: nothing has ever opened those bytes, so an empty
/// vulnerability list there would be a claim we cannot support
/// (`vulnerabilities_scanned` says so), and there is no SBOM to read a licence
/// out of — or to offer as a download, which is why `sbom` stays `unknown`.
async fn enrich_page(
    versions: &mut [ExploreVersionDto],
    admin_svc: &AdminService,
    sbom_svc: &Option<web::Data<Arc<SbomService>>>,
    registry: &str,
    name: &str,
    badge_for: &impl Fn(&str) -> Option<String>,
) {
    for row in versions.iter_mut() {
        row.socket_badge_url = badge_for(&row.version);
        // Graded here because this is the one funnel every row the console draws
        // passes through — the page, and the selected row when it is on another
        // one. It reads only fields the row already carries, so it costs nothing.
        row.state = version_state(row).to_owned();
        if row.source == "upstream" {
            continue;
        }
        row.vulnerabilities = admin_svc
            .list_vulnerabilities(registry, name, &row.version)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(VulnerabilityDto::from)
            .collect();
        row.license = license_for(sbom_svc, registry, name, &row.version).await;
        row.sbom = sbom_state_for(sbom_svc, registry, name, &row.version).await;
    }
}

/// One version's state, in the vocabulary the catalogue's rows already use.
///
/// Order matters and is the same as [`batlehub_core::entities::resolve_state`]'s:
/// a refusal outranks a withdrawal, and both outrank whether we hold the bytes.
fn version_state(row: &ExploreVersionDto) -> &'static str {
    match row.firewall {
        FirewallDto::Blocked { .. } => "blocked",
        FirewallDto::Yanked => "yanked",
        // No bytes here, and we know about it only because we asked upstream.
        _ if row.source == "upstream" => "pending",
        FirewallDto::Clear => "cached",
    }
}

/// Whether the console may offer **Fetch this version**, and why not.
///
/// Both halves are the server's to know — the operator's switch and whether the
/// registry kind has one artifact per version — so the page is told rather than
/// left to guess. A console that guessed would draw a button that always fails
/// on Maven (RFC 0007-bis §4.4).
async fn fetch_offer(
    local_svc: &LocalRegistryService,
    registry_map: &RegistryMap,
    registry: &str,
    identity: &Identity,
) -> FetchOfferDto {
    // A reader with no session cannot pull — `explore_fetch_version` answers
    // `401 fetch.unauthenticated` — so the button is not offered to one. Decided
    // here rather than by the console for the same reason the other two halves
    // are: the offer and the endpoint must agree, and a page that drew the button
    // anyway would be promising something the API refuses.
    //
    // The kind's reason is computed first and kept, so it survives the override:
    // on a Maven registry the honest answer is still "this kind has no single
    // artifact per version", and "sign in" would be advice that does not help.
    // Where the offer *would* have been made, the reason is `None` and the
    // console says the one thing it knows better than the server — that this
    // viewer has no session — in its own translated words.
    if identity.role == Role::Anonymous {
        let would_offer = fetch_offer_for_registry(local_svc, registry_map, registry).await;
        return FetchOfferDto {
            offered: false,
            reason: would_offer.reason,
        };
    }
    fetch_offer_for_registry(local_svc, registry_map, registry).await
}

/// The half that is about the registry: the operator's switch and whether the
/// kind has one artifact per version.
async fn fetch_offer_for_registry(
    local_svc: &LocalRegistryService,
    registry_map: &RegistryMap,
    registry: &str,
) -> FetchOfferDto {
    let enabled = local_svc
        .hot
        .read()
        .await
        .console_fetch
        .get(registry)
        .copied()
        .unwrap_or(batlehub_core::services::DEFAULT_CONSOLE_FETCH);
    if !enabled {
        // No reason given: the operator turned it off, and explaining an
        // operator's own configuration back to them on a package page is noise.
        return FetchOfferDto {
            offered: false,
            reason: None,
        };
    }
    match registry_map
        .type_of(registry)
        .and_then(|t| t.parse::<RegistryKind>().ok())
        .map(|kind| kind.fetchable_by_version())
    {
        Some(support) if support.is_supported() => FetchOfferDto {
            offered: true,
            reason: None,
        },
        // The kind's own reason, verbatim — so the published support table, the
        // endpoint's refusal and this page cannot disagree.
        Some(support) => FetchOfferDto {
            offered: false,
            reason: support.reason().map(str::to_owned),
        },
        None => FetchOfferDto {
            offered: false,
            reason: None,
        },
    }
}

/// The discovery read, and everything that decides not to do it.
struct DiscoveryResult {
    detail: Option<batlehub_core::services::UpstreamDetail>,
    dto: UpstreamReadDto,
}

async fn discovery_read(
    proxy_svc: &ProxyService,
    mode_map: &RegistryModeMap,
    registry: &str,
    name: &str,
    identity: &AuthIdentity,
    mode: UpstreamMode,
    // Every reason not to ask upstream, already decided by the caller: the
    // package is hosted locally, or this caller may not proxy from the
    // registry at all.
    suppress_upstream: bool,
) -> DiscoveryResult {
    let not_attempted = |error: Option<String>| DiscoveryResult {
        detail: None,
        dto: UpstreamReadDto {
            attempted: false,
            freshness: None,
            version_count: 0,
            truncated: false,
            error,
        },
    };

    if mode == UpstreamMode::Skip || suppress_upstream {
        return not_attempted(None);
    }
    // A `local`-mode registry has no upstream: there is nothing to ask, and
    // asking would be a request to a URL that is not configured.
    if mode_map.get(registry) == RegistryMode::Local {
        return not_attempted(None);
    }

    match proxy_svc.upstream_detail(registry, name, &identity.0).await {
        Ok(Some(outcome)) => DiscoveryResult {
            dto: UpstreamReadDto {
                attempted: true,
                freshness: Some(freshness_word(outcome.freshness).to_owned()),
                version_count: outcome.detail.versions.len(),
                truncated: outcome.truncated,
                error: None,
            },
            detail: Some(outcome.detail),
        },
        // Not attempted: the registry has it off, the kind cannot be asked, or
        // upstream is already known not to have this package. None of those is
        // an error, and reporting one would put a banner on a page that is
        // simply complete.
        Ok(None) => not_attempted(None),
        // Attempted and every rung failed. The page answers from local rows and
        // says so — rung 3 never degrades to an empty page presented as an
        // answer, which is what the old "no versions yet" did.
        Err(e) => {
            tracing::debug!(
                registry = %registry, package = %name, error = %e,
                "explore detail: the discovery read failed; answering from local rows"
            );
            DiscoveryResult {
                detail: None,
                dto: UpstreamReadDto {
                    attempted: true,
                    freshness: None,
                    version_count: 0,
                    truncated: false,
                    error: Some(e.to_string()),
                },
            }
        }
    }
}

/// The vocabulary `Freshness::header_value` already uses, so the page says the
/// same word the protocol paths do.
fn freshness_word(freshness: Freshness) -> &'static str {
    match freshness {
        Freshness::Cached => "cached",
        Freshness::Fresh => "fresh",
        Freshness::Stale => "stale",
    }
}
