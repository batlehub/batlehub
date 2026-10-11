use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

use batlehub_core::{
    entities::{
        AccessAction, AccessEvent, AccessResult, EventFilter, ExploreEntry, ExploreFilter,
        ExploreSortBy, PackageFilter, PackageId, PackageSource, PackageStatus, PackageSummary,
    },
    error::CoreError,
    ports::{PackageRepository, RecentErrorRecord},
};

/// In-memory [`PackageRepository`].
///
/// Stores package summaries keyed by [`PackageId::cache_key`] and access
/// events in an append-only `Vec`. `list_packages` and `list_events` honour
/// all filter fields including pagination (`limit` / `offset`).
/// A `limit` of `0` is treated as "no limit".
#[derive(Debug, Default)]
pub struct InMemoryPackageRepository {
    summaries: Arc<RwLock<HashMap<String, PackageSummary>>>,
    pub(crate) events: Arc<RwLock<Vec<AccessEvent>>>,
    /// The audit seal chain (RFC 0036 §5.3), for `AuditTrailStore`.
    pub(crate) seals: Arc<RwLock<Vec<batlehub_core::entities::SealRecord>>>,
}

impl InMemoryPackageRepository {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl InMemoryPackageRepository {
    /// The catalogue rows this store can account for, before pagination.
    ///
    /// Built from the same `PackageSummary` rows `list_packages` reads, so a
    /// test that seeds one gets it from both. It is deliberately a **partial**
    /// double: it knows nothing about locally published packages, cached bytes
    /// or download counts, because those live in stores this one does not have.
    /// What it does know — which packages exist, in which registry, and how many
    /// versions — is what the search paths need (RFC 0007-bis §4.3).
    async fn explore_rows(&self, filter: &ExploreFilter) -> Vec<ExploreEntry> {
        let summaries = self.summaries.read().await;
        let mut by_package: HashMap<(String, String), ExploreEntry> = HashMap::new();

        for summary in summaries.values() {
            let registry = summary.package_id.registry.clone();
            let name = summary.package_id.name.clone();
            if filter.registry.as_ref().is_some_and(|r| &registry != r) {
                continue;
            }
            if !filter.registries.is_empty() && !filter.registries.contains(&registry) {
                continue;
            }
            if filter
                .name_contains
                .as_ref()
                .is_some_and(|n| !name.contains(n.as_str()))
            {
                continue;
            }
            // Empty means no restriction, never "match nothing" — the same
            // distinction the Postgres side draws by binding `NULL` rather than
            // an empty array.
            if !filter.name_in.is_empty()
                && !filter
                    .name_in
                    .iter()
                    .any(|(r, n)| r == &registry && n == &name)
            {
                continue;
            }
            let entry = by_package
                .entry((registry.clone(), name.clone()))
                .or_insert_with(|| ExploreEntry {
                    registry,
                    name,
                    version_count: 0,
                    total_downloads: 0,
                    last_accessed: None,
                    source: PackageSource::Proxied,
                    has_blocked: false,
                    has_yanked: false,
                    cached_versions: 0,
                    cached_bytes: None,
                    last_fetched_at: None,
                    newest_version: None,
                    newest_published_at: None,
                });
            entry.version_count += 1;
            entry.has_blocked |= summary.status.is_blocked();
            if summary.last_accessed > entry.last_accessed {
                entry.last_accessed = summary.last_accessed;
            }
        }

        let mut rows: Vec<ExploreEntry> = by_package.into_values().collect();
        match filter.sort_by {
            ExploreSortBy::Name => {
                rows.sort_by(|a, b| (&a.registry, &a.name).cmp(&(&b.registry, &b.name)))
            }
            _ => rows.sort_by(|a, b| {
                b.last_accessed
                    .cmp(&a.last_accessed)
                    .then_with(|| (&a.registry, &a.name).cmp(&(&b.registry, &b.name)))
            }),
        }
        rows
    }
}

#[async_trait]
impl PackageRepository for InMemoryPackageRepository {
    async fn explore_packages(
        &self,
        filter: ExploreFilter,
    ) -> Result<Vec<ExploreEntry>, CoreError> {
        let rows = self.explore_rows(&filter).await;
        let start = (filter.offset as usize).min(rows.len());
        let end = if filter.limit == 0 {
            rows.len()
        } else {
            (start + filter.limit as usize).min(rows.len())
        };
        Ok(rows[start..end].to_vec())
    }

