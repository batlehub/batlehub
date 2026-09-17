use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::entities::{
    AccessEvent, EventFilter, ExploreEntry, ExploreFilter, ExploreViewer, PackageFilter, PackageId,
    PackageStatus, PackageSummary, RegistryStat,
};
use crate::error::CoreError;

/// A single row from the `access_events` "recent errors" query surfaced on the
/// admin health dashboard: a denied or upstream-error event for a registry.
#[derive(Debug, Clone)]
pub struct RecentErrorRecord {
    pub created_at: DateTime<Utc>,
    pub user_id: Option<String>,
    pub package_name: String,
    pub package_version: String,
    /// "denied" or "error".
    pub outcome: String,
    pub deny_reason: Option<String>,
}

/// Page size the default [`PackageRepository::blocked_versions`] asks for. A
/// package with more blocked versions than this would have the excess treated as
/// unblocked, so it is set far above any plausible real count rather than at a
/// display-friendly page size.
pub const MAX_BLOCKED_VERSIONS_PER_PACKAGE: u64 = 10_000;

/// Page size the default [`PackageRepository::blocked_in_registry`] asks for.
/// Same reasoning as [`MAX_BLOCKED_VERSIONS_PER_PACKAGE`], one order of
/// magnitude up because the scope is a whole registry rather than one package:
/// blocks past this bound would be treated as unblocked in multi-package
/// listings, so the ceiling sits far above any plausible real count.
pub const MAX_BLOCKED_VERSIONS_PER_REGISTRY: u64 = 100_000;

/// Persistent store for package statuses and access audit logs.
///
/// Backed by a relational database (PostgreSQL, MySQL, …).
#[async_trait]
pub trait PackageRepository: Send + Sync {
    /// Record an access event (download attempt, block action, etc.).
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError>;

    /// Get the current administrative status of a package.
    /// Returns `PackageStatus::Available` if the package has never been seen.
    async fn get_status(&self, pkg: &PackageId) -> Result<PackageStatus, CoreError>;

    /// Every blocked version of one package, in no particular order.
    ///
    /// The bulk counterpart to [`Self::get_status`], for the version *listing*
    /// paths: a packument or version index has to know which of a package's
    /// versions are blocked before it can leave them out, and asking per version
    /// would be one query per version on a hot metadata path.
    ///
    /// The default implementation derives the answer from
    /// [`Self::list_packages`] so every existing implementor keeps working;
    /// backends with a cheaper query (a single indexed `SELECT` rather than the
    /// listing query's audit-count joins) should override it.
    async fn blocked_versions(&self, registry: &str, name: &str) -> Result<Vec<String>, CoreError> {
        // `PackageFilter::default()` leaves `limit` at 0, which the SQL renders
        // as `LIMIT 0` — an explicit page size is required or this silently
        // returns nothing and every version reads as unblocked.
        let filter = PackageFilter {
            registry: Some(registry.to_owned()),
            name_exact: Some(name.to_owned()),
            blocked_only: true,
            limit: MAX_BLOCKED_VERSIONS_PER_PACKAGE,
            ..Default::default()
        };
        Ok(self
            .list_packages(filter)
            .await?
            .into_iter()
            .map(|p| p.package_id.version)
            .collect())
    }

