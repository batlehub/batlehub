//! RFC 0016 §10 — the retention half of the test plan.
//!
//! These tests are about *which versions survive*, which is decided entirely by
//! [`RetentionService::decide`] and the data gathered for it. They run against
//! in-memory doubles because the decision is arithmetic over a policy and three
//! facts about a version, and none of it is SQL. What is not decided here — that
//! a reclamation leaves a tombstone, drops the bytes and spends the coordinate —
//! belongs to `delete_version` and is asserted in `crates/web/tests/tombstones.rs`
//! and `crates/adapters/tests/pg_tombstones.rs`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};

use super::*;
use crate::entities::{
    AccessAction, AccessEvent, AccessResult, Identity, PackageFilter, PackageId, PackageStatus,
    PackageSummary, Role, Visibility,
};
use crate::ports::{
    LocalRegistryBackend, PackageRepository, StorageBackend, StorageMeta, StoredArtifact,
};
use crate::services::{new_hot_lock, HotConfig, LocalRegistryService};
use bytes::Bytes;

// ── Doubles ───────────────────────────────────────────────────────────────────

struct NoopStorage;

#[async_trait]
impl StorageBackend for NoopStorage {
    async fn store(&self, _: &str, _: Bytes, _: StorageMeta) -> Result<(), CoreError> {
        Ok(())
    }
    async fn retrieve(&self, _: &str) -> Result<Option<StoredArtifact>, CoreError> {
        Ok(None)
    }
    async fn exists(&self, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete(&self, _: &str) -> Result<bool, CoreError> {
        Ok(true)
    }
    async fn delete_by_prefix(&self, _: &str) -> Result<usize, CoreError> {
        Ok(0)
    }
    async fn stat_by_prefix(&self, _: &str) -> Result<(u64, u64), CoreError> {
        Ok((0, 0))
    }
    async fn list_keys(&self, _: &str) -> Result<Vec<String>, CoreError> {
        Ok(vec![])
    }
}

/// A package repository that answers only `last_downloads`, from events the test
/// records through the real `record_access` path.
///
/// Recording real [`AccessEvent`]s rather than seeding a version→timestamp map
/// is the point of the sidecar tests below: the `Download`/`ViewMetadata` split
/// is drawn by the *caller* of `record_access`, so a double that took the answer
/// directly would assert nothing about it.
#[derive(Default)]
struct EventRepo {
    events: tokio::sync::RwLock<Vec<AccessEvent>>,
}

impl EventRepo {
    /// Record one access exactly as the download path does: the action is chosen
    /// by `is_verification_sidecar`, not by the test.
    async fn record(&self, pkg: PackageId, at: DateTime<Utc>) {
        let action = if pkg.is_verification_sidecar() {
            AccessAction::ViewMetadata
        } else {
            AccessAction::Download
        };
        self.events.write().await.push(AccessEvent {
            id: uuid::Uuid::new_v4(),
            user_id: Some("user-1".to_owned()),
            user_role: Role::User,
            package_id: Some(pkg),
            action,
            result: AccessResult::Allowed,
            timestamp: at,
            ip_address: None,
            user_agent: None,
            throttled_count: None,
            detail: None,
        });
    }

