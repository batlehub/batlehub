use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::entities::Role;

/// Where a package entry originates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageSource {
    #[default]
    Proxied,
    Local,
    Both,
}

/// Sort order for explore queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExploreSortBy {
    #[default]
    Downloads,
    Name,
    /// Most recently downloaded *from* this instance.
    Recent,
    /// Most recently fetched *by* this instance from upstream — the order the
    /// design proof's catalog is in, and a different fact from `Recent`.
    Fetched,
}

/// One collapsed entry per (registry, package_name) in the explorer listing.
#[derive(Debug, Clone, Default)]
pub struct ExploreEntry {
    pub registry: String,
    pub name: String,
    pub version_count: u64,
    pub total_downloads: u64,
    /// When a client last *downloaded* this package from us.
    pub last_accessed: Option<DateTime<Utc>>,
    pub source: PackageSource,
    /// An administrator has blocked at least one version.
    pub has_blocked: bool,
    /// An owner has yanked at least one locally published version.
    ///
    /// Split out of `has_blocked`, which used to carry both: the explore query
    /// folded `BOOL_OR(lp.yanked)` into the same column as
    /// `BOOL_OR(status = 'blocked')`, so a yanked package and a blocked one
    /// were indistinguishable to every caller. They are different facts with
    /// different remedies — a block is lifted by an operator, a yank is not —
    /// and [`ResolutionState`] has to tell them apart.
    pub has_yanked: bool,
    /// How many versions of this package we currently hold bytes for.
    pub cached_versions: u64,
    /// Bytes held, summed across those versions.
    ///
    /// `None` means *unknown*, not zero: `artifact_cache_meta.size_bytes` is
    /// nullable, so an artifact cached before the size was recorded contributes
    /// no number. Zero would claim we hold an empty file.
    pub cached_bytes: Option<u64>,
    /// When we last fetched any version of this package from upstream.
    ///
    /// Distinct from `last_accessed`, which is the last time somebody read it
    /// *from* us. The proof's catalog is ordered on this one.
    pub last_fetched_at: Option<DateTime<Utc>>,
    /// The version we most recently obtained, by whichever timestamp applies —
    /// `cached_at` for a proxied artifact, `published_at` for a local one.
    ///
    /// Not "the greatest version": version strings do not sort semantically
    /// (`1.10.0` < `1.9.0` as text), and this row is collapsed across versions,
    /// so the honest answer is the one we saw last rather than a comparison we
    /// cannot make in SQL.
    pub newest_version: Option<String>,
    /// Publication time of `newest_version`, when it is known.
    ///
    /// Only local packages carry one: the upstream publish date is not
    /// persisted for proxied artifacts. [`ReleaseAgeGateParams`] already
    /// defines what to do when it is missing, so this is an input to that rule
    /// rather than a gap in it.
    pub newest_published_at: Option<DateTime<Utc>>,
}

// ── Resolution as state ───────────────────────────────────────────────────────

/// DESIGN.md's six states, which the console draws as a dot matrix: a fine 3×3
/// for what this instance holds and has verified, a coarse 2×2 for what it does
/// not, and the lit-cell count and hue for *which* not-held state it is.
///
/// This lives in `core` rather than in the handler because it is a statement
/// about the domain — "what is the standing of this package here" — and because
/// deriving it is the kind of ordered rule that wants unit tests without a
/// database or an HTTP app behind it. The handler's only job is to name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolutionState {
    /// Held and verified.
    Cached,
    /// Held, but past the registry's artifact TTL.
    Stale,
    /// Inside a release-age quarantine for this viewer.
    Held,
    /// Known to exist here, no bytes held yet.
    Pending,
    /// Withdrawn by its owner.
    Yanked,
    /// Refused by an administrator.
    Blocked,
}

impl ResolutionState {
    /// The wire name, matching the `serde` rename above.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cached => "cached",
            Self::Stale => "stale",
            Self::Held => "held",
            Self::Pending => "pending",
            Self::Yanked => "yanked",
            Self::Blocked => "blocked",
        }
    }
}

