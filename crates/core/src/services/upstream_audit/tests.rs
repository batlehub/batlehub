//! The state machine end to end (RFC 0014 §10), against fakes: a registry
//! client the test scripts, an inventory it seeds, and an in-core status
//! store that records every call so a voided sweep can be asserted to have
//! written nothing at all.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::*;
use crate::entities::{
    AccessEvent, EventFilter, MissObservation, NotificationEvent, NotificationEventType,
    PackageFilter, PackageId, PackageMetadata, PackageStatus, PackageSummary, ScanJob,
    SecurityPolicy, UpstreamKey, UpstreamStatusFilter,
};
use crate::ports::{
    ArtifactInventory, ArtifactMeta, FetchedArtifact, NotificationSink, PackageRepository,
    QueuedCount, RegistryClient, ScanQueue,
};
use crate::services::AdminService;
use crate::services::HotConfig;

// ── fakes ────────────────────────────────────────────────────────────────────

/// What upstream says, per package: `Some(versions)` lists them, `None`
/// denies the package, and a name in `errors` fails to answer.
struct ScriptedRegistry {
    kind: &'static str,
    listing: Mutex<HashMap<String, Option<Vec<String>>>>,
    errors: Mutex<HashSet<String>>,
    /// Whether `list_versions` is implemented at all (a kind that only
    /// answers per version leaves it at the default empty).
    lists: bool,
    /// The files a path-addressed upstream has (`true`) or has lost
    /// (`false`); a path not in the map was never there.
    files: Mutex<HashMap<String, bool>>,
}

impl ScriptedRegistry {
    fn new(kind: &'static str, lists: bool) -> Arc<Self> {
        Arc::new(Self {
            kind,
            listing: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashSet::new()),
            lists,
            files: Mutex::new(HashMap::new()),
        })
    }
    fn has_file(&self, path: &str) {
        self.files.lock().unwrap().insert(path.into(), true);
    }
    fn lost_file(&self, path: &str) {
        self.files.lock().unwrap().insert(path.into(), false);
    }
    fn has(&self, name: &str, versions: &[&str]) {
        self.listing.lock().unwrap().insert(
            name.into(),
            Some(versions.iter().map(|v| v.to_string()).collect()),
        );
    }
    fn gone(&self, name: &str) {
        self.listing.lock().unwrap().insert(name.into(), None);
    }
    fn failing(&self, name: &str, yes: bool) {
        let mut e = self.errors.lock().unwrap();
        if yes {
            e.insert(name.into());
        } else {
            e.remove(name);
        }
    }
}

#[async_trait]
impl RegistryClient for ScriptedRegistry {
    fn registry_type(&self) -> &str {
        self.kind
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        if self.errors.lock().unwrap().contains(&pkg.name) {
            return Err(CoreError::Registry("upstream down".into()));
        }
        match self.listing.lock().unwrap().get(&pkg.name) {
            Some(Some(vs)) if vs.contains(&pkg.version) => Ok(PackageMetadata::minimal(
                pkg.clone(),
                serde_json::Value::Null,
            )),
            _ => Err(CoreError::NotFound(format!("{pkg} not found"))),
        }
    }
    async fn fetch_artifact(&self, _: &PackageId) -> Result<FetchedArtifact, CoreError> {
        Err(CoreError::NotFound("no bytes in this fake".into()))
    }
    async fn probe_artifact(&self, pkg: &PackageId) -> Result<(), CoreError> {
        let path = pkg.artifact.clone().unwrap_or_default();
        if self.errors.lock().unwrap().contains(&path) {
            return Err(CoreError::Registry("upstream down".into()));
        }
        match self.files.lock().unwrap().get(&path) {
            Some(true) => Ok(()),
            _ => Err(CoreError::NotFound(format!("{path} not found upstream"))),
        }
    }
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        if !self.lists {
            return Ok(vec![]);
        }
        if self.errors.lock().unwrap().contains(package) {
            return Err(CoreError::Registry("upstream down".into()));
        }
        match self.listing.lock().unwrap().get(package) {
            Some(Some(vs)) => Ok(vs.clone()),
            Some(None) => Err(CoreError::NotFound(format!("{package} not found"))),
            None => Ok(vec![]),
        }
    }
}

struct SeededInventory {
    rows: Mutex<Vec<ArtifactMeta>>,
}

impl SeededInventory {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(Vec::new()),
        })
    }
    fn cached(&self, registry: &str, name: &str, version: &str, cached_at: DateTime<Utc>) {
        self.rows.lock().unwrap().push(ArtifactMeta {
            artifact_key: format!("{registry}/{name}/{version}"),
            registry: registry.into(),
            package_name: name.into(),
            version: version.into(),
            size_bytes: Some(1),
            cached_at,
            last_accessed_at: cached_at,
        });
    }
    /// A path kind's row: the synthetic `repo/_` coordinate, the file in
    /// the key (`generic.rs`).
    fn cached_file(&self, registry: &str, path: &str, cached_at: DateTime<Utc>) {
        self.rows.lock().unwrap().push(ArtifactMeta {
            artifact_key: format!("artifact:{registry}/repo/_/{path}"),
            registry: registry.into(),
            package_name: "repo".into(),
            version: "_".into(),
            size_bytes: Some(1),
            cached_at,
            last_accessed_at: cached_at,
        });
    }
}

#[async_trait]
impl ArtifactInventory for SeededInventory {
    async fn list_artifacts(&self, registry: &str) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.registry == registry)
            .cloned()
            .collect())
    }
    async fn list_artifacts_by_package(&self) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn list_expired_by_ttl(
        &self,
        _: &str,
        _: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }
    async fn list_idle(&self, _: &str, _: DateTime<Utc>) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }
    async fn total_size_bytes(&self, _: &str) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_lru(&self, _: &str, _: i64) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }
}

type RowKey = (String, String, Option<String>);

/// An in-core status store that logs every write, so "the sweep wrote
/// nothing" is an assertion on the log rather than on the final state.
#[derive(Default)]
struct RecordingStatus {
    rows: Mutex<HashMap<RowKey, UpstreamStatus>>,
    writes: Mutex<Vec<String>>,
}