    async fn record_denied(&self, pkg: PackageId, at: DateTime<Utc>) {
        self.events.write().await.push(AccessEvent {
            id: uuid::Uuid::new_v4(),
            user_id: Some("user-1".to_owned()),
            user_role: Role::User,
            package_id: Some(pkg),
            action: AccessAction::Download,
            result: AccessResult::Denied {
                reason: "blocked".to_owned(),
            },
            timestamp: at,
            ip_address: None,
            user_agent: None,
            throttled_count: None,
            detail: None,
        });
    }
}

#[async_trait]
impl PackageRepository for EventRepo {
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        self.events.write().await.push(event);
        Ok(())
    }
    async fn list_packages(&self, _: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(&self, _: PackageFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(
        &self,
        _: crate::entities::EventFilter,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(vec![])
    }
    async fn count_events(&self, _: crate::entities::EventFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn get_status(&self, _: &PackageId) -> Result<PackageStatus, CoreError> {
        Ok(PackageStatus::Available)
    }
    async fn set_status(&self, _: &PackageId, _: PackageStatus) -> Result<(), CoreError> {
        Ok(())
    }
    async fn delete_package(&self, _: &PackageId) -> Result<bool, CoreError> {
        Ok(false)
    }

    /// The same three constraints the Postgres query applies: `Download`,
    /// allowed, newest per version.
    async fn last_downloads(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>, CoreError> {
        let events = self.events.read().await;
        let mut newest: std::collections::HashMap<String, DateTime<Utc>> = Default::default();
        for e in events.iter() {
            if !matches!(e.action, AccessAction::Download)
                || !matches!(e.result, AccessResult::Allowed)
            {
                continue;
            }
            let Some(p) = e.package_id.as_ref() else {
                continue;
            };
            if p.registry != registry || p.name != package {
                continue;
            }
            newest
                .entry(p.version.clone())
                .and_modify(|t| {
                    if e.timestamp > *t {
                        *t = e.timestamp
                    }
                })
                .or_insert(e.timestamp);
        }
        Ok(newest.into_iter().collect())
    }
}

fn admin() -> Identity {
    Identity {
        user_id: Some("admin-1".to_owned()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}

fn local_svc(backend: Arc<dyn LocalRegistryBackend>) -> Arc<LocalRegistryService> {
    Arc::new(LocalRegistryService {
        backend,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig::default()),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    })
}

/// Tombstones the first version it is asked about and then refuses.
///
/// At module scope because two tests need it: the one that asserts a fault
/// leaves a *report* of what already went, and the one that asserts it leaves
/// the matching *trail*.
struct FlakyBackend {
    inner: Backend,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl LocalRegistryBackend for FlakyBackend {
    async fn publish(&self, pkg: PublishedPackage) -> Result<(), CoreError> {
        self.inner.publish(pkg).await
    }
    async fn yank(&self, r: &str, n: &str, v: &str) -> Result<(), CoreError> {
        self.inner.yank(r, n, v).await
    }
    async fn unyank(&self, r: &str, n: &str, v: &str) -> Result<(), CoreError> {
        self.inner.unyank(r, n, v).await
    }
    async fn deprecate(&self, r: &str, n: &str, v: &str, m: Option<&str>) -> Result<(), CoreError> {
        self.inner.deprecate(r, n, v, m).await
    }
    async fn undeprecate(&self, r: &str, n: &str, v: &str) -> Result<(), CoreError> {
        self.inner.undeprecate(r, n, v).await
    }
    async fn set_channel(&self, _: &str, _: &str, _: &str, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn unlist(&self, r: &str, n: &str, v: &str) -> Result<(), CoreError> {
        self.inner.unlist(r, n, v).await
    }
    async fn relist(&self, r: &str, n: &str, v: &str) -> Result<(), CoreError> {
        self.inner.relist(r, n, v).await
    }
    async fn get_versions(&self, r: &str, n: &str) -> Result<Vec<PublishedPackage>, CoreError> {
        self.inner.get_versions(r, n).await
    }
    async fn exists(&self, r: &str, n: &str) -> Result<bool, CoreError> {
        self.inner.exists(r, n).await
    }
    async fn list_package_names(&self, r: &str) -> Result<Vec<String>, CoreError> {
        self.inner.list_package_names(r).await
    }
    async fn tombstone_version(
        &self,
        r: &str,
        n: &str,
        v: &str,
        by: Option<&str>,
    ) -> Result<bool, CoreError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1 {
            return Err(CoreError::Database("disk on fire".into()));
        }
        self.inner.tombstone_version(r, n, v, by).await
    }
}

/// A published version, `age_days` old.
fn pkg(name: &str, version: &str, age_days: i64) -> PublishedPackage {
    PublishedPackage {
        registry: "reg".to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
        checksum: "abc".to_owned(),
        yanked: false,
        deprecated: false,
        deprecation_message: None,
        unlisted: false,
        index_metadata: serde_json::json!({}),
        published_at: Utc::now() - ChronoDuration::days(age_days),
        published_by: Some("publisher".to_owned()),
        signature_bytes: None,
        signature_type: None,
        visibility: Visibility::Public,
        retention_keep: false,
    }
}

fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

/// The policy every test starts from: nothing configured, so nothing reclaimed.
fn policy() -> RetentionPolicy {
    RetentionPolicy {
        keep_versions: None,
        keep_for: None,
        keep_if_pulled: None,
        keep_yanked: true,
        // Far enough back that the floor never fires unless a test asks it to.
        download_signal_floor: Utc::now() - ChronoDuration::days(10_000),
        reclaim_delay: Duration::ZERO,
        dry_run: true,
    }
}

/// A local-registry backend with the four behaviours a retention run depends on:
/// listing names, listing a package's versions oldest-first, tombstoning, and
/// refusing a spent coordinate.
///
/// Its own double rather than `adapters`' `InMemoryLocalRegistry`, because
/// `core` cannot depend on `adapters` — the dependency runs the other way. That
/// is a cost worth naming: the two doubles can drift, so anything about the
/// *storage* semantics is asserted in the adapter's own tests and this one
/// carries only what the decision logic reads.
#[derive(Default)]
struct Backend {
    versions: tokio::sync::RwLock<Vec<(PublishedPackage, Option<DateTime<Utc>>)>>,
}

impl Backend {
    async fn seed(&self, pkg: PublishedPackage) {
        self.versions.write().await.push((pkg, None));
    }
}

#[async_trait]
impl LocalRegistryBackend for Backend {
    async fn publish(&self, pkg: PublishedPackage) -> Result<(), CoreError> {
        let mut v = self.versions.write().await;
        if let Some((existing, deleted)) = v.iter().find(|(p, _)| {
            p.registry == pkg.registry && p.name == pkg.name && p.version == pkg.version
        }) {
            if let Some(at) = deleted {
                return Err(CoreError::Conflict(
                    crate::entities::Tombstone {
                        registry: existing.registry.clone(),
                        name: existing.name.clone(),
                        version: existing.version.clone(),
                        deleted_at: *at,
                        deleted_by: None,
                        detail_compacted_at: None,
                        published_at: existing.published_at,
                        published_by: None,
                        checksum: None,
                    }
                    .burned_coordinate_message(),
                ));
            }
            return Err(CoreError::Conflict("already published".into()));
        }
        v.push((pkg, None));
        Ok(())
    }
    async fn yank(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn unyank(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn deprecate(&self, _: &str, _: &str, _: &str, _: Option<&str>) -> Result<(), CoreError> {
        Ok(())
    }
    async fn undeprecate(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn set_channel(&self, _: &str, _: &str, _: &str, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn unlist(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn relist(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }

    /// **Oldest first**, matching the port's contract — the ordering
    /// `keep_versions` inverts to count back from the newest.
    async fn get_versions(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        let v = self.versions.read().await;
        let mut out: Vec<PublishedPackage> = v
            .iter()
            .filter(|(p, d)| d.is_none() && p.registry == registry && p.name == name)
            .map(|(p, _)| p.clone())
            .collect();
        out.sort_by_key(|p| p.published_at);
        Ok(out)
    }
    async fn exists(&self, registry: &str, name: &str) -> Result<bool, CoreError> {
        let v = self.versions.read().await;
        Ok(v.iter()
            .any(|(p, d)| d.is_none() && p.registry == registry && p.name == name))
    }
    async fn list_package_names(&self, registry: &str) -> Result<Vec<String>, CoreError> {
        let v = self.versions.read().await;
        let mut names: Vec<String> = v
            .iter()
            .filter(|(p, d)| d.is_none() && p.registry == registry)
            .map(|(p, _)| p.name.clone())
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
    async fn tombstone_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        _deleted_by: Option<&str>,
    ) -> Result<bool, CoreError> {
        let mut v = self.versions.write().await;
        for (p, d) in v.iter_mut() {
            if p.registry == registry && p.name == name && p.version == version && d.is_none() {
                *d = Some(Utc::now());
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn find_tombstone(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<crate::entities::Tombstone>, CoreError> {
        let v = self.versions.read().await;
        Ok(v.iter()
            .find(|(p, d)| {
                d.is_some() && p.registry == registry && p.name == name && p.version == version
            })
            .map(|(p, d)| crate::entities::Tombstone {
                registry: p.registry.clone(),
                name: p.name.clone(),
                version: p.version.clone(),
                deleted_at: d.unwrap(),
                // The run passes the acting identity through `delete_version`,
                // which is what the assertion about `deleted_by` reads.
                deleted_by: Some("admin-1".to_owned()),
                detail_compacted_at: None,
                published_at: p.published_at,
                published_by: p.published_by.clone(),
                checksum: Some(p.checksum.clone()),
            }))
    }
}

/// Publish `versions` into a fresh backend and return the service.
async fn seeded(versions: Vec<PublishedPackage>) -> Arc<LocalRegistryService> {
    let backend = Arc::new(Backend::default());
    for v in versions {
        backend.seed(v).await;
    }
    local_svc(backend)
}

/// The same, with the audit sink wired up.
///
/// `local_svc` leaves `package_repo: None`, which makes every audit write a
/// silent no-op — fine for the decision tests, useless for the trail ones.
async fn seeded_audited(
    versions: Vec<PublishedPackage>,
    repo: Arc<EventRepo>,
) -> Arc<LocalRegistryService> {
    let backend = Arc::new(Backend::default());
    for v in versions {
        backend.seed(v).await;
    }
    Arc::new(LocalRegistryService {
        backend,
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig::default()),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: Some(repo),
        readme: None,
    })
}

impl EventRepo {
    /// Every event of one action, in the order they were recorded.
    async fn of(&self, action: AccessAction) -> Vec<AccessEvent> {
        self.events
            .read()
            .await
            .iter()
            .filter(|e| e.action == action)
            .cloned()
            .collect()
    }
}

// ── The decision ──────────────────────────────────────────────────────────────

/// An inert policy reclaims nothing, and says so with a reason rather than an
/// empty answer the operator has to interpret.
#[tokio::test]
async fn a_policy_with_no_keep_condition_reclaims_nothing() {
    let svc = seeded(vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 800)]).await;
    let r = RetentionService::new(svc, None)
        .run("reg", &policy(), &admin())
        .await
        .unwrap();
    assert_eq!(r.examined, 2);
    assert_eq!(r.reclaimed, 0);
    assert_eq!(r.kept, 2);
    assert!(r
        .decisions
        .iter()
        .all(|d| d.kept_because == Some(KeepReason::NoPolicy)));
}

/// `keep_versions` counts back from the **newest**, and `get_versions` returns
/// oldest-first — the off-by-one that would keep exactly the wrong end.
#[tokio::test]
async fn keep_versions_keeps_the_newest_not_the_oldest() {
    let svc = seeded(vec![
        pkg("p", "1.0.0", 900),
        pkg("p", "2.0.0", 800),
        pkg("p", "3.0.0", 700),
    ])
    .await;
    let mut p = policy();
    p.keep_versions = Some(2);

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);
    for v in ["2.0.0", "3.0.0"] {
        let d = r.decisions.iter().find(|d| d.version == v).unwrap();
        assert_eq!(d.kept_because, Some(KeepReason::KeepVersions), "{v}");
    }
}

/// `keep_for_days` keeps by publish date, independently of rank.
#[tokio::test]
async fn keep_for_keeps_recent_publishes() {
    let svc = seeded(vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 5)]).await;
    let mut p = policy();
    p.keep_for = Some(days(30));

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);
    assert_eq!(
        r.decisions
            .iter()
            .find(|d| d.version == "2.0.0")
            .unwrap()
            .kept_because,
        Some(KeepReason::KeepFor)
    );
}

/// **The headline retention assertion** (RFC 0016 §4.3): a recently-pulled
/// version is never reclaimed, including when it is the oldest and outside every
/// other keep window.
#[tokio::test]
async fn a_recently_pulled_version_is_never_reclaimed() {
    let svc = seeded(vec![
        pkg("p", "1.0.0", 900), // oldest, and the one being used
        pkg("p", "2.0.0", 800),
        pkg("p", "3.0.0", 700),
    ])
    .await;
    let repo = Arc::new(EventRepo::default());
    repo.record(PackageId::new("reg", "p", "1.0.0"), Utc::now())
        .await;

    let mut p = policy();
    p.keep_versions = Some(1); // would otherwise keep only 3.0.0
    p.keep_if_pulled = Some(days(90));

    let r = RetentionService::new(svc, Some(repo))
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(
        r.reclaimed_coordinates,
        vec!["p@2.0.0".to_owned()],
        "the pulled version survives despite being the oldest and outside keep_versions"
    );
    assert_eq!(
        r.decisions
            .iter()
            .find(|d| d.version == "1.0.0")
            .unwrap()
            .kept_because,
        Some(KeepReason::KeepIfPulled)
    );
}

/// **The sidecar split, first half.** A version kept alive only by checksum
/// fetches is *not* kept: `.sha1`/`.asc`/`.sig` record as `ViewMetadata`, and a
/// verification fetch is not a consumer installing anything.
#[tokio::test]
async fn checksum_fetches_do_not_keep_a_version_alive() {
    let svc = seeded(vec![pkg("lib", "1.0.0", 900)]).await;
    let repo = Arc::new(EventRepo::default());
    for sidecar in ["lib-1.0.0.jar.sha1", "lib-1.0.0.jar.asc", "shasums"] {
        repo.record(
            PackageId::new("reg", "lib", "1.0.0").with_artifact(sidecar),
            Utc::now(),
        )
        .await;
    }

    let mut p = policy();
    p.keep_if_pulled = Some(days(90));

    let r = RetentionService::new(svc, Some(repo))
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(
        r.reclaimed_coordinates,
        vec!["lib@1.0.0".to_owned()],
        "a version whose only access records are verification sidecars is not in use"
    );
}

/// **The sidecar split, second half**, and the one a future widening of
/// `is_verification_sidecar` to "anything that is not the primary artifact"
/// would break: a `.pom` is a file a build actually consumes, so it counts.
#[tokio::test]
async fn a_pom_fetch_keeps_a_version_alive() {
    let svc = seeded(vec![pkg("lib", "1.0.0", 900)]).await;
    let repo = Arc::new(EventRepo::default());
    repo.record(
        PackageId::new("reg", "lib", "1.0.0").with_artifact("lib-1.0.0.pom"),
        Utc::now(),
    )
    .await;

    let mut p = policy();
    p.keep_if_pulled = Some(days(90));

    let r = RetentionService::new(svc, Some(repo))
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert!(
        r.reclaimed_coordinates.is_empty(),
        "a .pom is consumed by a build; it is a download, not a verification fetch"
    );
}

/// A *denied* download is not evidence of use. Otherwise a blocked package would
/// defend itself from reclamation by being repeatedly refused.
#[tokio::test]
async fn a_denied_download_does_not_keep_a_version_alive() {
    let svc = seeded(vec![pkg("p", "1.0.0", 900)]).await;
    let repo = Arc::new(EventRepo::default());
    repo.record_denied(PackageId::new("reg", "p", "1.0.0"), Utc::now())
        .await;

    let mut p = policy();
    p.keep_if_pulled = Some(days(90));

    let r = RetentionService::new(svc, Some(repo))
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);
}

/// A pull *older* than the window does not keep the version — otherwise
/// `keep_if_pulled` would mean "ever pulled".
#[tokio::test]
async fn a_stale_pull_does_not_keep_a_version_alive() {
    let svc = seeded(vec![pkg("p", "1.0.0", 900)]).await;
    let repo = Arc::new(EventRepo::default());
    repo.record(
        PackageId::new("reg", "p", "1.0.0"),
        Utc::now() - ChronoDuration::days(400),
    )
    .await;

    let mut p = policy();
    p.keep_if_pulled = Some(days(90));

    let r = RetentionService::new(svc, Some(repo))
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);
}

/// **The effective floor date** (RFC 0016 §4.3): a version with no access
/// records at all, published before the floor, is kept. The Maven and NuGet
/// local paths recorded nothing before the 2026-08-26 remediation, and a run
/// that read that silence as disuse would reclaim versions in daily use.
#[tokio::test]
async fn a_version_published_before_the_signal_floor_is_kept() {
    let svc = seeded(vec![pkg("old", "1.0.0", 900), pkg("new", "1.0.0", 100)]).await;
    let repo = Arc::new(EventRepo::default());

    let mut p = policy();
    p.keep_if_pulled = Some(days(90));
    p.download_signal_floor = Utc::now() - ChronoDuration::days(400);

    let r = RetentionService::new(svc, Some(repo))
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(
        r.reclaimed_coordinates,
        vec!["new@1.0.0".to_owned()],
        "only the version published *after* the floor may be judged on an absent record"
    );
    assert_eq!(
        r.decisions
            .iter()
            .find(|d| d.name == "old")
            .unwrap()
            .kept_because,
        Some(KeepReason::BeforeSignalFloor)
    );
}

/// The floor is consulted only when the policy reads the download signal. With
/// no `keep_if_pulled`, the signal is not being read and its gaps are nobody's
/// business — otherwise the floor would silently protect everything old from a
/// pure `keep_versions` policy.
#[tokio::test]
async fn the_signal_floor_is_inert_without_keep_if_pulled() {
    let svc = seeded(vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 800)]).await;
    let mut p = policy();
    p.keep_versions = Some(1);
    p.download_signal_floor = Utc::now(); // everything predates it

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);
}

/// A yanked version is kept by default. A yank says "do not install this", which
/// is a reason to stop resolving it and not a reason to destroy the only copy.
#[tokio::test]
async fn keep_yanked_is_on_by_default_and_can_be_turned_off() {
    let mut yanked = pkg("p", "1.0.0", 900);
    yanked.yanked = true;
    let svc = seeded(vec![yanked.clone()]).await;

    // `keep_for` alone: nothing is recent, so the only thing that can save the
    // yanked version is `keep_yanked`.
    let mut p = policy();
    p.keep_for = Some(days(30));

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert!(r.reclaimed_coordinates.is_empty());
    assert_eq!(
        r.decisions[0].kept_because,
        Some(KeepReason::KeepYanked),
        "a yanked version is kept, and the report says which condition kept it"
    );

    let svc = seeded(vec![yanked]).await;
    p.keep_yanked = false;
    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);
}

/// **The version-tier pin** (RFC 0016 §4.1) survives a run that reclaims every
/// other version of the same package, including when it is the oldest and least
/// pulled.
#[tokio::test]
async fn a_pinned_version_survives_a_run_that_reclaims_everything_else() {
    let mut pinned = pkg("p", "1.0.0", 900);
    pinned.retention_keep = true;
    let svc = seeded(vec![pinned, pkg("p", "2.0.0", 800), pkg("p", "3.0.0", 700)]).await;

    let mut p = policy();
    p.keep_for = Some(days(30)); // nothing is recent; everything unpinned goes

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(
        r.reclaimed_coordinates,
        vec!["p@2.0.0".to_owned(), "p@3.0.0".to_owned()]
    );
    assert_eq!(
        r.decisions
            .iter()
            .find(|d| d.version == "1.0.0")
            .unwrap()
            .kept_because,
        Some(KeepReason::Pinned)
    );
}

/// A run is scoped to the registry it names.
#[tokio::test]
async fn a_run_is_scoped_to_one_registry() {
    let backend = Arc::new(Backend::default());
    backend.seed(pkg("p", "1.0.0", 900)).await;
    let mut other = pkg("p", "1.0.0", 900);
    other.registry = "other".to_owned();
    backend.seed(other).await;
    let svc = local_svc(backend.clone());

    let mut p = policy();
    p.keep_for = Some(days(30));

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(
        r.examined, 1,
        "the other registry's versions are not touched"
    );
}

// ── dry_run ───────────────────────────────────────────────────────────────────

/// `dry_run` reclaims nothing, and reports exactly what the live run then does.
#[tokio::test]
async fn dry_run_changes_nothing_and_agrees_with_the_live_run() {
    let versions = vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 5)];
    let svc = seeded(versions.clone()).await;