    async fn count_explore_packages(&self, filter: ExploreFilter) -> Result<u64, CoreError> {
        Ok(self.explore_rows(&filter).await.len() as u64)
    }

    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        batlehub_core::services::audit_stream::emit(&event, true);
        // Only actions that always carry a real, version-specific package
        // coordinate should create/update a `PackageSummary` row. Ownership,
        // visibility, and account-wide actions (package_id: None, or Delete
        // which just removed the row) must not spuriously create one — this
        // mirrors the Postgres adapter's `creates_status_row` guard.
        let updates_summary = matches!(
            event.action,
            AccessAction::Download
                | AccessAction::ViewMetadata
                | AccessAction::Block
                | AccessAction::Unblock
        );
        if updates_summary {
            if let Some(pkg) = &event.package_id {
                let mut sums = self.summaries.write().await;
                let entry = sums
                    .entry(pkg.cache_key())
                    .or_insert_with(|| PackageSummary {
                        id: Uuid::new_v4(),
                        package_id: pkg.clone(),
                        status: PackageStatus::Available,
                        last_accessed: None,
                        last_accessed_by: None,
                        access_count: 0,
                    });
                entry.access_count += 1;
                entry.last_accessed = Some(event.timestamp);
                entry.last_accessed_by = event.user_id.clone();
            }
        }
        self.events.write().await.push(event);
        Ok(())
    }

    async fn get_status(&self, pkg: &PackageId) -> Result<PackageStatus, CoreError> {
        Ok(self
            .summaries
            .read()
            .await
            .get(&pkg.cache_key())
            .map(|s| s.status.clone())
            .unwrap_or(PackageStatus::Available))
    }

    /// Overrides the port's default (which pages through `list_packages`) with a
    /// direct scan — the map is already in memory, and the default would build
    /// and sort a whole `PackageSummary` page to read one field off each row.
    async fn blocked_versions(&self, registry: &str, name: &str) -> Result<Vec<String>, CoreError> {
        Ok(self
            .summaries
            .read()
            .await
            .values()
            .filter(|s| {
                s.status.is_blocked()
                    && s.package_id.registry == registry
                    && s.package_id.name == name
            })
            .map(|s| s.package_id.version.clone())
            .collect())
    }

    async fn set_status(&self, pkg: &PackageId, status: PackageStatus) -> Result<(), CoreError> {
        let mut sums = self.summaries.write().await;
        let entry = sums
            .entry(pkg.cache_key())
            .or_insert_with(|| PackageSummary {
                id: Uuid::new_v4(),
                package_id: pkg.clone(),
                status: PackageStatus::Available,
                last_accessed: None,
                last_accessed_by: None,
                access_count: 0,
            });
        entry.status = status;
        Ok(())
    }

    async fn delete_package(&self, pkg: &PackageId) -> Result<bool, CoreError> {
        let removed = self.summaries.write().await.remove(&pkg.cache_key());
        Ok(removed.is_some())
    }

    async fn list_packages(&self, filter: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
        let sums = self.summaries.read().await;
        let mut result: Vec<PackageSummary> = sums
            .values()
            .filter(|s| {
                filter
                    .registry
                    .as_ref()
                    .is_none_or(|r| s.package_id.registry == *r)
                    && (filter.registries.is_empty()
                        || filter.registries.contains(&s.package_id.registry))
                    && filter
                        .name_contains
                        .as_ref()
                        .is_none_or(|n| s.package_id.name.contains(n.as_str()))
                    && filter
                        .name_exact
                        .as_ref()
                        .is_none_or(|n| s.package_id.name == *n)
                    && (!filter.blocked_only || s.status.is_blocked())
            })
            .cloned()
            .collect();

        result.sort_by(|a, b| {
            b.last_accessed
                .unwrap_or(DateTime::<Utc>::MIN_UTC)
                .cmp(&a.last_accessed.unwrap_or(DateTime::<Utc>::MIN_UTC))
        });

        let offset = filter.offset as usize;
        if offset > 0 {
            result = result.into_iter().skip(offset).collect();
        }
        if filter.limit > 0 {
            result.truncate(filter.limit as usize);
        }

        Ok(result)
    }