impl RecordingStatus {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn row(&self, registry: &str, name: &str, version: Option<&str>) -> Option<UpstreamStatus> {
        self.rows
            .lock()
            .unwrap()
            .get(&(registry.into(), name.into(), version.map(str::to_owned)))
            .cloned()
    }
    fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }
    /// Move a row's first miss back in time, so the age floor is met.
    fn age(&self, registry: &str, name: &str, version: Option<&str>, by: Duration) {
        if let Some(r) = self.rows.lock().unwrap().get_mut(&(
            registry.into(),
            name.into(),
            version.map(str::to_owned),
        )) {
            r.first_missed_at -= chrono::Duration::from_std(by).unwrap();
        }
    }
}

#[async_trait]
impl UpstreamStatusPort for RecordingStatus {
    async fn record_miss(&self, obs: MissObservation<'_>) -> Result<UpstreamStatus, CoreError> {
        self.writes.lock().unwrap().push(format!(
            "miss {}/{}{}",
            obs.key.registry,
            obs.key.package_name,
            obs.key.version.map(|v| format!("@{v}")).unwrap_or_default()
        ));
        let key = (
            obs.key.registry.to_owned(),
            obs.key.package_name.to_owned(),
            obs.key.version.map(str::to_owned),
        );
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .entry(key)
            .and_modify(|r| {
                r.consecutive_misses += 1;
                r.last_checked_at = obs.at;
            })
            .or_insert_with(|| UpstreamStatus {
                registry: obs.key.registry.to_owned(),
                package_name: obs.key.package_name.to_owned(),
                version: obs.key.version.map(str::to_owned),
                state: UpstreamState::Missing,
                first_missed_at: obs.at,
                last_checked_at: obs.at,
                confirmed_at: None,
                consecutive_misses: 1,
                last_error: obs.error.map(str::to_owned),
            });
        Ok(row.clone())
    }
    async fn confirm(&self, key: &UpstreamKey<'_>, at: DateTime<Utc>) -> Result<(), CoreError> {
        self.writes
            .lock()
            .unwrap()
            .push(format!("confirm {}/{}", key.registry, key.package_name));
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .get_mut(&(
                key.registry.to_owned(),
                key.package_name.to_owned(),
                key.version.map(str::to_owned),
            ))
            .ok_or_else(|| CoreError::NotFound("no row".into()))?;
        row.state = UpstreamState::Disappeared;
        row.confirmed_at.get_or_insert(at);
        Ok(())
    }
    async fn clear(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError> {
        let removed = self.rows.lock().unwrap().remove(&(
            key.registry.to_owned(),
            key.package_name.to_owned(),
            key.version.map(str::to_owned),
        ));
        if removed.is_some() {
            self.writes
                .lock()
                .unwrap()
                .push(format!("clear {}/{}", key.registry, key.package_name));
        }
        Ok(removed)
    }
    async fn get(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError> {
        Ok(self.row(key.registry, key.package_name, key.version))
    }
    async fn list(&self, filter: UpstreamStatusFilter) -> Result<Vec<UpstreamStatus>, CoreError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| filter.registry.as_ref().is_none_or(|g| *g == r.registry))
            .filter(|r| filter.state.is_none_or(|s| s == r.state))
            .cloned()
            .collect())
    }
    async fn count(&self, filter: UpstreamStatusFilter) -> Result<u64, CoreError> {
        Ok(self.list(filter).await?.len() as u64)
    }
    async fn disappeared_keys(&self, registry: &str) -> Result<HashSet<String>, CoreError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.registry == registry && r.state == UpstreamState::Disappeared)
            .map(UpstreamStatus::coordinate)
            .collect())
    }
}

#[derive(Default)]
struct RecordingQueue {
    jobs: Mutex<Vec<(PackageId, ScanTrigger)>>,
}

#[async_trait]
impl ScanQueue for RecordingQueue {
    async fn try_lead(&self, _: i64) -> Result<bool, CoreError> {
        Ok(true)
    }

    async fn enqueue(
        &self,
        package: &PackageId,
        _: Option<DateTime<Utc>>,
        trigger: ScanTrigger,
    ) -> Result<bool, CoreError> {
        self.jobs.lock().unwrap().push((package.clone(), trigger));
        Ok(true)
    }
    async fn lease(
        &self,
        _: &str,
        _: &[String],
        _: u32,
        _: u64,
        _: u32,
    ) -> Result<Vec<ScanJob>, CoreError> {
        Ok(vec![])
    }
    async fn heartbeat(&self, _: uuid::Uuid, _: u64) -> Result<(), CoreError> {
        Ok(())
    }
    async fn complete(&self, _: uuid::Uuid) -> Result<(), CoreError> {
        Ok(())
    }
    async fn fail(&self, _: uuid::Uuid, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn exhausted(&self, _: u32, _: u32) -> Result<Vec<ScanJob>, CoreError> {
        Ok(vec![])
    }
    async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError> {
        Ok(vec![])
    }
}

/// Every event the sweep emitted, in order.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<NotificationEvent>>,
}

impl RecordingSink {
    fn events(&self) -> Vec<NotificationEvent> {
        self.events.lock().unwrap().clone()
    }
    fn of(&self, kind: NotificationEventType) -> Vec<NotificationEvent> {
        self.events()
            .into_iter()
            .filter(|e| e.event_type == kind)
            .collect()
    }
}

