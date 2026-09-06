use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;

use super::*;
use crate::entities::{
    AccessEvent, AccessResult, EventFilter, Identity, PackageFilter, PackageId, PackageStatus,
    PackageSummary, Role,
};
use crate::error::CoreError;
use crate::ports::PackageRepository;

struct MemRepo {
    statuses: Mutex<HashMap<String, PackageStatus>>,
    events: Mutex<Vec<AccessEvent>>,
}

impl MemRepo {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(HashMap::new()),
            events: Mutex::new(vec![]),
        })
    }

    fn events(&self) -> Vec<AccessEvent> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl PackageRepository for MemRepo {
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
    async fn get_status(&self, pkg: &PackageId) -> Result<PackageStatus, CoreError> {
        Ok(self
            .statuses
            .lock()
            .unwrap()
            .get(&pkg.cache_key())
            .cloned()
            .unwrap_or(PackageStatus::Available))
    }
    async fn set_status(&self, pkg: &PackageId, status: PackageStatus) -> Result<(), CoreError> {
        self.statuses
            .lock()
            .unwrap()
            .insert(pkg.cache_key(), status);
        Ok(())
    }
    async fn list_packages(&self, _f: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(&self, _f: PackageFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(&self, _f: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(self.events.lock().unwrap().clone())
    }
    async fn count_events(&self, _f: EventFilter) -> Result<u64, CoreError> {
        Ok(self.events.lock().unwrap().len() as u64)
    }
    /// Mirrors the real adapters' contract: this caller only, allowed
    /// downloads only, newest first (RFC 0004 §6.2).
    async fn list_own_downloads(
        &self,
        user_id: &str,
        since: chrono::DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        let mut rows: Vec<AccessEvent> = self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                e.user_id.as_deref() == Some(user_id)
                    && matches!(e.action, crate::entities::AccessAction::Download)
                    && matches!(e.result, AccessResult::Allowed)
                    && e.timestamp >= since
                    && e.package_id.is_some()
            })
            .cloned()
            .collect();
        rows.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        if limit > 0 {
            rows.truncate(limit as usize);
        }
        Ok(rows)
    }
    async fn delete_package(&self, pkg: &PackageId) -> Result<bool, CoreError> {
        Ok(self
            .statuses
            .lock()
            .unwrap()
            .remove(&pkg.cache_key())
            .is_some())
    }
}

fn admin_identity(user_id: &str) -> Identity {
    Identity {
        user_id: Some(user_id.to_owned()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}

fn anon_identity() -> Identity {
    Identity::anonymous()
}

#[tokio::test]
async fn block_package_sets_blocked_status() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "evil", "1.0.0");

    svc.block_package(
        &pkg,
        "supply chain risk".to_owned(),
        &admin_identity("alice"),
    )
    .await
    .unwrap();

    let status = repo.get_status(&pkg).await.unwrap();
    assert!(status.is_blocked());
}

#[tokio::test]
async fn block_package_uses_user_id_as_blocked_by() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "evil", "1.0.0");

    svc.block_package(&pkg, "reason".to_owned(), &admin_identity("alice"))
        .await
        .unwrap();

    let status = repo.get_status(&pkg).await.unwrap();
    match status {
        PackageStatus::Blocked { blocked_by, .. } => assert_eq!(blocked_by, "alice"),
        PackageStatus::Available => panic!("expected Blocked"),
    }
}

#[tokio::test]
async fn block_package_falls_back_to_role_when_no_user_id() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "evil", "1.0.0");

    svc.block_package(&pkg, "reason".to_owned(), &anon_identity())
        .await
        .unwrap();

    let status = repo.get_status(&pkg).await.unwrap();
    match status {
        PackageStatus::Blocked { blocked_by, .. } => assert_eq!(blocked_by, "anonymous"),
        PackageStatus::Available => panic!("expected Blocked"),
    }
}

#[tokio::test]
async fn block_package_records_audit_event() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "evil", "1.0.0");

    svc.block_package(&pkg, "reason".to_owned(), &admin_identity("alice"))
        .await
        .unwrap();

    let events = repo.events();
    assert_eq!(events.len(), 1, "one event expected");
    assert!(matches!(events[0].result, AccessResult::Allowed));
}

