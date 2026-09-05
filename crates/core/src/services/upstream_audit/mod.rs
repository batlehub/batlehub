//! The upstream audit (RFC 0014): a periodic sweep that asks each proxy or
//! hybrid upstream whether the artifacts cached from it are still there,
//! confirms a disappearance across several sweeps before believing it, and
//! holds a confirmed artifact back from eviction so the last copy in the
//! estate is not garbage-collected precisely because upstream stopped
//! refreshing it.
//!
//! Re-based onto RFC 0018's worker (0014 §13, 0018 decision 29): the sweep
//! runs on the `worker` role, its concurrency is `[worker].max_concurrent`,
//! and a confirmation on a `[security]` registry queues a rescan so the
//! verdict picks up the `UNPUBLISHED_UPSTREAM` finding the
//! [`crate::services::UpstreamPresenceScanner`] derives from the row. The
//! state machine — population gate, confirmation window, hold, reappearance
//! — lives here, keyed on the `upstream_status` rows.
//!
//! The split follows `services/warming/`: this file is the service and its
//! report, `probe.rs` is one package's probe, `confirm.rs` the state machine.

mod confirm;
mod probe;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Semaphore};

use crate::entities::{
    Identity, NotificationEvent, NotificationEventType, PackageId, PackageStatus, RegistryKind,
    Role, ScanTrigger, UpstreamState, UpstreamStatus, UpstreamStatusFilter,
};
use crate::error::CoreError;
use crate::ports::{
    ArtifactInventory, CacheStore, NotificationSink, RegistryClient, ScanQueue, UpstreamStatusPort,
};
use crate::services::{AdminService, HotConfigLock};

pub use confirm::Transition;
pub use probe::{
    listing_capable, probe_name, probe_package, ProbeOutcome, MAX_VERSION_PROBES_PER_PACKAGE,
};

/// The actor every event this service emits carries (RFC 0014 §11 q12):
/// not a user id, and the `system:` prefix cannot collide with one. It is
/// also what the block arm records as `blocked_by` (§4.3), and the one
/// value the conditional unblock lifts.
pub const SYSTEM_ACTOR: &str = "system:upstream-audit";

/// The identity the block arm writes with (RFC 0014 §6.5): `user_id` is
/// what `block_package` records as `blocked_by`, and the `system:` prefix
/// is not a legal user id from any provider.
pub fn system_identity() -> Identity {
    Identity {
        user_id: Some(SYSTEM_ACTOR.to_owned()),
        role: Role::Admin,
        ..Identity::system()
    }
}

/// What a block write did for one confirmation, for the event (§4.5:
/// `policy` says what was configured, `blocked` what happened).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockOutcome {
    /// Every version the confirmation covers is now blocked.
    pub blocked: bool,
    /// The reappearance lifted this service's own block.
    pub unblocked: bool,
    /// A reappearance left a block in place because it was not this
    /// service's — an admin's decision, not to be reversed by a `200`.
    pub kept_admin_block: bool,
}

/// What a confirmed disappearance does beyond the row (`[upstream_audit]
/// on_confirmed`, RFC 0014 §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConfirmed {
    /// Record, hold, notify. The default, and the whole of phases 1–5.
    #[default]
    Audit,
    /// …and refuse the version on the wire through the admin block list
    /// (phase 6).
    Block,
}

impl OnConfirmed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audit => "audit",
            Self::Block => "block",
        }
    }
}

impl std::str::FromStr for OnConfirmed {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "audit" => Ok(Self::Audit),
            "block" => Ok(Self::Block),
            other => Err(format!("unknown on_confirmed policy '{other}'")),
        }
    }
}

/// §4.3's reason string on a block this audit writes.
fn block_reason(row: &UpstreamStatus) -> String {
    format!(
        "upstream disappearance confirmed {} ({} misses since {})",
        row.confirmed_at
            .unwrap_or(row.last_checked_at)
            .format("%Y-%m-%d"),
        row.consecutive_misses,
        row.first_missed_at.format("%Y-%m-%d")
    )
}