impl NotificationSink for RecordingSink {
    fn emit(&self, event: NotificationEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// The block table, as the admin pen sees it: statuses, the audit trail,
/// and a switch that makes any write a panic — the strongest statement
/// that `"audit"` is inert (RFC 0014 §10).
#[derive(Default)]
struct BlockTable {
    statuses: Mutex<HashMap<String, PackageStatus>>,
    events: Mutex<Vec<AccessEvent>>,
    refuse_writes: Mutex<bool>,
    fail_writes: Mutex<bool>,
}

impl BlockTable {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn status(&self, id: &PackageId) -> PackageStatus {
        self.statuses
            .lock()
            .unwrap()
            .get(&id.cache_key())
            .cloned()
            .unwrap_or(PackageStatus::Available)
    }
    fn blocked_by(&self, id: &PackageId) -> Option<(String, String)> {
        match self.status(id) {
            PackageStatus::Blocked {
                reason, blocked_by, ..
            } => Some((blocked_by, reason)),
            _ => None,
        }
    }
    fn block_as(&self, id: &PackageId, who: &str) {
        self.statuses.lock().unwrap().insert(
            id.cache_key(),
            PackageStatus::Blocked {
                reason: "an admin's own reason".into(),
                blocked_by: who.into(),
                blocked_at: Utc::now(),
            },
        );
    }
}

#[async_trait]
impl PackageRepository for BlockTable {
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
    async fn get_status(&self, pkg: &PackageId) -> Result<PackageStatus, CoreError> {
        Ok(self.status(pkg))
    }
    async fn set_status(&self, pkg: &PackageId, status: PackageStatus) -> Result<(), CoreError> {
        assert!(
            !*self.refuse_writes.lock().unwrap(),
            "the block table was written under on_confirmed = \"audit\": {pkg} -> {status:?}"
        );
        if *self.fail_writes.lock().unwrap() {
            return Err(CoreError::Database(
                "the block table is read-only today".into(),
            ));
        }
        self.statuses
            .lock()
            .unwrap()
            .insert(pkg.cache_key(), status);
        Ok(())
    }
    async fn list_packages(&self, _: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(&self, _: PackageFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(&self, _: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(self.events.lock().unwrap().clone())
    }
    async fn count_events(&self, _: EventFilter) -> Result<u64, CoreError> {
        Ok(self.events.lock().unwrap().len() as u64)
    }
    async fn list_own_downloads(
        &self,
        _: &str,
        _: DateTime<Utc>,
        _: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(vec![])
    }
    async fn delete_package(&self, _: &PackageId) -> Result<bool, CoreError> {
        Ok(false)
    }
}

// ── the lab ──────────────────────────────────────────────────────────────────

const REG: &str = "npm-proxy";

struct Lab {
    upstream: Arc<ScriptedRegistry>,
    inventory: Arc<SeededInventory>,
    status: Arc<RecordingStatus>,
    queue: Arc<RecordingQueue>,
    sink: Arc<RecordingSink>,
    blocks: Arc<BlockTable>,
    svc: UpstreamAuditService,
}

fn lab_with(kind: &'static str, lists: bool, secured: bool, policy: UpstreamAuditPolicy) -> Lab {
    let upstream = ScriptedRegistry::new(kind, lists);
    let inventory = SeededInventory::new();
    let status = RecordingStatus::new();
    let queue = Arc::new(RecordingQueue::default());
    let sink = Arc::new(RecordingSink::default());
    let blocks = BlockTable::new();
    // Under "audit" the table refuses every write: the default must not
    // touch it, and a branch that did would panic here rather than pass.
    *blocks.refuse_writes.lock().unwrap() = policy.on_confirmed == OnConfirmed::Audit;
    let admin = Arc::new(AdminService::new(
        Arc::clone(&blocks) as Arc<dyn PackageRepository>
    ));
    let mut hot = HotConfig::default();
    hot.registries
        .insert(REG.into(), Arc::clone(&upstream) as Arc<dyn RegistryClient>);
    if secured {
        hot.security
            .insert(REG.into(), SecurityPolicy::defaults_for(REG));
    }
    let svc = UpstreamAuditService::new(
        Arc::clone(&inventory) as Arc<dyn ArtifactInventory>,
        Arc::clone(&status) as Arc<dyn UpstreamStatusPort>,
        Arc::new(RwLock::new(hot)),
        None,
        Some(Arc::clone(&queue) as Arc<dyn ScanQueue>),
        policy,
        4,
        vec![REG.into()],
    )
    .with_notifier(Arc::clone(&sink) as Arc<dyn NotificationSink>)
    .with_admin(admin);
    Lab {
        upstream,
        inventory,
        status,
        queue,
        sink,
        blocks,
        svc,
    }
}

/// `quick()` with the block arm.
fn blocking() -> UpstreamAuditPolicy {
    UpstreamAuditPolicy {
        on_confirmed: OnConfirmed::Block,
        ..quick()
    }
}

/// Fast confirmation: two misses, no age floor, ratio at the default.
fn quick() -> UpstreamAuditPolicy {
    UpstreamAuditPolicy {
        confirm_after: 2,
        confirm_min_age: Duration::ZERO,
        skip_recently_seen: false,
        ..Default::default()
    }
}

fn long_ago() -> DateTime<Utc> {
    Utc::now() - chrono::Duration::days(30)
}

/// Twelve present packages, so the ratio gate has a population to judge.
fn seed_population(lab: &Lab, n: usize) {
    for i in 0..n {
        let name = format!("steady-{i}");
        lab.inventory.cached(REG, &name, "1.0.0", long_ago());
        lab.upstream.has(&name, &["1.0.0"]);
    }
}

async fn one(lab: &Lab) -> RegistryReport {
    lab.svc.run_sweep().await.registries.remove(0)
}

// ── the state machine ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_miss_is_silent_until_confirm_after_and_then_one_transition() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]); // 1.3.1 unpublished

    let r1 = one(&lab).await;
    assert!(!r1.void);
    assert_eq!((r1.probed, r1.missing), (13, 1));
    assert!(r1.transitions.is_empty(), "one miss is not news");
    let row = lab.status.row(REG, "left-pad", Some("1.3.1")).unwrap();
    assert_eq!(
        (row.state, row.consecutive_misses),
        (UpstreamState::Missing, 1)
    );

    let r2 = one(&lab).await;
    assert_eq!(r2.confirmed(), 1);
    let row = lab.status.row(REG, "left-pad", Some("1.3.1")).unwrap();
    assert_eq!(row.state, UpstreamState::Disappeared);
    assert!(row.confirmed_at.is_some());

    // Confirmed once, not re-reported while the state persists.
    let r3 = one(&lab).await;
    assert!(r3.transitions.is_empty());
    assert_eq!(
        lab.status
            .writes()
            .iter()
            .filter(|w| w.starts_with("confirm"))
            .count(),
        1
    );
}

#[tokio::test]
async fn the_age_floor_holds_a_confirmation_the_count_would_allow() {
    let policy = UpstreamAuditPolicy {
        confirm_after: 2,
        confirm_min_age: Duration::from_secs(3600),
        skip_recently_seen: false,
        ..Default::default()
    };
    let lab = lab_with("npm", true, false, policy);
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]);
    one(&lab).await;
    let r = one(&lab).await;
    assert_eq!(
        r.confirmed(),
        0,
        "two misses inside the hour are not a disappearance"
    );
    assert_eq!(
        lab.status
            .row(REG, "left-pad", Some("1.3.1"))
            .unwrap()
            .consecutive_misses,
        2
    );
    // Time passes; the count is already met.
    lab.status
        .age(REG, "left-pad", Some("1.3.1"), Duration::from_secs(7200));
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 1);
}