    async fn count_packages(&self, filter: PackageFilter) -> Result<u64, CoreError> {
        let no_page = PackageFilter {
            limit: 0,
            offset: 0,
            ..filter
        };
        Ok(self.list_packages(no_page).await?.len() as u64)
    }

    async fn list_events(&self, filter: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        let events = self.events.read().await;
        let mut result: Vec<AccessEvent> = events
            .iter()
            .filter(|e| {
                filter
                    .registry
                    .as_ref()
                    .is_none_or(|r| e.package_id.as_ref().is_some_and(|p| p.registry == *r))
                    && filter
                        .package_name
                        .as_ref()
                        .is_none_or(|n| e.package_id.as_ref().is_some_and(|p| p.name == *n))
                    && filter
                        .user_id
                        .as_ref()
                        .is_none_or(|u| e.user_id.as_deref() == Some(u.as_str()))
                    // Empty means every action, matching the SQL adapter's
                    // `NULL` parameter rather than its `ANY('{}')`, which
                    // would match nothing.
                    && (filter.actions.is_empty() || filter.actions.contains(&e.action))
                    && (!filter.denied_only || e.result.is_denied())
                    && filter.from.is_none_or(|from| e.timestamp >= from)
                    && filter.to.is_none_or(|to| e.timestamp <= to)
            })
            .cloned()
            .collect();

        result.sort_by_key(|b| std::cmp::Reverse(b.timestamp));

        let offset = filter.offset as usize;
        if offset > 0 {
            result = result.into_iter().skip(offset).collect();
        }
        if filter.limit > 0 {
            result.truncate(filter.limit as usize);
        }

        Ok(result)
    }