/// A registry's cached packages: name → (cached versions, newest `cached_at`).
pub type CachedPackages = HashMap<String, (Vec<String>, DateTime<Utc>)>;

/// Below this many probed packages the ratio gate is skipped (RFC 0014 §13
/// O4): one package *is* a quarter of a four-package registry, and the
/// count and age floors carry the decision alone. A constant, not config.
pub const MIN_PROBED_FOR_RATIO: usize = 10;

/// The thresholds, as `[upstream_audit]` sets them (RFC 0014 §4.1).
#[derive(Debug, Clone)]
pub struct UpstreamAuditPolicy {
    /// Consecutive valid-sweep misses a disappearance must survive.
    pub confirm_after: u32,
    /// …and at least this long since the first miss.
    pub confirm_min_age: Duration,
    /// Above this fraction of a registry's probed packages missing, the
    /// sweep is void for that registry.
    pub outage_ratio: f64,
    /// Hold confirmed artifacts back from eviction, and pin their metadata.
    pub retain_disappeared: bool,
    /// A package cached from upstream since the last sweep started was
    /// present then; skip it.
    pub skip_recently_seen: bool,
    /// How long a pinned metadata entry stays fresh: long enough to reach
    /// the next sweep, which pins it again.
    pub metadata_pin_ttl: Duration,
    /// What a confirmation does beyond the row. Reported on every event as
    /// `policy`, beside `blocked` — what was configured and what happened
    /// are two fields on purpose (§4.5).
    pub on_confirmed: OnConfirmed,
}

impl Default for UpstreamAuditPolicy {
    fn default() -> Self {
        Self {
            confirm_after: 3,
            confirm_min_age: Duration::from_secs(86_400),
            outage_ratio: 0.25,
            retain_disappeared: true,
            skip_recently_seen: true,
            metadata_pin_ttl: Duration::from_secs(2 * 21_600),
            on_confirmed: OnConfirmed::Audit,
        }
    }
}

/// One registry's sweep, as it went.
#[derive(Debug, Clone, Default)]
pub struct RegistryReport {
    pub registry: String,
    /// Packages the sweep asked upstream about.
    pub probed: usize,
    /// Packages (or versions of them) upstream denied.
    pub missing: usize,
    /// Packages upstream could not answer for — excluded from the ratio.
    pub inconclusive: usize,
    /// Packages skipped because real traffic already proved them present.
    pub skipped_recent: usize,
    /// The ratio gate fired: nothing was written.
    pub void: bool,
    /// Packages whose per-version probe hit [`MAX_VERSION_PROBES_PER_PACKAGE`]
    /// — a bound the operator cannot see is a bound that reads as full
    /// coverage.
    pub capped_packages: usize,
    pub transitions: Vec<Transition>,
    pub duration: Duration,
}

impl RegistryReport {
    pub fn confirmed(&self) -> usize {
        self.transitions
            .iter()
            .filter(|t| matches!(t, Transition::Confirmed(..)))
            .count()
    }
    pub fn reappeared(&self) -> usize {
        self.transitions
            .iter()
            .filter(|t| matches!(t, Transition::Reappeared(..)))
            .count()
    }
}

/// A whole sweep.
#[derive(Debug, Clone, Default)]
pub struct SweepReport {
    pub registries: Vec<RegistryReport>,
}