#[tokio::test]
async fn a_successful_probe_clears_the_row_rather_than_decrementing_it() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "flappy", "2.0.0", long_ago());
    lab.upstream.has("flappy", &["1.0.0"]);
    one(&lab).await;
    assert!(lab.status.row(REG, "flappy", Some("2.0.0")).is_some());
    lab.upstream.has("flappy", &["1.0.0", "2.0.0"]);
    let r = one(&lab).await;
    assert_eq!(r.reappeared(), 1);
    assert!(lab.status.row(REG, "flappy", Some("2.0.0")).is_none());
    // Gone again: the count restarts at one.
    lab.upstream.has("flappy", &["1.0.0"]);
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 0);
    assert_eq!(
        lab.status
            .row(REG, "flappy", Some("2.0.0"))
            .unwrap()
            .consecutive_misses,
        1
    );
}

#[tokio::test]
async fn a_voided_sweep_writes_nothing_at_all() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    // Five of seventeen packages "gone" at once: an outage, not five unpublishes.
    for i in 0..5 {
        lab.inventory
            .cached(REG, &format!("outage-{i}"), "1.0.0", long_ago());
        lab.upstream.gone(&format!("outage-{i}"));
    }
    let r = one(&lab).await;
    assert!(r.void, "{r:?}");
    assert!(lab.status.writes().is_empty(), "{:?}", lab.status.writes());
    assert!(r.transitions.is_empty());
}

#[tokio::test]
async fn under_the_population_floor_the_ratio_says_nothing() {
    let lab = lab_with("npm", true, false, quick());
    // Three packages, one gone: 33 % missing, but three is under the floor.
    seed_population(&lab, 2);
    lab.inventory.cached(REG, "lonely", "1.0.0", long_ago());
    lab.upstream.gone("lonely");
    let r = one(&lab).await;
    assert!(!r.void);
    assert!(lab.status.row(REG, "lonely", None).is_some());
}

#[tokio::test]
async fn an_upstream_that_cannot_answer_is_inconclusive_on_both_sides_of_the_ratio() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    for i in 0..12 {
        lab.upstream.failing(&format!("steady-{i}"), true);
    }
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]);
    let r = one(&lab).await;
    // 12 inconclusive, 1 conclusive and missing: under the floor, not void.
    assert_eq!((r.inconclusive, r.missing, r.void), (12, 1, false));
    assert!(lab.status.row(REG, "left-pad", Some("1.3.1")).is_some());
    for i in 0..12 {
        assert!(lab
            .status
            .row(REG, &format!("steady-{i}"), Some("1.0.0"))
            .is_none());
    }
}

#[tokio::test]
async fn an_empty_listing_is_never_a_miss_and_falls_to_the_per_version_probe() {
    // A kind whose `list_versions` is the default empty: rung 3 answers.
    let lab = lab_with("npm", false, false, quick());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]);
    let r = one(&lab).await;
    assert_eq!((r.missing, r.inconclusive), (1, 0));
    assert!(lab.status.row(REG, "left-pad", Some("1.3.1")).is_some());
    for i in 0..12 {
        assert!(lab
            .status
            .row(REG, &format!("steady-{i}"), Some("1.0.0"))
            .is_none());
    }
}

#[tokio::test]
async fn the_per_version_probe_is_capped_and_says_so() {
    let lab = lab_with("npm", false, false, quick());
    seed_population(&lab, 12);
    let many: Vec<String> = (0..40).map(|i| format!("0.{i}.0")).collect();
    for v in &many {
        lab.inventory.cached(REG, "huge", v, long_ago());
    }
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    lab.upstream.has("huge", &refs);
    let r = one(&lab).await;
    assert_eq!(r.capped_packages, 1);
}

#[tokio::test]
async fn a_whole_package_disappearance_is_one_row_and_one_transition() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        lab.inventory.cached(REG, "withdrawn", v, long_ago());
    }
    lab.upstream.gone("withdrawn");
    one(&lab).await;
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 1);
    match &r.transitions[0] {
        Transition::Confirmed(row, versions) => {
            assert_eq!(row.version, None);
            assert_eq!(versions.len(), 3);
        }
        other => panic!("{other:?}"),
    }
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        assert!(lab.status.row(REG, "withdrawn", Some(v)).is_none());
    }
    let held = lab.status.disappeared_keys(REG).await.unwrap();
    assert_eq!(held, HashSet::from(["withdrawn".to_owned()]));
}

#[tokio::test]
async fn traffic_since_the_last_sweep_skips_the_probe() {
    let lab = lab_with(
        "npm",
        true,
        false,
        UpstreamAuditPolicy {
            confirm_after: 2,
            confirm_min_age: Duration::ZERO,
            skip_recently_seen: true,
            ..Default::default()
        },
    );
    seed_population(&lab, 12);
    let r1 = one(&lab).await;
    assert_eq!(
        r1.skipped_recent, 0,
        "nothing to compare against on the first sweep"
    );
    // Re-cached from upstream since the sweep started: present by evidence.
    lab.inventory.cached(
        REG,
        "fresh",
        "1.0.0",
        Utc::now() + chrono::Duration::seconds(1),
    );
    lab.upstream.gone("fresh");
    let r2 = one(&lab).await;
    assert_eq!(r2.skipped_recent, 1);
    assert!(lab.status.row(REG, "fresh", None).is_none());
}