    let mut p = policy();
    p.keep_for = Some(days(30));

    let preview = RetentionService::new(svc.clone(), None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert!(preview.dry_run);
    assert_eq!(preview.reclaimed, 1);
    assert_eq!(
        svc.backend.get_versions("reg", "p").await.unwrap().len(),
        2,
        "a dry run must not have removed anything"
    );

    p.dry_run = false;
    let live = RetentionService::new(svc.clone(), None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert!(!live.dry_run);
    assert_eq!(live.reclaimed_coordinates, preview.reclaimed_coordinates);
    assert_eq!(
        svc.backend.get_versions("reg", "p").await.unwrap().len(),
        1,
        "the live run reclaimed the version the dry run named"
    );
}

/// A reclamation spends the coordinate, exactly as a hand deletion does — the
/// property that stops retention from being a supply-chain mechanism by
/// accident (RFC 0016 §4.2).
#[tokio::test]
async fn a_reclaimed_version_leaves_a_tombstone() {
    let svc = seeded(vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 5)]).await;
    let mut p = policy();
    p.keep_for = Some(days(30));
    p.dry_run = false;

    RetentionService::new(svc.clone(), None)
        .run("reg", &p, &admin())
        .await
        .unwrap();

    let ts = svc
        .backend
        .find_tombstone("reg", "p", "1.0.0")
        .await
        .unwrap()
        .expect("a reclaimed version leaves a tombstone");
    assert_eq!(ts.deleted_by.as_deref(), Some("admin-1"));

