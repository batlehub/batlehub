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

use crate::entities::{PackageId, RegistryKind, ScanTrigger, UpstreamState, UpstreamStatus};
use crate::error::CoreError;
use crate::ports::{ArtifactInventory, CacheStore, RegistryClient, ScanQueue, UpstreamStatusPort};
use crate::services::HotConfigLock;

pub use confirm::Transition;
pub use probe::{probe_package, ProbeOutcome, MAX_VERSION_PROBES_PER_PACKAGE};

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
            last_started: Mutex::new(None),
        }
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
        for (name, (versions, newest)) in packages {
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

        if !report.void {
            self.act(registry, secured, &report.transitions, &applied.disappeared)
                .await;
        }
        report.duration = clock.elapsed();
        report
    }

    /// What a transition does beyond the row: pin the metadata of a held
    /// coordinate and queue a rescan on a `[security]` registry.
    async fn act(
        &self,
        registry: &str,
        secured: bool,
        transitions: &[Transition],
        disappeared: &[UpstreamStatus],
    ) {
        if self.policy.retain_disappeared {
            for row in disappeared {
                self.pin_metadata(row).await;
            }
        }
        if !secured {
            return;
        }
        let Some(queue) = &self.queue else {
            return;
        };
        for t in transitions {
            let (row, versions) = match t {
                Transition::Confirmed(row, versions) | Transition::Reappeared(row, versions) => {
                    (row, versions)
                }
            };
            for v in versions {
                let id = PackageId::new(registry, &row.package_name, v);
                if let Err(e) = queue.enqueue(&id, None, ScanTrigger::Rescan).await {
                    tracing::warn!(package = %id, error = %e, "upstream audit: could not queue the rescan");
                }
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