/// The sweep. Depends only on ports and the hot config, so it is unit
/// testable with the in-memory fakes the eviction and warming suites use.
pub struct UpstreamAuditService {
    pub inventory: Arc<dyn ArtifactInventory>,
    pub status: Arc<dyn UpstreamStatusPort>,
    /// Where the registry clients are, and which registries have a
    /// `[security]` profile.
    pub hot: HotConfigLock,
    /// The metadata cache, for the pin (RFC 0014 §13 O1). `None` pins
    /// nothing.
    pub cache: Option<Arc<dyn CacheStore>>,
    /// The scan queue, for the rescan a confirmation triggers on a
    /// `[security]` registry. `None` queues nothing.
    pub queue: Option<Arc<dyn ScanQueue>>,
    pub policy: UpstreamAuditPolicy,
    /// Simultaneous upstream probes — `[worker].max_concurrent`.
    pub concurrency: usize,
    /// The registries to audit: every `proxy`/`hybrid` registry, or the
    /// operator's list. A local registry has no upstream and is never here.
    pub registries: Vec<String>,
    /// Where a transition is reported (RFC 0014 §4.5). `None` tells nobody,
    /// which is what a process with notifications disabled asked for.
    notifier: Option<Arc<dyn NotificationSink>>,
    /// The block arm's pen (RFC 0014 §6.5): `AdminService::block_package`,
    /// so a block written here is in shape and in the audit trail exactly
    /// an admin's. `None` under `"audit"` — the default is inert by
    /// construction, not by a branch.
    admin: Option<Arc<AdminService>>,
    /// When the previous sweep started, for `skip_recently_seen`.
    last_started: Mutex<Option<DateTime<Utc>>>,
}