    /// Distinct subjects, most-recently-active first — the same order and the
    /// same case-insensitive substring match the Postgres implementation uses,
    /// so a test written against one holds against the other (RFC 0004-bis A8).
    async fn distinct_event_subjects(
        &self,
        contains: Option<&str>,
        limit: u64,
    ) -> Result<Vec<String>, CoreError> {
        let events = self.events.read().await;
        let needle = contains.map(str::to_lowercase);

        let mut sorted: Vec<&AccessEvent> = events.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.timestamp));

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for event in sorted {
            let Some(user_id) = event.user_id.as_deref() else {
                continue;
            };
            if let Some(ref n) = needle {
                if !user_id.to_lowercase().contains(n.as_str()) {
                    continue;
                }
            }
            if seen.insert(user_id.to_owned()) {
                out.push(user_id.to_owned());
                if limit > 0 && out.len() as u64 >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    async fn list_own_downloads(
        &self,
        user_id: &str,
        since: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        let events = self.events.read().await;
        let mut result: Vec<AccessEvent> = events
            .iter()
            .filter(|e| {
                e.user_id.as_deref() == Some(user_id)
                    && matches!(e.action, AccessAction::Download)
                    && matches!(e.result, AccessResult::Allowed)
                    && e.timestamp >= since
                    && e.package_id.is_some()
            })
            .cloned()
            .collect();
        result.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        if limit > 0 {
            result.truncate(limit as usize);
        }
        Ok(result)
    }

    /// The same three constraints the port states — `Download`, allowed, newest
    /// per version — applied to the event vector.
    ///
    /// The sidecar split does not appear here, and must not: it is drawn at
    /// record time, so a `.sha1` fetch arrives as `ViewMetadata` and is excluded
    /// by the action filter alone. Re-deriving it from `package_id` would make
    /// this double the rule and disagree with Postgres the day the suffix list
    /// changes.
    async fn last_downloads(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>, CoreError> {
        let events = self.events.read().await;
        let mut newest: HashMap<String, DateTime<Utc>> = HashMap::new();
        for event in events.iter() {
            if !matches!(event.action, AccessAction::Download)
                || !matches!(event.result, AccessResult::Allowed)
            {
                continue;
            }
            let Some(pkg) = event.package_id.as_ref() else {
                continue;
            };
            if pkg.registry != registry || pkg.name != package {
                continue;
            }
            newest
                .entry(pkg.version.clone())
                .and_modify(|t| {
                    if event.timestamp > *t {
                        *t = event.timestamp;
                    }
                })
                .or_insert(event.timestamp);
        }
        let mut out: Vec<(String, DateTime<Utc>)> = newest.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    async fn count_events(&self, filter: EventFilter) -> Result<u64, CoreError> {
        let no_page = EventFilter {
            limit: 0,
            offset: 0,
            ..filter
        };
        Ok(self.list_events(no_page).await?.len() as u64)
    }

    async fn registry_package_counts(
        &self,
        registries: &[String],
    ) -> Result<HashMap<String, i64>, CoreError> {
        let sums = self.summaries.read().await;
        let mut counts: HashMap<String, i64> = HashMap::new();
        for s in sums.values() {
            if registries.contains(&s.package_id.registry) {
                *counts.entry(s.package_id.registry.clone()).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    async fn registry_event_stats(
        &self,
        registries: &[String],
    ) -> Result<HashMap<String, (Option<DateTime<Utc>>, i64, i64)>, CoreError> {
        let events = self.events.read().await;
        let now = Utc::now();
        let mut stats: HashMap<String, (Option<DateTime<Utc>>, i64, i64)> = HashMap::new();
        for e in events.iter() {
            if !matches!(e.action, AccessAction::Download)
                || !matches!(e.result, AccessResult::Allowed)
            {
                continue;
            }
            let Some(registry) = e.package_id.as_ref().map(|p| p.registry.clone()) else {
                continue;
            };
            if !registries.contains(&registry) {
                continue;
            }
            let entry = stats.entry(registry).or_insert((None, 0, 0));
            entry.0 = Some(entry.0.map_or(e.timestamp, |cur| cur.max(e.timestamp)));
            if now - e.timestamp <= chrono::Duration::hours(1) {
                entry.1 += 1;
            }
            if now - e.timestamp <= chrono::Duration::days(1) {
                entry.2 += 1;
            }
        }
        Ok(stats)
    }

    async fn recent_registry_errors(
        &self,
        registry: &str,
        limit: i64,
    ) -> Result<Vec<RecentErrorRecord>, CoreError> {
        let events = self.events.read().await;
        let now = Utc::now();
        let mut errors: Vec<RecentErrorRecord> = events
            .iter()
            .filter(|e| {
                e.package_id
                    .as_ref()
                    .is_some_and(|p| p.registry == registry)
                    && matches!(
                        e.result,
                        AccessResult::Denied { .. } | AccessResult::ProxyError { .. }
                    )
                    && now - e.timestamp <= chrono::Duration::hours(24)
            })
            .map(|e| {
                let (outcome, deny_reason) = match &e.result {
                    AccessResult::Denied { reason } => ("denied".to_owned(), Some(reason.clone())),
                    AccessResult::ProxyError { reason } => {
                        ("error".to_owned(), Some(reason.clone()))
                    }
                    AccessResult::Allowed => unreachable!("filtered out above"),
                };
                RecentErrorRecord {
                    created_at: e.timestamp,
                    user_id: e.user_id.clone(),
                    package_name: e
                        .package_id
                        .as_ref()
                        .map(|p| p.name.clone())
                        .unwrap_or_default(),
                    package_version: e
                        .package_id
                        .as_ref()
                        .map(|p| p.version.clone())
                        .unwrap_or_default(),
                    outcome,
                    deny_reason,
                }
            })
            .collect();

        errors.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        if limit >= 0 {
            errors.truncate(limit as usize);
        }
        Ok(errors)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use batlehub_core::{
        entities::{
            AccessAction, AccessEvent, AccessResult, EventFilter, PackageFilter, PackageId,
            PackageStatus, Role,
        },
        ports::PackageRepository,
    };

    use super::InMemoryPackageRepository;

    fn pkg_id(registry: &str, name: &str) -> PackageId {
        PackageId::new(registry, name, "1.0.0")
    }

    fn allow_event(registry: &str, name: &str) -> AccessEvent {
        AccessEvent::allowed_download(pkg_id(registry, name), Some("user".to_owned()), Role::User)
    }

    #[tokio::test]
    async fn get_status_returns_available_for_unknown_package() {
        let repo = InMemoryPackageRepository::new();
        let status = repo.get_status(&pkg_id("reg", "foo")).await.unwrap();
        assert!(matches!(status, PackageStatus::Available));
    }

    #[tokio::test]
    async fn set_then_get_status_round_trips() {
        let repo = InMemoryPackageRepository::new();
        let blocked = PackageStatus::Blocked {
            reason: "test".to_owned(),
            blocked_by: "admin".to_owned(),
            blocked_at: Utc::now(),
        };
        repo.set_status(&pkg_id("reg", "foo"), blocked)
            .await
            .unwrap();
        let status = repo.get_status(&pkg_id("reg", "foo")).await.unwrap();
        assert!(status.is_blocked());
    }

    #[tokio::test]
    async fn record_access_increments_count() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(allow_event("reg", "foo")).await.unwrap();
        repo.record_access(allow_event("reg", "foo")).await.unwrap();

        let pkgs = repo
            .list_packages(PackageFilter {
                registry: Some("reg".to_owned()),
                name_exact: Some("foo".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].access_count, 2);
        assert!(pkgs[0].last_accessed.is_some());
    }

    #[tokio::test]
    async fn list_packages_filters_by_registry() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(allow_event("reg-a", "foo"))
            .await
            .unwrap();
        repo.record_access(allow_event("reg-b", "bar"))
            .await
            .unwrap();

        let result = repo
            .list_packages(PackageFilter {
                registry: Some("reg-a".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].package_id.registry, "reg-a");
    }

    #[tokio::test]
    async fn list_packages_name_contains_filter() {
        let repo = InMemoryPackageRepository::new();
        for name in ["my-lib", "my-app", "other"] {
            repo.record_access(allow_event("reg", name)).await.unwrap();
        }
        let result = repo
            .list_packages(PackageFilter {
                name_contains: Some("my".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.len(), 2);
    }

    #[tokio::test]
    async fn list_packages_pagination() {
        let repo = InMemoryPackageRepository::new();
        for name in ["a", "b", "c", "d", "e"] {
            repo.record_access(allow_event("reg", name)).await.unwrap();
        }

        let page = repo
            .list_packages(PackageFilter {
                limit: 2,
                offset: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.len(), 2);
    }

    #[tokio::test]
    async fn count_packages_matches_unfiltered_total() {
        let repo = InMemoryPackageRepository::new();
        for name in ["a", "b", "c"] {
            repo.record_access(allow_event("reg", name)).await.unwrap();
        }
        let count = repo
            .count_packages(PackageFilter {
                registry: Some("reg".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn list_events_filters_by_package_name() {
        let repo = InMemoryPackageRepository::new();
        for _ in 0..3 {
            repo.record_access(allow_event("reg", "foo")).await.unwrap();
        }
        repo.record_access(allow_event("reg", "bar")).await.unwrap();

        let events = repo
            .list_events(EventFilter {
                registry: Some("reg".to_owned()),
                package_name: Some("foo".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .all(|e| e.package_id.as_ref().is_some_and(|p| p.name == "foo")));
    }

    /// The filter an operator asking "what was deleted" uses: two deletion
    /// kinds at once, and none of the downloads they are buried under.
    #[tokio::test]
    async fn list_events_filters_by_action_set() {
        let repo = InMemoryPackageRepository::new();
        for _ in 0..3 {
            repo.record_access(allow_event("reg", "foo")).await.unwrap();
        }
        for action in [AccessAction::Delete, AccessAction::RetentionReclaim] {
            let mut ev = allow_event("reg", "foo");
            ev.action = action;
            repo.record_access(ev).await.unwrap();
        }

        let events = repo
            .list_events(EventFilter {
                actions: vec![AccessAction::Delete, AccessAction::RetentionReclaim],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(!events.iter().any(|e| e.action == AccessAction::Download));

        let only_policy = repo
            .list_events(EventFilter {
                actions: vec![AccessAction::RetentionReclaim],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(only_policy.len(), 1);
    }

    /// An empty action set must not mean "no actions" — the SQL adapter binds
    /// `NULL` for it, and the two have to agree or the same filter reads
    /// differently against Postgres and in tests.
    #[tokio::test]
    async fn list_events_with_no_action_filter_returns_every_action() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(allow_event("reg", "foo")).await.unwrap();
        let mut deleted = allow_event("reg", "foo");
        deleted.action = AccessAction::Delete;
        repo.record_access(deleted).await.unwrap();

        let events = repo.list_events(EventFilter::new()).await.unwrap();
        assert_eq!(events.len(), 2);
    }

    #[tokio::test]
    async fn list_events_paginates() {
        let repo = InMemoryPackageRepository::new();
        for _ in 0..5 {
            repo.record_access(allow_event("reg", "foo")).await.unwrap();
        }

        let page = repo
            .list_events(EventFilter {
                limit: 2,
                offset: 1,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.len(), 2);
    }

    // ── account-wide events (package_id: None) ────────────────────────────────

    fn account_event(action: AccessAction) -> AccessEvent {
        AccessEvent {
            id: uuid::Uuid::new_v4(),
            user_id: Some("admin".to_owned()),
            user_role: Role::Admin,
            package_id: None,
            action,
            result: batlehub_core::entities::AccessResult::Allowed,
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
            throttled_count: None,
            detail: None,
        }
    }

    #[tokio::test]
    async fn record_access_accepts_account_wide_event_with_no_package() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(account_event(AccessAction::BlockUser))
            .await
            .unwrap();

        let events = repo.list_events(EventFilter::new()).await.unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].package_id.is_none());
        assert!(matches!(events[0].action, AccessAction::BlockUser));
    }

    #[tokio::test]
    async fn account_wide_event_does_not_create_a_package_summary() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(account_event(AccessAction::BlockIp))
            .await
            .unwrap();

        let pkgs = repo.list_packages(PackageFilter::new()).await.unwrap();
        assert!(
            pkgs.is_empty(),
            "an account-wide event must not fabricate a package_statuses row"
        );
    }

    #[tokio::test]
    async fn list_events_with_registry_filter_excludes_account_wide_events() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(allow_event("reg", "foo")).await.unwrap();
        repo.record_access(account_event(AccessAction::UnblockUser))
            .await
            .unwrap();

        let events = repo
            .list_events(EventFilter {
                registry: Some("reg".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            events.len(),
            1,
            "account-wide event has no registry to match"
        );
        assert!(events[0].package_id.is_some());
    }

    // ── list_own_downloads (RFC 0004 §6.2) ────────────────────────────────────

    fn download_by(user: &str, name: &str, ago_secs: i64) -> AccessEvent {
        let mut e =
            AccessEvent::allowed_download(pkg_id("reg", name), Some(user.to_owned()), Role::User);
        e.timestamp = Utc::now() - chrono::Duration::seconds(ago_secs);
        e
    }

    #[tokio::test]
    async fn list_own_downloads_excludes_other_users() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(download_by("alice", "mine", 5))
            .await
            .unwrap();
        repo.record_access(download_by("bob", "theirs", 5))
            .await
            .unwrap();

        let rows = repo
            .list_own_downloads("alice", Utc::now() - chrono::Duration::days(1), 10)
            .await
            .unwrap();
        let names: Vec<&str> = rows
            .iter()
            .map(|e| e.package_id.as_ref().unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["mine"], "the port scopes, not the caller");
    }

    #[tokio::test]
    async fn list_own_downloads_excludes_denied_and_other_actions() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(download_by("alice", "ok", 5))
            .await
            .unwrap();

        let mut denied = download_by("alice", "denied", 4);
        denied.result = AccessResult::Denied {
            reason: "blocked".to_owned(),
        };
        repo.record_access(denied).await.unwrap();

        let mut viewed = download_by("alice", "viewed", 3);
        viewed.action = AccessAction::ViewMetadata;
        repo.record_access(viewed).await.unwrap();

        let rows = repo
            .list_own_downloads("alice", Utc::now() - chrono::Duration::days(1), 10)
            .await
            .unwrap();
        let names: Vec<&str> = rows
            .iter()
            .map(|e| e.package_id.as_ref().unwrap().name.as_str())
            .collect();
        assert_eq!(names, vec!["ok"]);
    }

    #[tokio::test]
    async fn list_own_downloads_honours_window_and_limit_newest_first() {
        let repo = InMemoryPackageRepository::new();
        repo.record_access(download_by("alice", "newest", 1))
            .await
            .unwrap();
        repo.record_access(download_by("alice", "older", 60))
            .await
            .unwrap();
        repo.record_access(download_by("alice", "ancient", 10 * 86_400))
            .await
            .unwrap();

        let since = Utc::now() - chrono::Duration::days(7);
        let rows = repo.list_own_downloads("alice", since, 10).await.unwrap();
        let names: Vec<&str> = rows
            .iter()
            .map(|e| e.package_id.as_ref().unwrap().name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["newest", "older"],
            "10 days back is outside a 7-day window"
        );

        let capped = repo.list_own_downloads("alice", since, 1).await.unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(
            capped[0].package_id.as_ref().unwrap().name,
            "newest",
            "the limit keeps the newest, not an arbitrary row"
        );
    }
}