#[tokio::test]
async fn a_local_registry_is_never_in_the_input() {
    let lab = lab_with("npm", true, false, quick());
    lab.inventory
        .cached("npm-local", "mine", "1.0.0", long_ago());
    let report = lab.svc.run_sweep().await;
    assert_eq!(report.registries.len(), 1);
    assert_eq!(report.registries[0].probed, 0);
}

// ── phase 4: notifications (RFC 0014 §4.5) ───────────────────────────────────

#[tokio::test]
async fn a_confirmation_emits_exactly_one_event_per_package_with_the_payload() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        lab.inventory.cached(REG, "withdrawn", v, long_ago());
    }
    lab.upstream.gone("withdrawn");

    one(&lab).await;
    assert!(lab.sink.events().is_empty(), "a silent miss tells nobody");

    one(&lab).await;
    let events = lab.sink.events();
    assert_eq!(events.len(), 1, "one package, one event: {events:?}");
    let e = &events[0];
    assert_eq!(
        e.event_type,
        NotificationEventType::PackageDisappearedUpstream
    );
    assert_eq!(
        (e.registry.as_str(), e.package_name.as_str()),
        (REG, "withdrawn")
    );
    assert_eq!(e.version, None, "package-level: the version is the package");
    assert_eq!(e.actor, SYSTEM_ACTOR);
    let m = &e.metadata;
    assert_eq!(m["consecutive_misses"], 2);
    assert_eq!(m["probe"], "package");
    assert_eq!(m["versions"].as_array().unwrap().len(), 3);
    assert_eq!(m["policy"], "audit");
    assert_eq!(m["blocked"], false);
    assert_eq!(m["held_from_eviction"], true);
    assert!(m["first_missed_at"].is_string());
    assert!(m["confirmed_at"].is_string());
    assert!(m["cached_at"].is_string());
    assert_eq!(m["sweep"]["probed"], 13);
    assert_eq!(m["sweep"]["missing"], 1);

    // Confirmed once, reported once.
    one(&lab).await;
    assert_eq!(lab.sink.events().len(), 1);
}

#[tokio::test]
async fn a_version_level_confirmation_names_the_version_and_the_rung() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]);
    one(&lab).await;
    one(&lab).await;
    let events = lab
        .sink
        .of(NotificationEventType::PackageDisappearedUpstream);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].version.as_deref(), Some("1.3.1"));
    assert_eq!(events[0].metadata["probe"], "version_listing");
}

#[tokio::test]
async fn a_voided_sweep_emits_upstream_unreachable_and_no_package_event() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    for i in 0..5 {
        lab.inventory
            .cached(REG, &format!("outage-{i}"), "1.0.0", long_ago());
        lab.upstream.gone(&format!("outage-{i}"));
    }
    let r = one(&lab).await;
    assert!(r.void);
    let events = lab.sink.events();
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e.event_type, NotificationEventType::UpstreamUnreachable);
    assert_eq!(e.package_name, "*", "registry-scoped");
    assert_eq!(e.version, None);
    assert_eq!(e.metadata["probed"], 17);
    assert_eq!(e.metadata["missing"], 5);
    assert!(lab
        .sink
        .of(NotificationEventType::PackageDisappearedUpstream)
        .is_empty());
}

#[tokio::test]
async fn a_reappearance_is_reported_only_for_a_row_that_had_reached_disappeared() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "flappy", "2.0.0", long_ago());

    // One miss, then back: the row was `missing`, never reported, and its
    // clearing is not news either.
    lab.upstream.has("flappy", &["1.0.0"]);
    one(&lab).await;
    lab.upstream.has("flappy", &["1.0.0", "2.0.0"]);
    let r = one(&lab).await;
    assert_eq!(r.reappeared(), 1, "the row cleared…");
    assert!(
        lab.sink.events().is_empty(),
        "…but nobody was told it was missing, so nobody is told it is back"
    );

    // Confirmed gone, then back: reported both ways.
    lab.upstream.has("flappy", &["1.0.0"]);
    one(&lab).await;
    one(&lab).await;
    assert_eq!(
        lab.sink
            .of(NotificationEventType::PackageDisappearedUpstream)
            .len(),
        1
    );
    lab.upstream.has("flappy", &["1.0.0", "2.0.0"]);
    one(&lab).await;
    let back = lab
        .sink
        .of(NotificationEventType::PackageReappearedUpstream);
    assert_eq!(back.len(), 1, "{:?}", lab.sink.events());
    assert_eq!(back[0].version.as_deref(), Some("2.0.0"));
    assert_eq!(back[0].metadata["unblocked"], false);
    assert!(back[0].metadata["confirmed_at"].is_string());
}

// ── phase 6: the block arm (RFC 0014 §4.3, §6.5, §10) ────────────────────────

#[tokio::test]
async fn under_audit_a_confirmation_touches_no_block_path_at_all() {
    // `lab_with` arms the block table to panic on any write under "audit".
    let lab = lab_with("npm", true, false, quick());
    assert!(!lab.svc.blocks());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "withdrawn", "1.0.0", long_ago());
    lab.upstream.gone("withdrawn");
    one(&lab).await;
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 1);
    assert!(!lab
        .blocks
        .status(&PackageId::new(REG, "withdrawn", "1.0.0"))
        .is_blocked());
    let e = &lab
        .sink
        .of(NotificationEventType::PackageDisappearedUpstream)[0];
    assert_eq!(e.metadata["policy"], "audit");
    assert_eq!(e.metadata["blocked"], false);
}