#[tokio::test]
async fn record_package_action_records_package_scoped_event() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "left-pad", "1.0.0");

    svc.record_package_action(
        &pkg,
        crate::entities::AccessAction::AddOwner,
        &admin_identity("alice"),
    )
    .await;

    let events = repo.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].package_id, Some(pkg));
    assert!(matches!(
        events[0].action,
        crate::entities::AccessAction::AddOwner
    ));
    assert!(matches!(events[0].result, AccessResult::Allowed));
}

#[tokio::test]
async fn record_account_action_records_event_with_no_package() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());

    svc.record_account_action(
        crate::entities::AccessAction::BlockUser,
        &admin_identity("alice"),
    )
    .await;

    let events = repo.events();
    assert_eq!(events.len(), 1);
    assert!(events[0].package_id.is_none());
    assert!(matches!(
        events[0].action,
        crate::entities::AccessAction::BlockUser
    ));
}

#[tokio::test]
async fn purge_events_before_records_audit_purge_event() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());

    svc.purge_events_before(Utc::now(), &admin_identity("alice"))
        .await
        .unwrap();

    let events = repo.events();
    assert_eq!(events.len(), 1, "purge itself must leave an audit trail");
    assert!(events[0].package_id.is_none());
    assert_eq!(events[0].user_id.as_deref(), Some("alice"));
    assert!(matches!(events[0].action, AccessAction::AuditPurge));
    assert!(matches!(events[0].result, AccessResult::Allowed));
}

#[tokio::test]
async fn unblock_package_sets_available_status() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "evil", "1.0.0");

    // Block first, then unblock
    svc.block_package(&pkg, "r".to_owned(), &admin_identity("a"))
        .await
        .unwrap();
    svc.unblock_package(&pkg, &admin_identity("a"))
        .await
        .unwrap();

    let status = repo.get_status(&pkg).await.unwrap();
    assert!(!status.is_blocked());
}

#[tokio::test]
async fn unblock_package_records_audit_event() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "evil", "1.0.0");

    svc.block_package(&pkg, "r".to_owned(), &admin_identity("a"))
        .await
        .unwrap();
    svc.unblock_package(&pkg, &admin_identity("a"))
        .await
        .unwrap();

    let events = repo.events();
    assert_eq!(events.len(), 2, "block + unblock events expected");
}

#[tokio::test]
async fn get_package_status_delegates_to_repo() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "serde", "1.0.0");

    // Fresh package defaults to Available
    let status = svc.get_package_status(&pkg).await.unwrap();
    assert!(!status.is_blocked());
}

#[tokio::test]
async fn list_packages_returns_repo_results() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());

    let result = svc.list_packages(PackageFilter::new()).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn list_events_returns_repo_results() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg = PackageId::new("npm", "pkg", "1.0.0");

    svc.block_package(&pkg, "r".to_owned(), &admin_identity("a"))
        .await
        .unwrap();

    let events = svc.list_events(EventFilter::new()).await.unwrap();
    assert!(!events.is_empty());
}

#[tokio::test]
async fn bulk_block_succeeds_for_all_valid_packages() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let items = vec![
        BulkBlockItem {
            package_id: PackageId::new("npm", "a", "1.0.0"),
            reason: "r1".into(),
        },
        BulkBlockItem {
            package_id: PackageId::new("npm", "b", "2.0.0"),
            reason: "r2".into(),
        },
    ];
    let result = svc
        .bulk_block_packages(items, &admin_identity("alice"))
        .await;
    assert_eq!(result.succeeded.len(), 2);
    assert_eq!(result.failed.len(), 0);
    assert!(repo
        .get_status(&PackageId::new("npm", "a", "1.0.0"))
        .await
        .unwrap()
        .is_blocked());
    assert!(repo
        .get_status(&PackageId::new("npm", "b", "2.0.0"))
        .await
        .unwrap()
        .is_blocked());
}

#[tokio::test]
async fn bulk_delete_reports_success_and_not_found() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg_a = PackageId::new("npm", "a", "1.0.0");
    let pkg_missing = PackageId::new("npm", "missing", "1.0.0");
    svc.block_package(&pkg_a, "r".into(), &admin_identity("alice"))
        .await
        .unwrap();

    let result = svc
        .bulk_delete_packages(
            vec![pkg_a.clone(), pkg_missing.clone()],
            &admin_identity("alice"),
        )
        .await;
    assert_eq!(result.succeeded, vec![pkg_a]);
    assert_eq!(result.failed.len(), 1);
    assert_eq!(result.failed[0].0, pkg_missing);
    assert_eq!(result.failed[0].1, "package not found");
}