/// The release-age gate's parameters, as plain data.
///
/// [`crate::rules::ReleaseAgeGateRule`] is a `Box<dyn Rule>` by the time it
/// reaches `HotConfig`, so its `min_age` and `bypass_roles` cannot be read back
/// out of it. The catalog needs those numbers to say whether a package is
/// quarantined *without* running the download path, so the builder records them
/// alongside the rule. Keeping the shape identical is what makes the two agree.
#[derive(Debug, Clone)]
pub struct ReleaseAgeGateParams {
    pub min_age: Duration,
    pub bypass_roles: Vec<Role>,
    pub deny_missing_timestamp: bool,
}

/// Per-registry inputs for [`resolve_state`].
#[derive(Debug, Clone, Default)]
pub struct ResolutionPolicy {
    /// `artifact_ttl_secs` from the registry's cache config. `None` means
    /// artifacts here never go stale, which is also the config's default.
    pub artifact_ttl: Option<Duration>,
    /// The registry's release-age gate, if it has one configured.
    pub release_age: Option<ReleaseAgeGateParams>,
}

/// Name a package's resolution state.
///
/// ## The order is the design
///
/// A package can satisfy several of these at once — a blocked version of a
/// package we also hold stale bytes for is both — so the sequence below is what
/// actually decides what a reader sees. It runs from "you cannot have this, and
/// here is who decided" down to "you can have this":
///
/// 1. **Blocked** — an operator's refusal. Loudest, and actionable by an
///    operator, so it outranks everything.
/// 2. **Yanked** — the owner withdrew it. Above `Held` because a yank is
///    permanent and about the artifact itself, whereas a quarantine is a clock
///    that will run out on its own.
/// 3. **Held** — inside the release-age window *for this viewer*. Above
///    `Pending` because "not here yet" is true of a quarantined package and
///    tells the reader nothing about why.
/// 4. **Pending** — nothing held. Before `Stale` because staleness is a
///    statement about bytes, and there are none.
/// 5. **Stale** — held, but past the TTL.
/// 6. **Cached** — held and current.
///
/// `now` is a parameter rather than `Utc::now()` so the ordering can be tested
/// against fixed instants.
pub fn resolve_state(
    entry: &ExploreEntry,
    policy: &ResolutionPolicy,
    viewer: &Role,
    now: DateTime<Utc>,
) -> ResolutionState {
    if entry.has_blocked {
        return ResolutionState::Blocked;
    }
    if entry.has_yanked {
        return ResolutionState::Yanked;
    }
    if let Some(gate) = &policy.release_age {
        if is_quarantined(gate, entry.newest_published_at, viewer, now) {
            return ResolutionState::Held;
        }
    }
    if entry.cached_versions == 0 {
        return ResolutionState::Pending;
    }
    if let (Some(ttl), Some(fetched)) = (policy.artifact_ttl, entry.last_fetched_at) {
        // `to_std` fails on a negative span — a `cached_at` in the future,
        // which a clock skew between writer and reader can produce. That is
        // "fetched very recently", so it falls through to Cached.
        if (now - fetched).to_std().is_ok_and(|age| age >= ttl) {
            return ResolutionState::Stale;
        }
    }
    ResolutionState::Cached
}

/// Whether the release-age gate would refuse this viewer, transcribed from
/// [`crate::rules::ReleaseAgeGateRule::evaluate`].
///
/// Deliberately a transcription and not an approximation, including the two
/// edges that are easy to get wrong:
///
/// - A bypass role short-circuits *both* branches, the missing-timestamp one
///   included. An admin is never told a package is quarantined.
/// - A missing timestamp is only a refusal when `deny_missing_timestamp` is on.
///   Proxied artifacts never carry a publish date (it is not persisted), so on
///   the default setting a proxy-only registry simply never reports `Held` —
///   which is exactly what the download path does, rather than a gap in it.
fn is_quarantined(
    gate: &ReleaseAgeGateParams,
    published_at: Option<DateTime<Utc>>,
    viewer: &Role,
    now: DateTime<Utc>,
) -> bool {
    if viewer.clears_bypass(&gate.bypass_roles) {
        return false;
    }
    match published_at {
        None => gate.deny_missing_timestamp,
        Some(published) => match (now - published).to_std() {
            Ok(age) => age < gate.min_age,
            // Published in the future. The rule's `to_std().unwrap_or_default()`
            // reads that span as age zero, which is inside any non-zero window,
            // so it denies — and so does this.
            Err(_) => true,
        },
    }
}