    let err = svc
        .backend
        .publish(pkg("p", "1.0.0", 0))
        .await
        .expect_err("the reclaimed coordinate is spent");
    assert!(err.to_string().contains("never reused"), "{err}");
}

/// **Retention refuses to guess.** A `keep_if_pulled` policy with no repository
/// to read the signal from would reclaim versions it cannot prove are idle, so
/// the run errors rather than treating "no signal" as "no downloads".
#[tokio::test]
async fn keep_if_pulled_without_a_download_signal_refuses_to_run() {
    let svc = seeded(vec![pkg("p", "1.0.0", 900)]).await;
    let mut p = policy();
    p.keep_if_pulled = Some(days(90));

    let err = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .expect_err("a policy that reads a signal it cannot see must refuse");
    assert!(
        err.to_string().contains("cannot prove are idle"),
        "the refusal must say why: {err}"
    );
}

// ── The union of vetoes ───────────────────────────────────────────────────────

/// The property the whole design rests on, stated directly: **any** matching
/// condition keeps, so adding a condition can only ever add survivors. Asserted
/// over every subset of the three conditions against a version each one alone
/// would keep.
#[tokio::test]
async fn adding_a_keep_condition_never_reclaims_more() {
    // Old, recently pulled — kept by `keep_if_pulled`, by nothing else.
    let svc_versions = vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 800)];
    let repo = Arc::new(EventRepo::default());
    repo.record(PackageId::new("reg", "p", "1.0.0"), Utc::now())
        .await;

    let mut reclaimed_counts = Vec::new();
    for extra in [false, true] {
        let svc = seeded(svc_versions.clone()).await;
        let mut p = policy();
        p.keep_versions = Some(1);
        if extra {
            p.keep_if_pulled = Some(days(90));
        }
        let r = RetentionService::new(svc, Some(repo.clone()))
            .run("reg", &p, &admin())
            .await
            .unwrap();
        reclaimed_counts.push(r.reclaimed);
    }
    assert!(
        reclaimed_counts[1] < reclaimed_counts[0],
        "adding keep_if_pulled must keep strictly more here: {reclaimed_counts:?}"
    );
}