impl UpstreamAuditService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inventory: Arc<dyn ArtifactInventory>,
        status: Arc<dyn UpstreamStatusPort>,
        hot: HotConfigLock,
        cache: Option<Arc<dyn CacheStore>>,
        queue: Option<Arc<dyn ScanQueue>>,
        policy: UpstreamAuditPolicy,
        concurrency: usize,
        registries: Vec<String>,
    ) -> Self {
        Self {
            inventory,
            status,
            hot,
            cache,
            queue,
            policy,
            concurrency: concurrency.max(1),
            registries,
            notifier: None,
            admin: None,
            last_started: Mutex::new(None),
        }
    }

    /// Report transitions through `sink` (RFC 0014 §4.5).
    pub fn with_notifier(mut self, sink: Arc<dyn NotificationSink>) -> Self {
        self.notifier = Some(sink);
        self
    }

    /// Block through `admin` under `on_confirmed = "block"` (RFC 0014 §4.3,
    /// §6.5). Ignored under `"audit"`.
    pub fn with_admin(mut self, admin: Arc<AdminService>) -> Self {
        if self.policy.on_confirmed == OnConfirmed::Block {
            self.admin = Some(admin);
        }
        self
    }

    /// Whether this service blocks on confirmation.
    pub fn blocks(&self) -> bool {
        self.admin.is_some()
    }

    /// Sweep every audited registry. One registry's failure is reported and
    /// does not stop the others.
    pub async fn run_sweep(&self) -> SweepReport {
        let started = Utc::now();
        let previous = self.last_started.lock().await.replace(started);
        let all = match self.inventory.list_artifacts_by_package().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "upstream audit: could not list the cache; sweep skipped");
                return SweepReport::default();
            }
        };
        // registry → package → (versions, newest cached_at)
        let mut by_registry: HashMap<String, CachedPackages> = HashMap::new();
        for row in all {
            if !self.registries.contains(&row.registry) {
                continue;
            }
            let entry = by_registry
                .entry(row.registry)
                .or_default()
                .entry(row.package_name)
                .or_insert_with(|| (Vec::new(), row.cached_at));
            entry.0.push(row.version);
            entry.1 = entry.1.max(row.cached_at);
        }
        let mut report = SweepReport::default();
        for registry in &self.registries {
            let packages = by_registry.remove(registry).unwrap_or_default();
            report
                .registries
                .push(self.sweep_registry(registry, packages, previous).await);
        }
        report
    }

    /// One registry: probe, gate, apply, act.
    pub async fn sweep_registry(
        &self,
        registry: &str,
        packages: CachedPackages,
        previous_start: Option<DateTime<Utc>>,
    ) -> RegistryReport {
        let clock = Instant::now();
        let mut report = RegistryReport {
            registry: registry.to_owned(),
            ..Default::default()
        };
        let (client, kind, secured) = {
            let hot = self.hot.read().await;
            let Some(client) = hot.registries.get(registry).cloned() else {
                tracing::warn!(registry, "upstream audit: no client for registry; skipped");
                return report;
            };
            let kind: Option<RegistryKind> = client.registry_type().parse().ok();
            (client, kind, hot.security.contains_key(registry))
        };
        let Some(kind) = kind else {
            return report;
        };

        // Probe, bounded.
        let sem = Arc::new(Semaphore::new(self.concurrency));
        let mut handles = Vec::new();
        // The newest `cached_at` per package, for the event payload (§4.5),
        // and the versions held per package, for the block arm's scope
        // (§4.3: a package-level confirmation blocks every held version).
        let mut cached_at: HashMap<String, DateTime<Utc>> = HashMap::new();
        let mut held_versions: HashMap<String, Vec<String>> = HashMap::new();
        for (name, (versions, newest)) in packages {
            cached_at.insert(name.clone(), newest);
            held_versions.insert(name.clone(), versions.clone());
            if self.policy.skip_recently_seen && previous_start.is_some_and(|p| newest > p) {
                report.skipped_recent += 1;
                continue;
            }
            let sem = Arc::clone(&sem);
            let client: Arc<dyn RegistryClient> = Arc::clone(&client);
            let registry = registry.to_owned();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.ok();
                let outcome =
                    probe_package(client.as_ref(), kind, &registry, &name, &versions).await;
                (name, versions, outcome)
            }));
        }
        let mut outcomes = Vec::new();
        for h in handles {
            if let Ok(o) = h.await {
                outcomes.push(o);
            }
        }
        report.probed = outcomes.len();

        let now = Utc::now();
        let applied =
            confirm::apply(self.status.as_ref(), registry, &outcomes, &self.policy, now).await;
        report.missing = applied.missing;
        report.inconclusive = applied.inconclusive;
        report.capped_packages = applied.capped;
        report.void = applied.void;
        report.transitions = applied.transitions;

        if report.void {
            self.notify_void(&report);
        } else {
            let outcomes_by_row = self
                .act(registry, secured, &report.transitions, &applied.disappeared)
                .await;
            // §6.5: a confirmation that crashed between the status write and
            // the block is reconciled by the next sweep — every `disappeared`
            // row is re-checked against the block table, on every sweep.
            self.reconcile(registry, &held_versions).await;
            let probes: HashMap<&str, &'static str> = outcomes
                .iter()
                .map(|(name, _, (outcome, _))| (name.as_str(), probe_name(kind, outcome)))
                .collect();
            self.notify_transitions(&report, &probes, &cached_at, &outcomes_by_row);
        }
        report.duration = clock.elapsed();
        report
    }

    /// The reconciliation pass (RFC 0014 §6.5): under `"block"`, every
    /// `disappeared` row of the registry whose versions are not all blocked
    /// gets blocked now. Idempotent, and what makes turning the policy on
    /// with existing confirmed rows do the obvious thing.
    async fn reconcile(&self, registry: &str, held: &HashMap<String, Vec<String>>) {
        let Some(admin) = &self.admin else {
            return;
        };
        let rows = match self
            .status
            .list(UpstreamStatusFilter {
                registry: Some(registry.to_owned()),
                state: Some(UpstreamState::Disappeared),
                ..Default::default()
            })
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(registry, error = %e, "upstream audit: could not list confirmed rows to reconcile");
                return;
            }
        };
        for row in rows {
            let versions: Vec<String> = match &row.version {
                Some(v) => vec![v.clone()],
                None => held.get(&row.package_name).cloned().unwrap_or_default(),
            };
            for v in versions {
                let id = PackageId::new(registry, &row.package_name, &v);
                match admin.repo.get_status(&id).await {
                    Ok(PackageStatus::Blocked { .. }) => {}
                    Ok(_) => {
                        tracing::info!(package = %id, "upstream audit: confirmed and unblocked; reconciling");
                        if let Err(e) = admin
                            .block_package(&id, block_reason(&row), &system_identity())
                            .await
                        {
                            tracing::warn!(package = %id, error = %e, "upstream audit: reconciliation block failed");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(package = %id, error = %e, "upstream audit: could not read the block status")
                    }
                }
            }
        }
    }

    /// RFC 0014 §4.5: one event per transition — per *package* when the
    /// whole package went, not per version — and none for a silent miss.
    /// A reappearance is news only for a row that had reached
    /// `disappeared`: an unconfirmed miss that clears was never reported,
    /// so its clearing is not a reversal anybody heard about.
    fn notify_transitions(
        &self,
        report: &RegistryReport,
        probes: &HashMap<&str, &'static str>,
        cached_at: &HashMap<String, DateTime<Utc>>,
        outcomes: &[BlockOutcome],
    ) {
        let Some(sink) = &self.notifier else {
            return;
        };
        let sweep = serde_json::json!({
            "probed": report.probed,
            "missing": report.missing,
            "inconclusive": report.inconclusive,
        });
        for (i, t) in report.transitions.iter().enumerate() {
            let outcome = outcomes.get(i).copied().unwrap_or_default();
            let (kind, row, versions) = match t {
                Transition::Confirmed(row, versions) => (
                    NotificationEventType::PackageDisappearedUpstream,
                    row,
                    versions,
                ),
                Transition::Reappeared(row, versions) => {
                    if row.state != UpstreamState::Disappeared {
                        continue;
                    }
                    (
                        NotificationEventType::PackageReappearedUpstream,
                        row,
                        versions,
                    )
                }
            };
            let mut event = NotificationEvent::new(
                kind,
                &row.registry,
                &row.package_name,
                row.version.clone(),
                SYSTEM_ACTOR,
            );
            let mut metadata = serde_json::json!({
                "first_missed_at": row.first_missed_at,
                "confirmed_at": row.confirmed_at,
                "consecutive_misses": row.consecutive_misses,
                "probe": probes.get(row.package_name.as_str()).copied().unwrap_or("none"),
                "cached_at": cached_at.get(&row.package_name),
                "versions": versions,
                "policy": self.policy.on_confirmed.as_str(),
                "sweep": sweep,
            });
            match kind {
                NotificationEventType::PackageDisappearedUpstream => {
                    metadata["held_from_eviction"] =
                        serde_json::Value::Bool(self.policy.retain_disappeared);
                    // What happened, beside what was configured (§4.5): they
                    // differ when the block write failed.
                    metadata["blocked"] = serde_json::Value::Bool(outcome.blocked);
                }
                _ => {
                    metadata["unblocked"] = serde_json::Value::Bool(outcome.unblocked);
                    if outcome.kept_admin_block {
                        // §4.4: a manual decision is now the only thing
                        // keeping it blocked, and the admin should know.
                        metadata["unblock_skipped_reason"] =
                            serde_json::Value::String("blocked_by_admin".to_owned());
                    }
                }
            }
            event.metadata = metadata;
            sink.emit(event);
        }
    }

    /// RFC 0014 §4.5 `upstream_unreachable`: registry-scoped, `package_name`
    /// `"*"`, so a subscription with no package filter matches it and a
    /// per-package one does not.
    fn notify_void(&self, report: &RegistryReport) {
        let Some(sink) = &self.notifier else {
            return;
        };
        let mut event = NotificationEvent::new(
            NotificationEventType::UpstreamUnreachable,
            &report.registry,
            "*",
            None,
            SYSTEM_ACTOR,
        );
        event.metadata = serde_json::json!({
            "probed": report.probed,
            "missing": report.missing,
            "inconclusive": report.inconclusive,
            "outage_ratio": self.policy.outage_ratio,
        });
        sink.emit(event);
    }

    /// What a transition does beyond the row: pin the metadata of a held
    /// coordinate, block or unblock under `"block"` (§4.3, §6.5 — the row
    /// is already written, so a crash here is reconciled by the next
    /// sweep), and queue a rescan on a `[security]` registry. Returns the
    /// block arm's outcome per transition, in order, for the events.
    async fn act(
        &self,
        registry: &str,
        secured: bool,
        transitions: &[Transition],
        disappeared: &[UpstreamStatus],
    ) -> Vec<BlockOutcome> {
        if self.policy.retain_disappeared {
            for row in disappeared {
                self.pin_metadata(row).await;
            }
        }
        let mut outcomes = Vec::with_capacity(transitions.len());
        for t in transitions {
            outcomes.push(self.apply_block_policy(registry, t).await);
        }
        if !secured {
            return outcomes;
        }
        let Some(queue) = &self.queue else {
            return outcomes;
        };
        for t in transitions {
            let (row, versions) = match t {
                Transition::Confirmed(row, versions) | Transition::Reappeared(row, versions) => {
                    (row, versions)
                }
            };
            for v in versions {
                let id = PackageId::new(registry, &row.package_name, v);
                // With the version's date when the metadata cache still has
                // it: the worker's age gate reads it, and a rescan without
                // one would re-judge a dated version as `TIMESTAMP_MISSING`.
                let published_at = match &self.cache {
                    Some(cache) => match cache
                        .get_stale(&crate::services::proxy::proxy_meta_key(&id))
                        .await
                    {
                        Ok(Some(entry)) => entry.metadata.published_at,
                        _ => None,
                    },
                    None => None,
                };
                if let Err(e) = queue.enqueue(&id, published_at, ScanTrigger::Rescan).await {
                    tracing::warn!(package = %id, error = %e, "upstream audit: could not queue the rescan");
                }
            }
        }
        outcomes
    }

    /// The block arm for one transition (RFC 0014 §4.3). A confirmation
    /// blocks every version it covers, with §4.3's reason and
    /// `blocked_by = system:upstream-audit`; `blocked` is true only when
    /// every write succeeded. A reappearance unblocks a version only when
    /// the block is this service's own — an admin's block, or one this
    /// service wrote and an admin then edited, stays, and the event says so.
    async fn apply_block_policy(&self, registry: &str, t: &Transition) -> BlockOutcome {
        let Some(admin) = &self.admin else {
            return BlockOutcome::default();
        };
        let identity = system_identity();
        match t {
            Transition::Confirmed(row, versions) => {
                let mut all = true;
                for v in versions {
                    let id = PackageId::new(registry, &row.package_name, v);
                    if let Err(e) = admin.block_package(&id, block_reason(row), &identity).await {
                        tracing::warn!(package = %id, error = %e, "upstream audit: block failed");
                        all = false;
                    }
                }
                BlockOutcome {
                    blocked: all && !versions.is_empty(),
                    ..Default::default()
                }
            }
            Transition::Reappeared(row, versions) => {
                if row.state != UpstreamState::Disappeared {
                    return BlockOutcome::default();
                }
                let mut outcome = BlockOutcome::default();
                let mut lifted = 0usize;
                for v in versions {
                    let id = PackageId::new(registry, &row.package_name, v);
                    match admin.repo.get_status(&id).await {
                        Ok(PackageStatus::Blocked { blocked_by, .. })
                            if blocked_by == SYSTEM_ACTOR =>
                        {
                            match admin.unblock_package(&id, &identity).await {
                                Ok(()) => lifted += 1,
                                Err(e) => {
                                    tracing::warn!(package = %id, error = %e, "upstream audit: unblock failed")
                                }
                            }
                        }
                        Ok(PackageStatus::Blocked { blocked_by, .. }) => {
                            tracing::warn!(
                                package = %id,
                                blocked_by,
                                "upstream audit: reappeared upstream, but the block is not this audit's; left in place"
                            );
                            outcome.kept_admin_block = true;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(package = %id, error = %e, "upstream audit: could not read the block status")
                        }
                    }
                }
                outcome.unblocked = lifted > 0 && lifted == versions.len();
                outcome
            }
        }
    }

    /// Re-store the coordinate's cached metadata with a fresh TTL (RFC 0014
    /// §13 O1): a `disappeared` row pins the stale copy, otherwise the
    /// listing forgets the version one layer above the artifact the hold
    /// keeps. Package-level rows pin every cached version's entry through
    /// the version rows the sweep also wrote; here, the one the row names.
    async fn pin_metadata(&self, row: &UpstreamStatus) {
        let Some(cache) = &self.cache else {
            return;
        };
        let Some(version) = &row.version else {
            return;
        };
        if row.state != UpstreamState::Disappeared {
            return;
        }
        let id = PackageId::new(&row.registry, &row.package_name, version);
        let key = crate::services::proxy::proxy_meta_key(&id);
        match cache.get_stale(&key).await {
            Ok(Some(entry)) => {
                if let Err(e) = cache
                    .set(&key, entry, Some(self.policy.metadata_pin_ttl))
                    .await
                {
                    tracing::warn!(package = %id, error = %e, "upstream audit: could not pin the metadata");
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(package = %id, error = %e, "upstream audit: metadata cache unreadable")
            }
        }
    }

    /// Probe one package now, through the same ladder and state machine a
    /// sweep uses (RFC 0014 §4.6 `recheck`). A probe, not an override: an
    /// admin who has just confirmed with upstream that a package is back
    /// does not wait for the next interval, and a package still missing
    /// gains one more miss towards confirmation — never a confirmation the
    /// count and age floors would not grant. Below the population floor by
    /// construction, so the ratio gate never voids it.
    ///
    /// `NotFound` for a registry the audit does not cover and for a package
    /// (or version) with nothing cached: there is nothing to ask upstream
    /// about, and the row is the cache's, not the operator's.
    pub async fn recheck(
        &self,
        registry: &str,
        package: &str,
        version: Option<&str>,
    ) -> Result<RegistryReport, CoreError> {
        if !self.registries.iter().any(|r| r == registry) {
            return Err(CoreError::NotFound(format!(
                "registry '{registry}' is not audited ([upstream_audit] registries)"
            )));
        }
        let mut packages: CachedPackages = HashMap::new();
        for row in self.inventory.list_artifacts(registry).await? {
            if row.package_name != package || version.is_some_and(|v| row.version != v) {
                continue;
            }
            let entry = packages
                .entry(row.package_name)
                .or_insert_with(|| (Vec::new(), row.cached_at));
            entry.0.push(row.version);
            entry.1 = entry.1.max(row.cached_at);
        }
        if packages.is_empty() {
            return Err(CoreError::NotFound(format!(
                "nothing cached for {registry}/{package}{}",
                version.map(|v| format!("@{v}")).unwrap_or_default()
            )));
        }
        Ok(self.sweep_registry(registry, packages, None).await)
    }

    /// The per-registry counts, for the gauges.
    pub async fn counts(&self, registry: &str) -> Result<(u64, u64), CoreError> {
        use crate::entities::UpstreamStatusFilter;
        let missing = self
            .status
            .count(UpstreamStatusFilter {
                registry: Some(registry.to_owned()),
                state: Some(UpstreamState::Missing),
                ..Default::default()
            })
            .await?;
        let disappeared = self
            .status
            .count(UpstreamStatusFilter {
                registry: Some(registry.to_owned()),
                state: Some(UpstreamState::Disappeared),
                ..Default::default()
            })
            .await?;
        Ok((missing, disappeared))
    }
}