/// Who is asking, for the purpose of per-package visibility filtering.
///
/// Registry-level access is a separate, coarser gate handled by
/// `ExploreFilter::registries`. This type carries what is needed to apply the
/// *package*-level `Visibility` rule — the same rule
/// `LocalRegistryService::check_visibility` enforces on the download path — to a
/// listing, so an `internal`/`team` package does not surface its name and version
/// count to someone who could never download it.
///
/// [`Default`] is the anonymous viewer: not an admin, not authenticated, no
/// groups. That is the safe default for a filter built without thinking about
/// it — it sees only `public` packages.
#[derive(Debug, Clone, Default)]
pub struct ExploreViewer {
    /// Admins bypass visibility entirely, matching `check_visibility`.
    pub is_admin: bool,
    /// Any non-anonymous identity. Gates `Visibility::Internal`.
    pub is_authenticated: bool,
    /// The caller's group memberships, used for `Visibility::Team`.
    pub groups: Vec<String>,
}

impl ExploreViewer {
    /// Group ids with spaces removed, matching how `check_team_visibility`
    /// compares them (`g.replace(' ', "")`). Pre-normalising here keeps the
    /// comparison identical on the SQL side, where doing it per row would be
    /// both slower and easy to get subtly different.
    pub fn normalised_groups(&self) -> Vec<String> {
        self.groups.iter().map(|g| g.replace(' ', "")).collect()
    }
}

/// Filter for explore queries.
#[derive(Debug, Clone, Default)]
pub struct ExploreFilter {
    pub registry: Option<String>,
    /// Access-control allow-list. Empty means "all accessible registries".
    pub registries: Vec<String>,
    pub name_contains: Option<String>,
    /// Restrict to these exact `(registry, name)` pairs. Empty means no
    /// restriction.
    ///
    /// How a README search's hits become catalogue rows: the search names the
    /// packages, and this query supplies everything the listing shows about
    /// them — **through the same visibility gate**, so a package hidden from the
    /// listing does not become visible by quoting a phrase from its README
    /// (RFC 0007-bis §7.3). ANDed with `name_contains` when both are set, which
    /// no caller does.
    pub name_in: Vec<(String, String)>,
    pub sort_by: ExploreSortBy,
    pub limit: u64,
    pub offset: u64,
    /// Package-level visibility gate. See [`ExploreViewer`].
    pub viewer: ExploreViewer,
}

/// Per-registry statistics for the explorer sidebar.
#[derive(Debug, Clone, Default)]
pub struct RegistryStat {
    pub registry: String,
    pub package_count: u64,
    pub total_downloads: u64,
    /// Bytes this instance holds for the registry.
    ///
    /// `None` is unknown, not zero — see [`ExploreEntry::cached_bytes`]. The
    /// design proof's catalog caption spends one of its four facts on this
    /// ("6.1 GB cached"); it is the answer to "what is this registry costing
    /// us", which nothing else on the page reports.
    pub cached_bytes: Option<u64>,
}

/// Full package detail returned by the explorer detail endpoint.
#[derive(Debug, Clone)]
pub struct ExplorePackageDetail {
    pub registry: String,
    pub name: String,
    pub gate: GateInfo,
    pub versions: Vec<ExploreVersionEntry>,
}

/// Gate/access status for a package's registry.
#[derive(Debug, Clone)]
pub struct GateInfo {
    /// Whether the caller's role can access this registry through the proxy.
    pub registry_accessible: bool,
    /// Whether the caller is a beta-channel member for this registry.
    pub beta_member: bool,
}

/// One version of a package, with source and firewall status.
#[derive(Debug, Clone)]
pub struct ExploreVersionEntry {
    pub version: String,
    pub source: String,
    pub firewall: FirewallInfo,
    pub download_count: u64,
    pub last_accessed: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    /// `true` when the version string contains a `-` (semver pre-release component).
    pub is_prerelease: bool,
}

