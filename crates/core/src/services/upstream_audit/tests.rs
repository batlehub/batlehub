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
    MissObservation, PackageId, PackageMetadata, ScanJob, SecurityPolicy, UpstreamKey,
    UpstreamStatusFilter,
};
use crate::ports::{
    ArtifactInventory, ArtifactMeta, FetchedArtifact, QueuedCount, RegistryClient, ScanQueue,
};
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
}

impl ScriptedRegistry {
    fn new(kind: &'static str, lists: bool) -> Arc<Self> {
        Arc::new(Self {
            kind,
            listing: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashSet::new()),
            lists,
        })
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

// ── the lab ──────────────────────────────────────────────────────────────────

const REG: &str = "npm-proxy";

struct Lab {
    upstream: Arc<ScriptedRegistry>,
    inventory: Arc<SeededInventory>,
    status: Arc<RecordingStatus>,
    queue: Arc<RecordingQueue>,
    svc: UpstreamAuditService,
}

fn lab_with(kind: &'static str, lists: bool, secured: bool, policy: UpstreamAuditPolicy) -> Lab {
    let upstream = ScriptedRegistry::new(kind, lists);
    let inventory = SeededInventory::new();
    let status = RecordingStatus::new();
    let queue = Arc::new(RecordingQueue::default());
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
    );
    Lab {
        upstream,
        inventory,
        status,
        queue,
        svc,
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