    /// When this package's newest block was written, or `None` when it has no
    /// block rows.
    ///
    /// One field, for one document: Ansible Galaxy's collection document is
    /// re-read by `ansible-galaxy` on every resolve, *uncached*, and its
    /// `updated_at` is how the client decides whether to drop its day-old copy
    /// of the versions list. Serving `max(upstream updated_at, newest
    /// blocked_at)` is what turns a day of `404`s on a freshly blocked version
    /// into a correct resolution (RFC 0031 §4.4).
    ///
    /// Asked separately from [`Self::blocked_versions`] rather than folded into
    /// it, because every other kind's listing path would then pay for a column
    /// no other document reads.
    ///
    /// The default derives the answer from [`Self::list_packages`], mirroring
    /// its siblings; backends with a cheaper query should override it.
    async fn blocked_changed_at(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, CoreError> {
        let filter = PackageFilter {
            registry: Some(registry.to_owned()),
            name_exact: Some(name.to_owned()),
            blocked_only: true,
            limit: MAX_BLOCKED_VERSIONS_PER_PACKAGE,
            ..Default::default()
        };
        Ok(self
            .list_packages(filter)
            .await?
            .into_iter()
            .filter_map(|p| match p.status {
                crate::entities::PackageStatus::Blocked { blocked_at, .. } => Some(blocked_at),
                crate::entities::PackageStatus::Available => None,
            })
            .max())
    }

    /// Every blocked `(name, version)` in one registry, in no particular order.
    ///
    /// For the *multi-package* listing documents — conda's `repodata.json`, a
    /// JetBrains `updatePlugins.xml`, a forge's release listing — which describe
    /// many packages at once. [`Self::blocked_versions`] is the wrong query
    /// shape there: filtering a channel's repodata one package at a time would
    /// be a query per package in the document.
    ///
    /// Callers are expected to hold the result briefly rather than per request
    /// (`repodata.json` for a busy channel is requested on every `conda
    /// install`), which is the one place a block is not effective on the very
    /// next request; see `BlockedRegistrySnapshot`.
    ///
    /// The default derives the answer from [`Self::list_packages`], mirroring
    /// [`Self::blocked_versions`]; backends with a cheaper query should
    /// override it.
    async fn blocked_in_registry(
        &self,
        registry: &str,
    ) -> Result<Vec<(String, String)>, CoreError> {
        let filter = PackageFilter {
            registry: Some(registry.to_owned()),
            blocked_only: true,
            limit: MAX_BLOCKED_VERSIONS_PER_REGISTRY,
            ..Default::default()
        };
        Ok(self
            .list_packages(filter)
            .await?
            .into_iter()
            .map(|p| (p.package_id.name, p.package_id.version))
            .collect())
    }

    /// Update the administrative status of a package.
    async fn set_status(&self, pkg: &PackageId, status: PackageStatus) -> Result<(), CoreError>;

    /// Remove a package's administrative record entirely.
    /// Returns `true` if a row was found and deleted, `false` if it did not exist.
    async fn delete_package(&self, pkg: &PackageId) -> Result<bool, CoreError>;

    /// List all known packages with optional filtering and pagination.
    async fn list_packages(&self, filter: PackageFilter) -> Result<Vec<PackageSummary>, CoreError>;

    /// Count matching packages without applying `limit`/`offset`. Used for accurate pagination totals.
    async fn count_packages(&self, filter: PackageFilter) -> Result<u64, CoreError>;

    /// Query the access event log.
    async fn list_events(&self, filter: EventFilter) -> Result<Vec<AccessEvent>, CoreError>;

    /// Count matching access events without applying `limit`/`offset`. Used for accurate pagination totals.
    async fn count_events(&self, filter: EventFilter) -> Result<u64, CoreError>;

    /// The caller's own successful downloads, newest first.
    ///
    /// Deliberately *not* `list_events` with a filter: this backs
    /// `GET /api/v1/me/downloads`, and the scoping that keeps one user out of
    /// another's history belongs here rather than in a handler, where a
    /// forgotten `user_id` would leak the whole log (RFC 0004 §6.2, §7). The
    /// three constraints — this principal, `AccessAction::Download`, allowed
    /// only — are the method's contract, not the caller's to assemble.
    ///
    /// `since` bounds how far back to look; `limit` caps the rows returned.
    /// Anonymous callers have no history: there is no `user_id` to scope by, so
    /// the handler must not call this for them.
    async fn list_own_downloads(
        &self,
        user_id: &str,
        since: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        let (_, _, _) = (user_id, since, limit);
        Ok(vec![])
    }

    /// The newest successful **download** of each version of one package.
    ///
    /// The signal retention's `keep_if_pulled` reads (RFC 0016 §4.3), and the
    /// rule that makes retention safe to switch on: whatever anyone is actually
    /// using stays, regardless of age or count.
    ///
    /// Three constraints are the method's contract rather than the caller's to
    /// assemble, for the same reason [`Self::list_own_downloads`] states its own.
    ///
    /// 1. **`AccessAction::Download` only.** A `ViewMetadata` is a client
    ///    reading an index, not a consumer installing anything. Counting it
    ///    would keep every version any listing ever mentioned.
    /// 2. **Allowed only.** A denied download is evidence someone *wanted* the
    ///    version and did not get it, which is not evidence the version is in
    ///    use — and a blocked package would otherwise defend itself from
    ///    reclamation by being repeatedly refused.
    /// 3. **Per version, newest first.** A sweep asks about a whole package at
    ///    once; one round trip per version would make a retention run over
    ///    200 000 packages a million queries.
    ///
    /// The `Download`/`ViewMetadata` split is drawn at record time by
    /// [`PackageId::is_verification_sidecar`], not here: a `.sha1` beside a jar
    /// records as `ViewMetadata` and a `.pom` records as a `Download`, because a
    /// `.pom` is a file a build actually consumes. Both halves matter to
    /// retention and both are pinned by tests.
    ///
    /// Returns `(version, last_download)` pairs. A version with no recorded
    /// download is **absent** rather than present-with-`None` — the caller has
    /// to decide what an absence means, and under the floor-date rule in
    /// RFC 0016 §4.3 it does not always mean "never pulled".
    ///
    /// [`PackageId::is_verification_sidecar`]: crate::entities::PackageId::is_verification_sidecar
    async fn last_downloads(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>, CoreError> {
        let (_, _) = (registry, package);
        Ok(vec![])
    }

    /// Delete access-event rows older than `before`. Returns the number of rows deleted.
    async fn purge_events_before(&self, before: DateTime<Utc>) -> Result<u64, CoreError> {
        let _ = before;
        Ok(0)
    }

    /// Distinct non-null `user_id`s the access log has seen, newest activity
    /// first, optionally narrowed to those containing `contains`.
    ///
    /// RFC 0004-bis A8. Four console fields ask an operator to type a subject
    /// and nothing can offer one: `/api/v1/admin/users/blocked` lists only the
    /// *blocked*. The failure is silent — filtering the audit log for `alice`
    /// on an instance that stores `oidc:alice` returns an empty table, which
    /// reads exactly like "this user did nothing" on the surface whose entire
    /// purpose is establishing what someone did.
    ///
    /// Scoped to identities this instance has actually seen, not a user
    /// directory: this product does not have one and should not grow one here.
    async fn distinct_event_subjects(
        &self,
        contains: Option<&str>,
        limit: u64,
    ) -> Result<Vec<String>, CoreError> {
        let _ = (contains, limit);
        Ok(vec![])
    }

    /// Explorer: collapsed list of packages (one entry per name) from both proxied and local sources.
    async fn explore_packages(
        &self,
        filter: ExploreFilter,
    ) -> Result<Vec<ExploreEntry>, CoreError> {
        let _ = filter;
        Ok(vec![])
    }

    /// Explorer: count of unique (registry, name) pairs matching the filter.
    async fn count_explore_packages(&self, filter: ExploreFilter) -> Result<u64, CoreError> {
        let _ = filter;
        Ok(0)
    }

    /// Explorer: per-registry package counts and download totals.
    ///
    /// `viewer` is not optional and not cosmetic (RFC 0015 §4.4): every number
    /// here is an aggregate over packages, so one computed without it discloses
    /// the set it was computed over. An empty `accessible_registries` means
    /// **nothing**, never "all" — survey finding 2's reading is what this
    /// signature exists to make unavailable.
    async fn registry_explore_stats(
        &self,
        accessible_registries: &[String],
        viewer: &ExploreViewer,
    ) -> Result<Vec<RegistryStat>, CoreError> {
        let _ = (accessible_registries, viewer);
        Ok(vec![])
    }

    /// Admin health dashboard: distinct package counts per registry, keyed by
    /// registry name. Registries with no packages are simply absent from the map.
    async fn registry_package_counts(
        &self,
        registries: &[String],
    ) -> Result<HashMap<String, i64>, CoreError> {
        let _ = registries;
        Ok(HashMap::new())
    }

    /// Admin health dashboard: per-registry download stats — last successful
    /// pull time, pulls in the last hour, and pulls in the last day. Keyed by
    /// registry name; registries with no matching events are absent from the map.
    async fn registry_event_stats(
        &self,
        registries: &[String],
    ) -> Result<HashMap<String, (Option<DateTime<Utc>>, i64, i64)>, CoreError> {
        let _ = registries;
        Ok(HashMap::new())
    }

    /// Admin health dashboard: most recent denied/error access events for a
    /// single registry within the last 24 hours, newest first, capped at `limit`.
    async fn recent_registry_errors(
        &self,
        registry: &str,
        limit: i64,
    ) -> Result<Vec<RecentErrorRecord>, CoreError> {
        let _ = (registry, limit);
        Ok(Vec::new())
    }
}