#[tokio::test]
async fn under_block_a_version_confirmation_blocks_that_version_with_the_rfcs_row() {
    let lab = lab_with("npm", true, false, blocking());
    assert!(lab.svc.blocks());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "left-pad", "1.3.0", long_ago());
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]); // 1.3.1 unpublished
    one(&lab).await;
    assert!(
        lab.blocks
            .blocked_by(&PackageId::new(REG, "left-pad", "1.3.1"))
            .is_none(),
        "one miss blocks nothing"
    );
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 1);
    let (by, reason) = lab
        .blocks
        .blocked_by(&PackageId::new(REG, "left-pad", "1.3.1"))
        .expect("blocked");
    assert_eq!(by, SYSTEM_ACTOR);
    assert!(
        reason.starts_with("upstream disappearance confirmed ")
            && reason.contains("2 misses since"),
        "{reason}"
    );
    assert!(
        lab.blocks
            .blocked_by(&PackageId::new(REG, "left-pad", "1.3.0"))
            .is_none(),
        "the version upstream still has is untouched"
    );
    // In the audit trail, as an admin's block would be.
    let trail = lab.blocks.events.lock().unwrap().clone();
    assert!(trail.iter().any(|e| {
        matches!(e.action, crate::entities::AccessAction::Block)
            && e.user_id.as_deref() == Some(SYSTEM_ACTOR)
    }));
    let e = &lab
        .sink
        .of(NotificationEventType::PackageDisappearedUpstream)[0];
    assert_eq!(e.metadata["policy"], "block");
    assert_eq!(e.metadata["blocked"], true);
}

#[tokio::test]
async fn under_block_a_package_confirmation_blocks_every_held_version() {
    let lab = lab_with("npm", true, false, blocking());
    seed_population(&lab, 12);
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        lab.inventory.cached(REG, "withdrawn", v, long_ago());
    }
    lab.upstream.gone("withdrawn");
    one(&lab).await;
    one(&lab).await;
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        let (by, _) = lab
            .blocks
            .blocked_by(&PackageId::new(REG, "withdrawn", v))
            .unwrap_or_else(|| panic!("{v} not blocked"));
        assert_eq!(by, SYSTEM_ACTOR);
    }
}

#[tokio::test]
async fn a_reappearance_unblocks_only_this_audits_own_block_and_flags_an_admins() {
    let lab = lab_with("npm", true, false, blocking());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "flappy", "2.0.0", long_ago());
    lab.inventory.cached(REG, "manual", "1.0.0", long_ago());
    lab.upstream.has("flappy", &["1.0.0"]);
    lab.upstream.has("manual", &["0.9.0"]);
    one(&lab).await;
    one(&lab).await;
    let flappy = PackageId::new(REG, "flappy", "2.0.0");
    let manual = PackageId::new(REG, "manual", "1.0.0");
    assert!(lab.blocks.blocked_by(&flappy).is_some());
    // An admin edits the second block before upstream comes back.
    lab.blocks.block_as(&manual, "alice");

    lab.upstream.has("flappy", &["1.0.0", "2.0.0"]);
    lab.upstream.has("manual", &["0.9.0", "1.0.0"]);
    let r = one(&lab).await;
    assert_eq!(r.reappeared(), 2);
    assert!(!lab.blocks.status(&flappy).is_blocked(), "ours: lifted");
    assert_eq!(
        lab.blocks.blocked_by(&manual).map(|(by, _)| by).as_deref(),
        Some("alice"),
        "an admin's decision is not reversed by a 200"
    );
    let back = lab
        .sink
        .of(NotificationEventType::PackageReappearedUpstream);
    let of = |name: &str| {
        back.iter()
            .find(|e| e.package_name == name)
            .unwrap()
            .metadata
            .clone()
    };
    assert_eq!(of("flappy")["unblocked"], true);
    assert!(of("flappy").get("unblock_skipped_reason").is_none());
    assert_eq!(of("manual")["unblocked"], false);
    assert_eq!(of("manual")["unblock_skipped_reason"], "blocked_by_admin");
}

#[tokio::test]
async fn a_failed_block_write_is_reported_as_not_blocked_rather_than_a_lie() {
    let lab = lab_with("npm", true, false, blocking());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "withdrawn", "1.0.0", long_ago());
    lab.upstream.gone("withdrawn");
    one(&lab).await;
    *lab.blocks.fail_writes.lock().unwrap() = true;
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 1, "the row is still confirmed");
    let e = &lab
        .sink
        .of(NotificationEventType::PackageDisappearedUpstream)[0];
    assert_eq!(e.metadata["policy"], "block");
    assert_eq!(e.metadata["blocked"], false);

    // §6.5: the next sweep reconciles a confirmed row that has no block.
    *lab.blocks.fail_writes.lock().unwrap() = false;
    let r = one(&lab).await;
    assert!(r.transitions.is_empty(), "nothing new to report");
    let (by, _) = lab
        .blocks
        .blocked_by(&PackageId::new(REG, "withdrawn", "1.0.0"))
        .expect("reconciled");
    assert_eq!(by, SYSTEM_ACTOR);
}

#[tokio::test]
async fn enabling_block_on_a_server_with_confirmed_rows_reconciles_them() {
    // A row confirmed under "audit" (nothing blocked)…
    let audit = lab_with("npm", true, false, quick());
    seed_population(&audit, 12);
    audit
        .inventory
        .cached(REG, "withdrawn", "1.0.0", long_ago());
    audit.upstream.gone("withdrawn");
    one(&audit).await;
    one(&audit).await;
    assert!(audit.status.row(REG, "withdrawn", None).is_some());

    // …and the same estate restarted under "block": the first sweep blocks
    // it without a new transition.
    let block = lab_with("npm", true, false, blocking());
    seed_population(&block, 12);
    block
        .inventory
        .cached(REG, "withdrawn", "1.0.0", long_ago());
    block.upstream.gone("withdrawn");
    let row = audit.status.row(REG, "withdrawn", None).unwrap();
    block
        .status
        .rows
        .lock()
        .unwrap()
        .insert((REG.into(), "withdrawn".into(), None), row);
    let r = one(&block).await;
    assert!(r.transitions.is_empty(), "already confirmed: {r:?}");
    let (by, _) = block
        .blocks
        .blocked_by(&PackageId::new(REG, "withdrawn", "1.0.0"))
        .expect("reconciled on the first sweep");
    assert_eq!(by, SYSTEM_ACTOR);
}