/// A run that hits a fault **stops and says so**, keeping the record of what it
/// already reclaimed.
///
/// Neither of the two obvious alternatives is right. Propagating the error
/// throws away the only list of which versions actually went, leaving an
/// operator to reconstruct it from the audit log. Continuing grinds through the
/// rest of the estate against a backend that is evidently broken, turning one
/// fault into a very long one.
#[tokio::test]
async fn a_failing_reclamation_stops_the_run_and_reports_what_went() {
    let inner = Backend::default();
    for v in ["1.0.0", "2.0.0", "3.0.0"] {
        inner.seed(pkg("p", v, 900)).await;
    }
    let svc = local_svc(Arc::new(FlakyBackend {
        inner,
        calls: Default::default(),
    }));

    let mut p = policy();
    p.keep_for = Some(days(30)); // nothing is recent: all three are doomed
    p.dry_run = false;

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .expect("a fault is a partial report, not an error");

    assert_eq!(r.reclaimed, 1, "only the one that actually went is counted");
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);
    let reason = r
        .incomplete_because
        .expect("the report must say it stopped");
    assert!(reason.contains("p@2.0.0"), "and where: {reason}");
    assert!(reason.contains("disk on fire"), "and why: {reason}");
}

// ── The trail ─────────────────────────────────────────────────────────────────