/// Firewall / blocking status of a single package version.
#[derive(Debug, Clone)]
pub enum FirewallInfo {
    Clear,
    Blocked {
        reason: String,
        blocked_by: String,
        blocked_at: DateTime<Utc>,
    },
    Yanked,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as CDuration;

    const HOUR: Duration = Duration::from_secs(3600);

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("fixed instant")
    }

    /// A package we hold one current version of, and nothing wrong with it.
    fn cached_entry() -> ExploreEntry {
        ExploreEntry {
            registry: "npm1".into(),
            name: "serde".into(),
            version_count: 1,
            cached_versions: 1,
            cached_bytes: Some(4096),
            last_fetched_at: Some(now() - CDuration::minutes(5)),
            newest_version: Some("1.0.219".into()),
            ..Default::default()
        }
    }

    fn gate(min_age: Duration, bypass: Vec<Role>) -> ResolutionPolicy {
        ResolutionPolicy {
            artifact_ttl: None,
            release_age: Some(ReleaseAgeGateParams {
                min_age,
                bypass_roles: bypass,
                deny_missing_timestamp: false,
            }),
        }
    }

    #[test]
    fn held_and_verified_is_cached() {
        let state = resolve_state(
            &cached_entry(),
            &ResolutionPolicy::default(),
            &Role::User,
            now(),
        );
        assert_eq!(state, ResolutionState::Cached);
    }

    #[test]
    fn known_but_unheld_is_pending() {
        let entry = ExploreEntry {
            cached_versions: 0,
            cached_bytes: None,
            last_fetched_at: None,
            ..cached_entry()
        };
        let state = resolve_state(&entry, &ResolutionPolicy::default(), &Role::User, now());
        assert_eq!(state, ResolutionState::Pending);
    }

    #[test]
    fn past_the_ttl_is_stale() {
        let policy = ResolutionPolicy {
            artifact_ttl: Some(HOUR),
            release_age: None,
        };
        let entry = ExploreEntry {
            last_fetched_at: Some(now() - CDuration::hours(2)),
            ..cached_entry()
        };
        assert_eq!(
            resolve_state(&entry, &policy, &Role::User, now()),
            ResolutionState::Stale
        );

        // Inside the window it is simply current. Pinned alongside the case
        // above because an off-by-one here silently ages the whole catalog.
        let fresh = ExploreEntry {
            last_fetched_at: Some(now() - CDuration::minutes(59)),
            ..cached_entry()
        };
        assert_eq!(
            resolve_state(&fresh, &policy, &Role::User, now()),
            ResolutionState::Cached
        );
    }

    /// A TTL with nothing held is `Pending`, not `Stale`: staleness is a claim
    /// about bytes, and there are none to make it about.
    #[test]
    fn nothing_held_is_pending_even_past_the_ttl() {
        let policy = ResolutionPolicy {
            artifact_ttl: Some(HOUR),
            release_age: None,
        };
        let entry = ExploreEntry {
            cached_versions: 0,
            last_fetched_at: Some(now() - CDuration::hours(9)),
            ..cached_entry()
        };
        assert_eq!(
            resolve_state(&entry, &policy, &Role::User, now()),
            ResolutionState::Pending
        );
    }

    #[test]
    fn inside_the_quarantine_window_is_held() {
        let entry = ExploreEntry {
            newest_published_at: Some(now() - CDuration::minutes(11)),
            ..cached_entry()
        };
        assert_eq!(
            resolve_state(&entry, &gate(HOUR, vec![]), &Role::User, now()),
            ResolutionState::Held
        );
    }

    /// The gate exempts a role from the *download*, so the catalog must not
    /// tell that viewer the package is quarantined — they can have it.
    #[test]
    fn a_bypass_role_never_sees_held() {
        let entry = ExploreEntry {
            newest_published_at: Some(now() - CDuration::minutes(11)),
            ..cached_entry()
        };
        let policy = gate(HOUR, vec![Role::Admin]);
        assert_eq!(
            resolve_state(&entry, &policy, &Role::Admin, now()),
            ResolutionState::Cached
        );
        assert_eq!(
            resolve_state(&entry, &policy, &Role::User, now()),
            ResolutionState::Held
        );
    }

    /// Proxied artifacts carry no publish date. On the rule's default setting
    /// that is not a refusal, so the catalog must not invent one.
    #[test]
    fn a_missing_publish_date_follows_deny_missing_timestamp() {
        let entry = ExploreEntry {
            newest_published_at: None,
            ..cached_entry()
        };
        assert_eq!(
            resolve_state(&entry, &gate(HOUR, vec![]), &Role::User, now()),
            ResolutionState::Cached
        );

        let mut strict = gate(HOUR, vec![]);
        strict
            .release_age
            .as_mut()
            .expect("gate")
            .deny_missing_timestamp = true;
        assert_eq!(
            resolve_state(&entry, &strict, &Role::User, now()),
            ResolutionState::Held
        );
    }

    /// A clock running backwards between writer and reader puts `published_at`
    /// in the future. `ReleaseAgeGateRule` reads that span as age zero and
    /// denies; this must agree, or the catalog would show as downloadable a
    /// package the download path refuses.
    #[test]
    fn a_publish_date_in_the_future_is_held_like_the_rule_holds_it() {
        let entry = ExploreEntry {
            newest_published_at: Some(now() + CDuration::hours(1)),
            ..cached_entry()
        };
        assert_eq!(
            resolve_state(&entry, &gate(HOUR, vec![]), &Role::User, now()),
            ResolutionState::Held
        );
    }

    /// The whole point of the ordering. Each of these entries satisfies more
    /// than one state; the table says which one wins, in the sequence
    /// `resolve_state` documents.
    #[test]
    fn precedence_runs_blocked_yanked_held_pending_stale() {
        let policy = ResolutionPolicy {
            artifact_ttl: Some(HOUR),
            release_age: Some(ReleaseAgeGateParams {
                min_age: HOUR,
                bypass_roles: vec![],
                deny_missing_timestamp: false,
            }),
        };
        // Stale bytes, a quarantined newest version, yanked and blocked at once.
        let everything = ExploreEntry {
            has_blocked: true,
            has_yanked: true,
            last_fetched_at: Some(now() - CDuration::hours(9)),
            newest_published_at: Some(now() - CDuration::minutes(1)),
            ..cached_entry()
        };
        assert_eq!(
            resolve_state(&everything, &policy, &Role::User, now()),
            ResolutionState::Blocked
        );

        let yanked = ExploreEntry {
            has_blocked: false,
            ..everything.clone()
        };
        assert_eq!(
            resolve_state(&yanked, &policy, &Role::User, now()),
            ResolutionState::Yanked
        );

        let held = ExploreEntry {
            has_yanked: false,
            ..yanked.clone()
        };
        assert_eq!(
            resolve_state(&held, &policy, &Role::User, now()),
            ResolutionState::Held
        );

        // Out of quarantine, nothing held: Pending outranks Stale.
        let pending = ExploreEntry {
            newest_published_at: Some(now() - CDuration::days(30)),
            cached_versions: 0,
            ..held.clone()
        };
        assert_eq!(
            resolve_state(&pending, &policy, &Role::User, now()),
            ResolutionState::Pending
        );

        let stale = ExploreEntry {
            cached_versions: 2,
            ..pending.clone()
        };
        assert_eq!(
            resolve_state(&stale, &policy, &Role::User, now()),
            ResolutionState::Stale
        );
    }

    /// The wire names are what the console keys its dot matrix off, so they are
    /// pinned rather than left to `serde`'s rename surviving a refactor.
    #[test]
    fn wire_names_match_the_design_vocabulary() {
        assert_eq!(ResolutionState::Cached.as_str(), "cached");
        assert_eq!(ResolutionState::Stale.as_str(), "stale");
        assert_eq!(ResolutionState::Held.as_str(), "held");
        assert_eq!(ResolutionState::Pending.as_str(), "pending");
        assert_eq!(ResolutionState::Yanked.as_str(), "yanked");
        assert_eq!(ResolutionState::Blocked.as_str(), "blocked");
    }
}