// ── the 0018 seam ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_confirmation_on_a_security_registry_queues_a_rescan_per_version() {
    let lab = lab_with("npm", true, true, quick());
    seed_population(&lab, 12);
    for v in ["1.0.0", "1.1.0"] {
        lab.inventory.cached(REG, "withdrawn", v, long_ago());
    }
    lab.upstream.gone("withdrawn");
    one(&lab).await;
    assert!(
        lab.queue.jobs.lock().unwrap().is_empty(),
        "a silent miss queues nothing"
    );
    one(&lab).await;
    let jobs = lab.queue.jobs.lock().unwrap().clone();
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().all(|(_, t)| *t == ScanTrigger::Rescan));
    assert!(jobs.iter().any(|(p, _)| p.version == "1.0.0"));
}

#[tokio::test]
async fn a_registry_without_security_queues_nothing() {
    let lab = lab_with("npm", true, false, quick());
    seed_population(&lab, 12);
    lab.inventory.cached(REG, "withdrawn", "1.0.0", long_ago());
    lab.upstream.gone("withdrawn");
    one(&lab).await;
    one(&lab).await;
    assert!(lab.queue.jobs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_presence_scanner_reads_the_row_and_never_probes() {
    use crate::ports::{ArtifactScanner, ScanInput};
    use crate::services::UpstreamPresenceScanner;

    let status = RecordingStatus::new();
    let now = Utc::now();
    status
        .record_miss(MissObservation {
            key: UpstreamKey::package(REG, "withdrawn"),
            at: now,
            error: None,
        })
        .await
        .unwrap();
    let input = |name: &str| ScanInput {
        package: PackageMetadata::minimal(
            PackageId::new(REG, name, "1.0.0"),
            serde_json::Value::Null,
        ),
        kind: crate::entities::RegistryKind::Npm,
        purl: String::new(),
        artifact: None,
        sbom: None,
        listing: None,
    };
    let audit = UpstreamPresenceScanner {
        status: Arc::clone(&status) as Arc<dyn UpstreamStatusPort>,
        deny: false,
    };
    assert!(
        audit.scan(&input("withdrawn")).await.unwrap().is_empty(),
        "missing is not confirmed"
    );
    status
        .confirm(&UpstreamKey::package(REG, "withdrawn"), now)
        .await
        .unwrap();
    let f = audit.scan(&input("withdrawn")).await.unwrap();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, crate::entities::ReasonCode::UnpublishedUpstream);
    assert_eq!(
        f[0].severity,
        crate::entities::Severity::Low,
        "audit mode records, never holds"
    );
    let block = UpstreamPresenceScanner {
        status: Arc::clone(&status) as Arc<dyn UpstreamStatusPort>,
        deny: true,
    };
    assert_eq!(
        block.scan(&input("withdrawn")).await.unwrap()[0].severity,
        crate::entities::Severity::High
    );
    assert!(block.scan(&input("other")).await.unwrap().is_empty());
}

// ── RFC 0014 §13.5: the path-addressed kinds, probed per file ────────────────

fn seed_files(lab: &Lab, n: usize) {
    for i in 0..n {
        let path = format!("pool/main/s/steady/steady_{i}.deb");
        lab.inventory.cached_file(REG, &path, long_ago());
        lab.upstream.has_file(&path);
    }
}

fn file_id(path: &str) -> PackageId {
    PackageId::new(REG, "repo", "_").with_artifact(path)
}

#[test]
fn a_path_kinds_held_version_is_the_file_read_off_its_key() {
    let row = |key: &str| ArtifactMeta {
        artifact_key: key.into(),
        registry: REG.into(),
        package_name: "repo".into(),
        version: "_".into(),
        size_bytes: None,
        cached_at: long_ago(),
        last_accessed_at: long_ago(),
    };
    assert_eq!(
        held_version(
            RegistryKind::Deb,
            &row(&format!("artifact:{REG}/repo/_/dists/stable/Release"))
        ),
        Some("dists/stable/Release".into())
    );
    assert_eq!(
        held_version(
            RegistryKind::Generic,
            &row(&format!("{REG}/repo/_/node/v20/node.tar.gz"))
        ),
        Some("node/v20/node.tar.gz".into())
    );
    assert_eq!(
        held_version(RegistryKind::Rpm, &row(&format!("{REG}/repo/_/"))),
        None
    );
    assert_eq!(
        held_version(RegistryKind::Rpm, &row(&format!("{REG}/other/1.0"))),
        None
    );
    // A kind with a package identity keeps its version.
    let mut npm = row(&format!("{REG}/left-pad/1.3.0"));
    npm.package_name = "left-pad".into();
    npm.version = "1.3.0".into();
    assert_eq!(held_version(RegistryKind::Npm, &npm), Some("1.3.0".into()));
    assert_eq!(
        audit_coordinate(RegistryKind::Deb, REG, "repo", "pool/x.deb"),
        file_id("pool/x.deb")
    );
    assert_eq!(
        audit_coordinate(RegistryKind::Npm, REG, "left-pad", "1.3.0"),
        PackageId::new(REG, "left-pad", "1.3.0")
    );
}

#[tokio::test]
async fn a_vanished_file_of_a_path_kind_is_confirmed_under_its_path_and_blocked_as_that_file() {
    let lab = lab_with("deb", false, false, blocking());
    seed_files(&lab, 12);
    let gone = "pool/main/x/x_1.0_amd64.deb";
    lab.inventory.cached_file(REG, gone, long_ago());
    lab.upstream.lost_file(gone);

    let r1 = one(&lab).await;
    assert!(!r1.void, "{r1:?}");
    assert_eq!((r1.missing, r1.inconclusive), (1, 0));
    assert!(r1.transitions.is_empty());
    let row = lab
        .status
        .row(REG, "repo", Some(gone))
        .expect("the miss is filed under the file");
    assert_eq!(row.consecutive_misses, 1);
    assert!(lab.blocks.blocked_by(&file_id(gone)).is_none());

    let r2 = one(&lab).await;
    assert_eq!(r2.confirmed(), 1, "{r2:?}");
    let (by, _) = lab
        .blocks
        .blocked_by(&file_id(gone))
        .expect("the file is blocked");
    assert_eq!(by, SYSTEM_ACTOR);
    assert!(
        lab.blocks
            .blocked_by(&PackageId::new(REG, "repo", "_"))
            .is_none(),
        "the bare coordinate is every file of the registry and must not be blocked"
    );
    assert!(
        lab.blocks
            .blocked_by(&file_id("pool/main/s/steady/steady_0.deb"))
            .is_none(),
        "a file upstream still has is untouched"
    );
    let events = lab.sink.events();
    let e = events
        .iter()
        .find(|e| e.event_type == NotificationEventType::PackageDisappearedUpstream)
        .expect("one disappearance event");
    assert_eq!(e.package_name, "repo");
    assert_eq!(e.version.as_deref(), Some(gone));
    assert_eq!(e.metadata["probe"], "artifact");
    assert_eq!(e.metadata["blocked"], true);

    // Back upstream: the audit's own block is lifted.
    lab.upstream.has_file(gone);
    let r3 = one(&lab).await;
    assert_eq!(r3.reappeared(), 1, "{r3:?}");
    assert!(lab.blocks.blocked_by(&file_id(gone)).is_none());
}

#[tokio::test]
async fn a_path_upstream_that_cannot_answer_the_probe_is_inconclusive() {
    let lab = lab_with("generic", false, false, quick());
    seed_files(&lab, 12);
    let flaky = "node/v20.0.0/node.tar.gz";
    lab.inventory.cached_file(REG, flaky, long_ago());
    lab.upstream.failing(flaky, true);
    let r = one(&lab).await;
    // `repo` is one package: the first file that fails to answer makes the
    // whole package inconclusive, as rung 3 does.
    assert_eq!((r.missing, r.inconclusive), (0, 1), "{r:?}");
    assert!(lab.status.row(REG, "repo", Some(flaky)).is_none());
}

#[tokio::test]
async fn a_path_kinds_recheck_selects_one_file_by_its_path() {
    let lab = lab_with("rpm", false, false, quick());
    seed_files(&lab, 12);
    let gone = "Packages/x/x-1.0.rpm";
    lab.inventory.cached_file(REG, gone, long_ago());
    lab.upstream.lost_file(gone);
    let r = lab.svc.recheck(REG, "repo", Some(gone)).await.unwrap();
    assert_eq!((r.probed, r.missing), (1, 1), "{r:?}");
    assert!(lab.status.row(REG, "repo", Some(gone)).is_some());
    let err = lab.svc.recheck(REG, "repo", Some("nothing/held")).await;
    assert!(matches!(err, Err(CoreError::NotFound(_))));
}

// ── RFC 0014 §13 O6: the registry-tier `on_confirmed` ───────────────────────

/// The lab's registry with a registry-tier row of its own.
async fn set_registry_tier(lab: &Lab, on_confirmed: OnConfirmed) {
    use crate::entities::RegistryPolicyTiers;
    let mut tiers = RegistryPolicyTiers::open(RegistryKind::Npm, REG);
    tiers.registry.on_confirmed = Some(on_confirmed);
    lab.svc
        .hot
        .write()
        .await
        .policy_tiers
        .insert(REG.into(), Arc::new(tiers));
}

#[tokio::test]
async fn a_registry_tier_block_under_an_estate_wide_audit_blocks_that_registry() {
    // The estate says audit; the pen is handed over regardless.
    let lab = lab_with("npm", true, false, quick());
    *lab.blocks.refuse_writes.lock().unwrap() = false;
    set_registry_tier(&lab, OnConfirmed::Block).await;
    assert!(!lab.svc.blocks(), "the estate key is still audit");
    assert!(
        lab.svc.blocks_for(REG).await,
        "the registry's own row says block"
    );
    assert_eq!(lab.svc.policy_for(REG).await, OnConfirmed::Block);
    assert_eq!(
        lab.svc.policy_for("some-other").await,
        OnConfirmed::Audit,
        "a registry without a row inherits the estate's key"
    );

    seed_population(&lab, 12);
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]);
    one(&lab).await;
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 1);
    let (by, _) = lab
        .blocks
        .blocked_by(&PackageId::new(REG, "left-pad", "1.3.1"))
        .expect("blocked under the registry's row");
    assert_eq!(by, SYSTEM_ACTOR);
    let events = lab.sink.events();
    let e = events
        .iter()
        .find(|e| e.event_type == NotificationEventType::PackageDisappearedUpstream)
        .unwrap();
    assert_eq!(
        e.metadata["policy"], "block",
        "the event names the effective policy"
    );
    assert_eq!(e.metadata["blocked"], true);
}