/// RFC 0016 §3: "an operator reading the audit trail must be able to tell a
/// policy reclamation from a human deletion".
///
/// The run carries the operator's *own* identity — it is their token that
/// triggered it — so the subject cannot be what distinguishes them. The action
/// has to.
#[tokio::test]
async fn a_reclamation_is_audited_as_retention_reclaim_not_delete() {
    let repo = Arc::new(EventRepo::default());
    let svc = seeded_audited(
        vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 800)],
        repo.clone(),
    )
    .await;
    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = false;

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed_coordinates, vec!["p@1.0.0".to_owned()]);

    assert!(
        repo.of(AccessAction::Delete).await.is_empty(),
        "a policy reclamation must not be indistinguishable from a hand deletion"
    );
    let reclaims = repo.of(AccessAction::RetentionReclaim).await;
    assert_eq!(reclaims.len(), 1);
    let coord = reclaims[0].package_id.as_ref().unwrap();
    assert_eq!(
        (
            coord.registry.as_str(),
            coord.name.as_str(),
            coord.version.as_str()
        ),
        ("reg", "p", "1.0.0")
    );
    assert_eq!(reclaims[0].user_id.as_deref(), Some("admin-1"));
}

/// One run event per run, registry-scoped, whether or not anything was
/// reclaimed — "who ran the policy against prod" must not be answerable only
/// when it deleted something.
#[tokio::test]
async fn a_live_run_that_reclaims_nothing_still_records_the_run() {
    let repo = Arc::new(EventRepo::default());
    let svc = seeded_audited(vec![pkg("p", "1.0.0", 2)], repo.clone()).await;
    let mut p = policy();
    p.keep_for = Some(days(30)); // the only version is recent: nothing is doomed
    p.dry_run = false;

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(r.reclaimed, 0);

    let runs = repo.of(AccessAction::RetentionRun).await;
    assert_eq!(runs.len(), 1);
    let coord = runs[0].package_id.as_ref().unwrap();
    assert_eq!(coord.registry, "reg");
    assert!(
        coord.name.is_empty() && coord.version.is_empty(),
        "a run is about a registry, not about a package that was not involved"
    );
    assert!(repo.of(AccessAction::RetentionDryRun).await.is_empty());
}