#[tokio::test]
async fn bulk_block_handles_more_items_than_the_concurrency_cap() {
    // BULK_ACTION_CONCURRENCY is 16; exercise a batch larger than that to
    // confirm the bounded-concurrency stream still processes every item
    // (not just the first `buffer_unordered` window).
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let items: Vec<BulkBlockItem> = (0..40)
        .map(|i| BulkBlockItem {
            package_id: PackageId::new("npm", format!("pkg-{i}"), "1.0.0"),
            reason: "bulk".into(),
        })
        .collect();

    let result = svc
        .bulk_block_packages(items, &admin_identity("alice"))
        .await;
    assert_eq!(result.succeeded.len(), 40);
    assert_eq!(result.failed.len(), 0);
    for i in 0..40 {
        assert!(repo
            .get_status(&PackageId::new("npm", format!("pkg-{i}"), "1.0.0"))
            .await
            .unwrap()
            .is_blocked());
    }
}

#[tokio::test]
async fn bulk_unblock_succeeds_for_all_packages() {
    let repo = MemRepo::new();
    let svc = AdminService::new(repo.clone());
    let pkg_a = PackageId::new("npm", "a", "1.0.0");
    let pkg_b = PackageId::new("npm", "b", "2.0.0");
    svc.block_package(&pkg_a, "r".into(), &admin_identity("alice"))
        .await
        .unwrap();
    svc.block_package(&pkg_b, "r".into(), &admin_identity("alice"))
        .await
        .unwrap();

    let result = svc
        .bulk_unblock_packages(vec![pkg_a.clone(), pkg_b.clone()], &admin_identity("alice"))
        .await;
    assert_eq!(result.succeeded.len(), 2);
    assert_eq!(result.failed.len(), 0);
    assert!(!repo.get_status(&pkg_a).await.unwrap().is_blocked());
    assert!(!repo.get_status(&pkg_b).await.unwrap().is_blocked());
}

// ── explore cache integration ─────────────────────────────────────────────

use std::time::Duration;

use crate::entities::{ExploreEntry, ExploreFilter, PackageSource, RegistryStat};
use crate::services::explore_cache::ExploreCache;

/// A repo that always returns a fixed set of explore results.
struct StubExploreRepo {
    entries: Vec<ExploreEntry>,
    count: u64,
    stats: Vec<RegistryStat>,
}

impl StubExploreRepo {
    fn arc(entries: Vec<ExploreEntry>, count: u64, stats: Vec<RegistryStat>) -> Arc<Self> {
        Arc::new(Self {
            entries,
            count,
            stats,
        })
    }
}