#[tokio::test]
async fn a_registry_tier_audit_under_an_estate_wide_block_leaves_that_registry_alone() {
    let lab = lab_with("npm", true, false, blocking());
    set_registry_tier(&lab, OnConfirmed::Audit).await;
    assert!(lab.svc.blocks(), "the estate key is block");
    assert!(
        !lab.svc.blocks_for(REG).await,
        "this registry's row says audit"
    );

    seed_population(&lab, 12);
    lab.inventory.cached(REG, "left-pad", "1.3.1", long_ago());
    lab.upstream.has("left-pad", &["1.3.0"]);
    one(&lab).await;
    let r = one(&lab).await;
    assert_eq!(r.confirmed(), 1);
    assert!(
        lab.blocks
            .blocked_by(&PackageId::new(REG, "left-pad", "1.3.1"))
            .is_none(),
        "audit-only here, whatever the estate says"
    );
    let events = lab.sink.events();
    let e = events
        .iter()
        .find(|e| e.event_type == NotificationEventType::PackageDisappearedUpstream)
        .unwrap();
    assert_eq!(e.metadata["policy"], "audit");
    assert_eq!(e.metadata["blocked"], false);

    // The reconciliation pass is gated the same way: a confirmed row on an
    // audit-only registry is not blocked after the fact.
    one(&lab).await;
    assert!(lab
        .blocks
        .blocked_by(&PackageId::new(REG, "left-pad", "1.3.1"))
        .is_none());
}