/// A dry run leaves the run event and **nothing else**. No `retention_reclaim`
/// row for a version that is still there: an auditor has to be able to read
/// that action as "this version is gone".
#[tokio::test]
async fn a_dry_run_records_the_run_and_no_reclamations() {
    let repo = Arc::new(EventRepo::default());
    let svc = seeded_audited(
        vec![pkg("p", "1.0.0", 900), pkg("p", "2.0.0", 800)],
        repo.clone(),
    )
    .await;
    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = true;

    let r = RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();
    assert_eq!(
        r.reclaimed_coordinates,
        vec!["p@1.0.0".to_owned()],
        "the report still says what would go"
    );

    assert!(repo.of(AccessAction::RetentionReclaim).await.is_empty());
    assert!(repo.of(AccessAction::Delete).await.is_empty());
    let dry = repo.of(AccessAction::RetentionDryRun).await;
    assert_eq!(dry.len(), 1, "the preview itself is on the record");
    assert!(
        repo.of(AccessAction::RetentionRun).await.is_empty(),
        "a dry run must never look like a run that could have written"
    );
}

/// A run that stops on a fault still records the run, and records exactly the
/// reclamations that happened before it.
#[tokio::test]
async fn an_incomplete_run_is_still_audited() {
    let inner = Backend::default();
    for v in ["1.0.0", "2.0.0", "3.0.0"] {
        inner.seed(pkg("p", v, 900)).await;
    }
    let repo = Arc::new(EventRepo::default());
    let svc = Arc::new(LocalRegistryService {
        backend: Arc::new(FlakyBackend {
            inner,
            calls: Default::default(),
        }),
        storage: Arc::new(NoopStorage),
        hot: new_hot_lock(HotConfig::default()),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: Some(repo.clone()),
        readme: None,
    });

    let mut p = policy();
    p.keep_for = Some(days(30));
    p.dry_run = false;

    RetentionService::new(svc, None)
        .run("reg", &p, &admin())
        .await
        .unwrap();

    assert_eq!(
        repo.of(AccessAction::RetentionReclaim).await.len(),
        1,
        "only the version that actually went"
    );
    assert_eq!(
        repo.of(AccessAction::RetentionRun).await.len(),
        1,
        "a run that faulted is still a run somebody started"
    );
}