#[async_trait]
impl PackageRepository for StubExploreRepo {
    async fn record_access(&self, _: AccessEvent) -> Result<(), CoreError> {
        Ok(())
    }
    async fn get_status(&self, _: &PackageId) -> Result<PackageStatus, CoreError> {
        Ok(PackageStatus::Available)
    }
    async fn set_status(&self, _: &PackageId, _: PackageStatus) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_packages(&self, _: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(&self, _: PackageFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(&self, _: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(vec![])
    }
    async fn count_events(&self, _: EventFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn explore_packages(&self, _: ExploreFilter) -> Result<Vec<ExploreEntry>, CoreError> {
        Ok(self.entries.clone())
    }
    async fn count_explore_packages(&self, _: ExploreFilter) -> Result<u64, CoreError> {
        Ok(self.count)
    }
    async fn registry_explore_stats(
        &self,
        _: &[String],
        _: &crate::entities::ExploreViewer,
    ) -> Result<Vec<RegistryStat>, CoreError> {
        Ok(self.stats.clone())
    }
    async fn delete_package(&self, _: &PackageId) -> Result<bool, CoreError> {
        Ok(false)
    }
}

/// A repo whose explore methods always return an error.
struct FailingExploreRepo;

impl FailingExploreRepo {
    fn arc() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl PackageRepository for FailingExploreRepo {
    async fn record_access(&self, _: AccessEvent) -> Result<(), CoreError> {
        Ok(())
    }
    async fn get_status(&self, _: &PackageId) -> Result<PackageStatus, CoreError> {
        Ok(PackageStatus::Available)
    }
    async fn set_status(&self, _: &PackageId, _: PackageStatus) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_packages(&self, _: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(&self, _: PackageFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(&self, _: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(vec![])
    }
    async fn count_events(&self, _: EventFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn explore_packages(&self, _: ExploreFilter) -> Result<Vec<ExploreEntry>, CoreError> {
        Err(CoreError::Database("simulated failure".into()))
    }
    async fn count_explore_packages(&self, _: ExploreFilter) -> Result<u64, CoreError> {
        Err(CoreError::Database("simulated failure".into()))
    }
    async fn registry_explore_stats(
        &self,
        _: &[String],
        _: &crate::entities::ExploreViewer,
    ) -> Result<Vec<RegistryStat>, CoreError> {
        Err(CoreError::Database("simulated failure".into()))
    }
    async fn delete_package(&self, _: &PackageId) -> Result<bool, CoreError> {
        Ok(false)
    }
}

fn sample_entry() -> ExploreEntry {
    ExploreEntry {
        registry: "npm".into(),
        name: "lodash".into(),
        version_count: 1,
        total_downloads: 10,
        last_accessed: None,
        source: PackageSource::Proxied,
        has_blocked: false,
        ..Default::default()
    }
}

fn sample_stat() -> RegistryStat {
    RegistryStat {
        registry: "npm".into(),
        package_count: 1,
        total_downloads: 10,
        ..Default::default()
    }
}

fn default_filter() -> ExploreFilter {
    ExploreFilter {
        registry: Some("npm".into()),
        registries: vec![],
        ..ExploreFilter::default()
    }
}

fn make_svc_with_cache(repo: Arc<dyn PackageRepository>, cache: Arc<ExploreCache>) -> AdminService {
    AdminService {
        repo,
        explore_cache: cache,
        vuln_repo: None,
        hot: None,
    }
}

// ── explore_packages ──────────────────────────────────────────────────────

#[tokio::test]
async fn explore_packages_cache_hit_skips_repo() {
    let cache = Arc::new(ExploreCache::new());
    let filter = default_filter();
    // Pre-warm the cache with known data
    cache
        .set_packages("pre", vec![sample_entry()], 1, vec!["npm".into()])
        .await;

    // Use the same key the service would compute
    let key = format!(
        "items:{}",
        crate::services::explore_cache::packages_cache_key(&filter)
    );
    cache
        .set_packages(&key, vec![sample_entry()], 1, vec!["npm".into()])
        .await;

    // FailingRepo would return Err, but the cache hit prevents it from being called
    let svc = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (items, unavailable) = svc.explore_packages(filter).await.unwrap();
    assert!(!unavailable);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "lodash");
}

#[tokio::test]
async fn explore_packages_cache_miss_queries_repo_and_caches() {
    let cache = Arc::new(ExploreCache::new());
    let filter = default_filter();
    let repo = StubExploreRepo::arc(vec![sample_entry()], 1, vec![]);
    let svc = make_svc_with_cache(repo, Arc::clone(&cache));

    let (items, unavailable) = svc.explore_packages(filter.clone()).await.unwrap();
    assert!(!unavailable);
    assert_eq!(items.len(), 1);

    // A second call with a failing repo should now be served from cache
    let svc2 = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (cached_items, unavailable2) = svc2.explore_packages(filter).await.unwrap();
    assert!(!unavailable2);
    assert_eq!(cached_items.len(), 1);
}

#[tokio::test]
async fn explore_packages_db_fail_stale_exists_serves_stale() {
    // Use 0-TTL cache so the entry is immediately stale
    let cache = Arc::new(ExploreCache::with_ttl(Duration::ZERO));
    let filter = default_filter();
    let key = format!(
        "items:{}",
        crate::services::explore_cache::packages_cache_key(&filter)
    );
    cache
        .set_packages(&key, vec![sample_entry()], 1, vec!["npm".into()])
        .await;

    let svc = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (items, unavailable) = svc.explore_packages(filter).await.unwrap();
    // Stale served → upstream_unavailable must be false
    assert!(!unavailable);
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn explore_packages_db_fail_no_cache_returns_unavailable() {
    let cache = Arc::new(ExploreCache::new());
    let svc = make_svc_with_cache(FailingExploreRepo::arc(), cache);
    let (items, unavailable) = svc.explore_packages(default_filter()).await.unwrap();
    assert!(unavailable);
    assert!(items.is_empty());
}

// ── count_explore_packages ────────────────────────────────────────────────

#[tokio::test]
async fn count_explore_packages_cache_hit_skips_repo() {
    let cache = Arc::new(ExploreCache::new());
    let filter = default_filter();
    let key = format!(
        "count:{}",
        crate::services::explore_cache::packages_cache_key(&filter)
    );
    // set_packages stores the count too
    cache
        .set_packages(&key, vec![], 42, vec!["npm".into()])
        .await;

    let svc = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (count, unavailable) = svc.count_explore_packages(filter).await.unwrap();
    assert!(!unavailable);
    assert_eq!(count, 42);
}

#[tokio::test]
async fn count_explore_packages_db_fail_no_cache_returns_unavailable() {
    let cache = Arc::new(ExploreCache::new());
    let svc = make_svc_with_cache(FailingExploreRepo::arc(), cache);
    let (count, unavailable) = svc.count_explore_packages(default_filter()).await.unwrap();
    assert!(unavailable);
    assert_eq!(count, 0);
}

#[tokio::test]
async fn count_explore_packages_db_fail_stale_returns_count() {
    let cache = Arc::new(ExploreCache::with_ttl(Duration::ZERO));
    let filter = default_filter();
    let key = format!(
        "count:{}",
        crate::services::explore_cache::packages_cache_key(&filter)
    );
    cache
        .set_packages(&key, vec![], 7, vec!["npm".into()])
        .await;

    let svc = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (count, unavailable) = svc.count_explore_packages(filter).await.unwrap();
    assert!(!unavailable);
    assert_eq!(count, 7);
}

#[tokio::test]
async fn explore_and_count_do_not_clobber_each_other_with_matching_filters() {
    // Same limit/offset (as would happen if a caller ever passed `per_page=0`
    // to both the items and the count query) must not let one call's cached
    // write stomp the other's, since each only carries real data for the
    // half it computed (items+0, or empty-items+count).
    let cache = Arc::new(ExploreCache::new());
    let filter = ExploreFilter {
        registry: Some("npm".into()),
        registries: vec![],
        limit: 0,
        offset: 0,
        ..ExploreFilter::default()
    };
    let repo = StubExploreRepo::arc(vec![sample_entry()], 42, vec![]);
    let svc = make_svc_with_cache(repo, Arc::clone(&cache));

    let (items, items_unavailable) = svc.explore_packages(filter.clone()).await.unwrap();
    let (count, count_unavailable) = svc.count_explore_packages(filter.clone()).await.unwrap();
    assert!(!items_unavailable && !count_unavailable);
    assert_eq!(items.len(), 1);
    assert_eq!(count, 42);

    // Re-read both from cache (repo now fails) — neither should have been
    // poisoned by the other's write.
    let svc2 = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (cached_items, _) = svc2.explore_packages(filter.clone()).await.unwrap();
    let (cached_count, _) = svc2.count_explore_packages(filter).await.unwrap();
    assert_eq!(
        cached_items.len(),
        1,
        "items must not be clobbered by the count write"
    );
    assert_eq!(
        cached_count, 42,
        "count must not be clobbered by the items write"
    );
}

// ── registry_explore_stats ────────────────────────────────────────────────

#[tokio::test]
async fn registry_explore_stats_cache_hit_skips_repo() {
    let cache = Arc::new(ExploreCache::new());
    let regs = vec!["npm".to_string()];
    let key = crate::services::explore_cache::stats_cache_key(&regs, &Default::default());
    cache
        .set_stats(&key, vec![sample_stat()], regs.clone())
        .await;

    let svc = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (stats, unavailable) = svc
        .registry_explore_stats(&regs, &Default::default())
        .await
        .unwrap();
    assert!(!unavailable);
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].registry, "npm");
}

#[tokio::test]
async fn registry_explore_stats_cache_miss_queries_repo_and_caches() {
    let cache = Arc::new(ExploreCache::new());
    let regs = vec!["npm".to_string()];
    let repo = StubExploreRepo::arc(vec![], 0, vec![sample_stat()]);
    let svc = make_svc_with_cache(repo, Arc::clone(&cache));

    let (stats, unavailable) = svc
        .registry_explore_stats(&regs, &Default::default())
        .await
        .unwrap();
    assert!(!unavailable);
    assert_eq!(stats.len(), 1);

    // Second call with failing repo should serve from cache
    let svc2 = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (cached, _) = svc2
        .registry_explore_stats(&regs, &Default::default())
        .await
        .unwrap();
    assert_eq!(cached.len(), 1);
}

#[tokio::test]
async fn registry_explore_stats_db_fail_stale_serves_stale() {
    let cache = Arc::new(ExploreCache::with_ttl(Duration::ZERO));
    let regs = vec!["npm".to_string()];
    let key = crate::services::explore_cache::stats_cache_key(&regs, &Default::default());
    cache
        .set_stats(&key, vec![sample_stat()], regs.clone())
        .await;

    let svc = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (stats, unavailable) = svc
        .registry_explore_stats(&regs, &Default::default())
        .await
        .unwrap();
    assert!(!unavailable);
    assert_eq!(stats.len(), 1);
}

#[tokio::test]
async fn registry_explore_stats_db_fail_no_cache_returns_unavailable() {
    let cache = Arc::new(ExploreCache::new());
    let svc = make_svc_with_cache(FailingExploreRepo::arc(), cache);
    let (stats, unavailable) = svc
        .registry_explore_stats(&["npm".into()], &Default::default())
        .await
        .unwrap();
    assert!(unavailable);
    assert!(stats.is_empty());
}

// ── cache invalidation on admin action ────────────────────────────────────

#[tokio::test]
async fn explore_cache_invalidation_clears_subsequent_reads() {
    let cache = Arc::new(ExploreCache::new());
    let filter = default_filter();
    let key = format!(
        "items:{}",
        crate::services::explore_cache::packages_cache_key(&filter)
    );
    cache
        .set_packages(&key, vec![sample_entry()], 1, vec!["npm".into()])
        .await;

    // Invalidate npm
    cache.invalidate(Some("npm")).await;

    // Now the failing repo controls the response → unavailable
    let svc = make_svc_with_cache(FailingExploreRepo::arc(), Arc::clone(&cache));
    let (_, unavailable) = svc.explore_packages(filter).await.unwrap();
    assert!(unavailable);
}

// ── list_vulnerabilities ──────────────────────────────────────────────────

struct OneFindingVulnRepo;

#[async_trait]
impl VulnerabilityRepository for OneFindingVulnRepo {
    async fn replace_findings_for_artifact(
        &self,
        _artifact_key: &str,
        _findings: Vec<crate::entities::ArtifactVulnerability>,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_for_coordinate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Vec<crate::entities::ArtifactVulnerability>, CoreError> {
        Ok(vec![crate::entities::ArtifactVulnerability {
            id: uuid::Uuid::new_v4(),
            artifact_key: format!("artifact:{registry}/{name}/{version}"),
            registry: registry.to_owned(),
            package_name: name.to_owned(),
            version: version.to_owned(),
            osv_id: "RUSTSEC-2021-0001".to_owned(),
            severity: crate::entities::Severity::High,
            summary: "boom".to_owned(),
            fixed_version: Some("0.3.1".to_owned()),
            purl: format!("pkg:cargo/{name}@{version}"),
            detected_at: Utc::now(),
        }])
    }
}

#[tokio::test]
async fn list_vulnerabilities_empty_without_repo() {
    let svc = AdminService::new(MemRepo::new());
    let out = svc
        .list_vulnerabilities("cargo", "yaml", "0.3.0")
        .await
        .unwrap();
    assert!(out.is_empty(), "no vuln repo attached → empty");
}

#[tokio::test]
async fn list_vulnerabilities_delegates_to_repo() {
    let svc =
        AdminService::new(MemRepo::new()).with_vulnerability_repo(Arc::new(OneFindingVulnRepo));
    let out = svc
        .list_vulnerabilities("cargo", "yaml", "0.3.0")
        .await
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].osv_id, "RUSTSEC-2021-0001");
    assert_eq!(out[0].fixed_version.as_deref(), Some("0.3.1"));
}

// ── recent_own_coordinates: the R6/R15 collapse rule ─────────────────────────

fn seed_pull(repo: &Arc<MemRepo>, user: &str, name: &str, version: &str, ago_secs: i64) {
    let mut event = AccessEvent::allowed_download(
        PackageId::new("npm", name, version),
        Some(user.to_owned()),
        Role::User,
    );
    event.timestamp = Utc::now() - chrono::Duration::seconds(ago_secs);
    repo.events.lock().unwrap().push(event);
}

#[tokio::test]
async fn recent_own_coordinates_collapses_repeated_pulls_of_one_version() {
    let repo = MemRepo::new();
    // A CI job with a warm lockfile: the same version, twenty times.
    for i in 0..20 {
        seed_pull(&repo, "alice", "busy", "1.0.0", 100 - i);
    }
    seed_pull(&repo, "alice", "other", "2.0.0", 500);

    let svc = AdminService::new(repo);
    let out = svc
        .recent_own_coordinates("alice", Utc::now() - chrono::Duration::days(7), 5)
        .await
        .unwrap();

    let rows: Vec<(&str, &str)> = out
        .iter()
        .map(|(p, _)| (p.name.as_str(), p.version.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![("busy", "1.0.0"), ("other", "2.0.0")],
        "twenty pulls of one version are one coordinate"
    );
}

#[tokio::test]
async fn recent_own_coordinates_keeps_each_version_as_its_own_row() {
    let repo = MemRepo::new();
    // Three versions of one package: three coordinates, not one (RFC 0004 R15).
    // An advisory is a fact about a version, so the version has to survive.
    for i in 0..3 {
        seed_pull(&repo, "alice", "lib", &format!("1.0.{i}"), 100 - i);
    }

    let svc = AdminService::new(repo);
    let out = svc
        .recent_own_coordinates("alice", Utc::now() - chrono::Duration::days(7), 5)
        .await
        .unwrap();

    let versions: Vec<&str> = out.iter().map(|(p, _)| p.version.as_str()).collect();
    assert_eq!(
        versions,
        vec!["1.0.2", "1.0.1", "1.0.0"],
        "distinct versions stay distinct, newest first"
    );
}

#[tokio::test]
async fn recent_own_coordinates_drops_the_artifact_from_the_key() {
    let repo = MemRepo::new();
    // The same version fetched as two different files within it. Findings are
    // recorded per coordinate, so these must not become two rows.
    for artifact in ["tarball", "metadata"] {
        let mut event = AccessEvent::allowed_download(
            PackageId::new("npm", "lib", "1.0.0").with_artifact(artifact),
            Some("alice".to_owned()),
            Role::User,
        );
        event.timestamp = Utc::now() - chrono::Duration::seconds(10);
        repo.events.lock().unwrap().push(event);
    }

    let svc = AdminService::new(repo);
    let out = svc
        .recent_own_coordinates("alice", Utc::now() - chrono::Duration::days(7), 5)
        .await
        .unwrap();

    assert_eq!(out.len(), 1, "one version is one coordinate");
    assert_eq!(
        out[0].0.artifact, None,
        "the artifact is not part of a coordinate the findings table knows about"
    );
}

#[tokio::test]
async fn recent_own_coordinates_caps_at_max_and_excludes_other_users() {
    let repo = MemRepo::new();
    for i in 0..8 {
        seed_pull(&repo, "alice", &format!("pkg-{i}"), "1.0.0", 100 - i);
    }
    seed_pull(&repo, "bob", "bobs-pkg", "1.0.0", 1);

    let svc = AdminService::new(repo);
    let out = svc
        .recent_own_coordinates("alice", Utc::now() - chrono::Duration::days(7), 5)
        .await
        .unwrap();

    assert_eq!(out.len(), 5, "capped at max_coordinates");
    let names: Vec<&str> = out.iter().map(|(p, _)| p.name.as_str()).collect();
    assert!(
        !names.contains(&"bobs-pkg"),
        "another user's pull is not the caller's, got {names:?}"
    );
}

#[tokio::test]
async fn recent_own_coordinates_zero_max_reads_nothing() {
    let repo = MemRepo::new();
    seed_pull(&repo, "alice", "pkg", "1.0.0", 1);
    let svc = AdminService::new(repo);
    let out = svc
        .recent_own_coordinates("alice", Utc::now() - chrono::Duration::days(7), 0)
        .await
        .unwrap();
    assert!(out.is_empty());
}
