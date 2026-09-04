//! Discarding *cached copies* — never the only copy.
//!
//! # Why this shares no code with `services::retention`
//!
//! The asymmetry is set out in that module's header and it decides everything
//! here too, in the other direction: what eviction drops is recoverable by a
//! re-fetch, so the defaults are permissive where retention's are protective,
//! and **the trail is per-run where retention's is per-version**. An LRU sweep
//! evicts by the thousand; one audit row per evicted blob would bury the
//! deletions that are not recoverable under the ones that are.
//!
//! # What a run leaves behind
//!
//! | | Live run | Dry run |
//! | --- | --- | --- |
//! | Registry-scoped run event | `cache_evict_run` | `cache_evict_dry_run` |
//! | Per-artifact event | none — see above | none |
//! | The keys | [`EvictionReport::evicted_keys`], bounded | same |
//!
//! A single artifact dropped **by hand** is `cache_evict` and does carry its
//! coordinate: that one is an operator's decision about one package, and there
//! is one of it.

mod report;
pub use report::{CoherenceReport, EvictionReport, MAX_REPORTED_KEYS};

use std::sync::Arc;

use chrono::{Duration, Utc};

use crate::entities::{AccessAction, AccessEvent, AccessResult, Identity, PackageId};
use crate::error::CoreError;
use crate::ports::{ArtifactMeta, ArtifactMetaRepository, PackageRepository, StorageBackend};

/// Configuration for the eviction service. All fields are optional; omitting a
/// field disables that eviction strategy.
#[derive(Debug, Clone, Default)]
pub struct EvictionConfig {
    /// Evict artifacts whose `cached_at` is older than this many seconds.
    pub artifact_ttl_secs: Option<u64>,
    /// Evict artifacts not accessed for this many days.
    pub idle_days: Option<u64>,
    /// When total storage for a registry exceeds this byte count, evict the
    /// least-recently-used artifacts until usage falls below the threshold.
    pub max_size_bytes: Option<u64>,
    /// Keep only the N most-recently-cached versions per (registry, package).
    pub keep_latest_n: Option<usize>,
    /// Registry name to scope eviction to. Pass `""` to run across all registries.
    pub registry: String,
}

impl EvictionConfig {
    /// Whether any eviction strategy is configured — i.e. whether a sweep would
    /// do anything at all.
    ///
    /// The handler's `404 "eviction not configured"` and the wiring in
    /// `server/src/setup.rs` ask the same question, and asked it twice in two
    /// places until the coherence sweep needed a service for *every* registry:
    /// orphaned blobs do not wait for an eviction policy to be configured.
    pub fn evicts_anything(&self) -> bool {
        self.artifact_ttl_secs.is_some()
            || self.idle_days.is_some()
            || self.max_size_bytes.is_some()
            || self.keep_latest_n.is_some()
    }
}

/// Drives artifact eviction across storage and artifact-meta.
pub struct EvictionService {
    pub artifact_meta: Arc<dyn ArtifactMetaRepository>,
    pub storage: Arc<dyn StorageBackend>,
    pub config: EvictionConfig,
    /// Storage keys that looked orphaned during the *previous* coherence run.
    /// The coherence sweep only deletes a blob that is orphaned on two
    /// consecutive runs, which closes the write/delete race with `fetch_and_cache`
    /// (whose `store` → `record_artifact` window is milliseconds, always far
    /// shorter than the coherence interval) without cross-service locking.
    coherence_pending: tokio::sync::Mutex<std::collections::HashSet<String>>,
    /// Where the run event is recorded. `None` — the default — disables the
    /// trail rather than failing the run: eviction discards recoverable copies,
    /// and refusing to reclaim disk because the audit sink is absent would be
    /// the wrong trade in the direction this service is not protecting.
    packages: Option<Arc<dyn PackageRepository>>,
    /// The upstream-disappearance rows (RFC 0014 §5.3). `None` — what every
    /// existing construction site and test passes — means no hold, so the
    /// suite's behaviour is unchanged by construction rather than by review.
    upstream_status: Option<Arc<dyn crate::ports::UpstreamStatusPort>>,
}

impl EvictionService {
    pub fn new(
        artifact_meta: Arc<dyn ArtifactMetaRepository>,
        storage: Arc<dyn StorageBackend>,
        config: EvictionConfig,
    ) -> Self {
        Self {
            artifact_meta,
            storage,
            config,
            coherence_pending: tokio::sync::Mutex::new(std::collections::HashSet::new()),
            packages: None,
            upstream_status: None,
        }
    }

    /// Wire the eviction hold: a `disappeared` artifact is skipped by the
    /// TTL, idle and keep-latest-N passes, and sorted last by the size cap.
    pub fn with_upstream_status(
        mut self,
        status: Arc<dyn crate::ports::UpstreamStatusPort>,
    ) -> Self {
        self.upstream_status = Some(status);
        self
    }

    /// The `hold_key` forms held in this registry, fetched once per pass.
    /// A store error holds nothing and says so: eviction reclaiming disk
    /// must not stop because the audit table is unreachable, and the next
    /// pass asks again.
    async fn held_keys(&self) -> std::collections::HashSet<String> {
        let Some(status) = &self.upstream_status else {
            return std::collections::HashSet::new();
        };
        match status.disappeared_keys(&self.config.registry).await {
            Ok(keys) => keys,
            Err(e) => {
                tracing::warn!(registry = %self.config.registry, error = %e, "eviction: upstream-status unreadable; holding nothing this pass");
                std::collections::HashSet::new()
            }
        }
    }

    /// Whether the hold covers `meta`: its version, or its whole package.
    fn is_held(held: &std::collections::HashSet<String>, meta: &ArtifactMeta) -> bool {
        !held.is_empty()
            && (held.contains(&crate::entities::hold_key(
                &meta.package_name,
                Some(&meta.version),
            )) || held.contains(&crate::entities::hold_key(&meta.package_name, None)))
    }

    /// Wire the audit trail.
    ///
    /// A builder rather than a `new` parameter so the many call sites that do
    /// not audit — every unit test — stay as they are, and so wiring it is a
    /// visible line in `server/src/setup.rs` rather than a `None` nobody reads.
    pub fn with_audit(mut self, packages: Arc<dyn PackageRepository>) -> Self {
        self.packages = Some(packages);
        self
    }

    /// Run all configured eviction strategies in sequence.
    ///
    /// Under `dry_run` nothing is deleted and the report says what would have
    /// gone. Unlike retention's, this is a per-request choice with no config
    /// safety catch behind it: what a live run drops comes back on the next
    /// request, so the interlock retention needs would be ceremony here.
    pub async fn run_all(
        &self,
        dry_run: bool,
        identity: &Identity,
    ) -> Result<EvictionReport, CoreError> {
        let mut report = if dry_run {
            EvictionReport::dry()
        } else {
            EvictionReport::live()
        };

        // `let n = …` then assign, rather than assigning the call directly:
        // each strategy borrows the report to collect its keys, and the borrow
        // has to end before the count lands in it.
        if self.config.artifact_ttl_secs.is_some() {
            let n = self.run_ttl(&mut report).await?;
            report.evicted_ttl = n;
        }
        if self.config.idle_days.is_some() {
            let n = self.run_idle(&mut report).await?;
            report.evicted_idle = n;
        }
        if self.config.keep_latest_n.is_some() {
            let n = self.run_keep_latest_n(&mut report).await?;
            report.evicted_old_versions = n;
        }
        if self.config.max_size_bytes.is_some() {
            let n = self.run_lru_size_cap(&mut report).await?;
            report.evicted_lru = n;
        }

        report.total = report.evicted_ttl
            + report.evicted_idle
            + report.evicted_old_versions
            + report.evicted_lru;
        self.record_run(&report, identity).await;
        Ok(report)
    }

    /// Record the sweep itself, registry-scoped, one event.
    ///
    /// Recorded on every run, live or dry, for the reason retention's own
    /// `record_run` gives: pointing a policy at a production cache is an
    /// operator's action against that registry, and a trail that only holds the
    /// runs which happened to delete something cannot answer "who has been
    /// running this".
    ///
    /// The counts go to the log line rather than the event: `AccessEvent` has
    /// nowhere to put them, and a summary number in the audit trail that
    /// nothing else can check is worse than no number.
    async fn record_run(&self, report: &EvictionReport, identity: &Identity) {
        tracing::info!(
            registry = %self.config.registry,
            dry_run = report.dry_run,
            total = report.total,
            ttl = report.evicted_ttl,
            idle = report.evicted_idle,
            keep_latest_n = report.evicted_old_versions,
            lru = report.evicted_lru,
            truncated = report.keys_truncated,
            user_id = identity.user_id.as_deref().unwrap_or(""),
            "cache eviction run finished"
        );
        let Some(repo) = self.packages.as_ref() else {
            return;
        };
        let action = if report.dry_run {
            AccessAction::CacheEvictDryRun
        } else {
            AccessAction::CacheEvictRun
        };
        // The empty name and version are what a registry-scoped event looks
        // like here, as they already do for `TombstoneCompact`: the run touched
        // many coordinates and inventing one that was not involved would be
        // worse than leaving them blank. An `EvictionConfig` with no registry
        // sweeps *every* registry, so it has no scope to record either — that
        // is `None`, the shape `AuditPurge` already uses for an action with no
        // coordinate at all, rather than a row of three empty strings.
        let package_id = (!self.config.registry.is_empty())
            .then(|| PackageId::new(&self.config.registry, "", ""));
        let event = AccessEvent {
            id: uuid::Uuid::new_v4(),
            user_id: identity.user_id.clone(),
            user_role: identity.role.clone(),
            package_id,
            action,
            result: AccessResult::Allowed,
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
        };
        if let Err(e) = repo.record_access(event).await {
            tracing::warn!(error = %e, "audit log write failed for cache eviction run");
        }
    }

    /// Drop one cached artifact: the bytes, then its meta row.
    ///
    /// Returns whether it counts as evicted. A dry run returns `true` without
    /// touching either store — the point of the preview is the count and the
    /// key list, and both come from the same walk the live run does.
    ///
    /// The storage delete failing skips the meta delete, as every strategy did
    /// before this was one function: a meta row without its blob is a cache
    /// miss, a blob without its meta row is a leak the coherence sweep has to
    /// find.
    async fn drop_artifact(
        &self,
        meta: &ArtifactMeta,
        report: &mut EvictionReport,
        strategy: &'static str,
    ) -> bool {
        report.record(&meta.artifact_key);
        if report.dry_run {
            return true;
        }
        if let Err(e) = self.storage.delete(&meta.artifact_key).await {
            tracing::warn!(key = %meta.artifact_key, error = %e, strategy, "eviction: storage delete failed");
            return false;
        }
        if let Err(e) = self
            .artifact_meta
            .delete_artifact_meta(&meta.artifact_key)
            .await
        {
            tracing::warn!(key = %meta.artifact_key, error = %e, strategy, "eviction: meta delete failed");
        }
        true
    }

    /// Evict artifacts whose `cached_at` is older than `artifact_ttl_secs`.
    pub async fn run_ttl(&self, report: &mut EvictionReport) -> Result<usize, CoreError> {
        let ttl_secs = match self.config.artifact_ttl_secs {
            Some(s) => s,
            None => return Ok(0),
        };
        let cutoff = Utc::now() - Duration::seconds(ttl_secs as i64);
        let expired = self
            .artifact_meta
            .list_expired_by_ttl(&self.config.registry, cutoff)
            .await?;
        let held = self.held_keys().await;
        let mut count = 0;
        for meta in expired {
            if Self::is_held(&held, &meta) {
                report.held += 1;
                continue;
            }
            if self.drop_artifact(&meta, report, "ttl").await {
                count += 1;
            }
        }
        if count > 0 {
            tracing::info!(count, registry = %self.config.registry, dry_run = report.dry_run, "eviction(ttl): evicted artifacts");
        }
        Ok(count)
    }

    /// Evict artifacts not accessed for `idle_days` days.
    pub async fn run_idle(&self, report: &mut EvictionReport) -> Result<usize, CoreError> {
        let days = match self.config.idle_days {
            Some(d) => d,
            None => return Ok(0),
        };
        let cutoff = Utc::now() - Duration::days(days as i64);
        let idle = self
            .artifact_meta
            .list_idle(&self.config.registry, cutoff)
            .await?;
        let held = self.held_keys().await;
        let mut count = 0;
        for meta in idle {
            if Self::is_held(&held, &meta) {
                report.held += 1;
                continue;
            }
            if self.drop_artifact(&meta, report, "idle").await {
                count += 1;
            }
        }
        if count > 0 {
            tracing::info!(count, registry = %self.config.registry, dry_run = report.dry_run, "eviction(idle): evicted artifacts");
        }
        Ok(count)
    }

    /// For each (registry, package), keep only the N most-recently-cached versions;
    /// evict the rest.
    pub async fn run_keep_latest_n(&self, report: &mut EvictionReport) -> Result<usize, CoreError> {
        let n = match self.config.keep_latest_n {
            Some(n) if n > 0 => n,
            _ => return Ok(0),
        };

        let all = self.artifact_meta.list_artifacts_by_package().await?;
        let held = self.held_keys().await;

        // list_artifacts_by_package returns rows ordered by (registry, package_name, cached_at DESC)
        // Group and pick the tail beyond the first N per group.
        let mut count = 0;
        let mut current_group: Option<(String, String)> = None;
        let mut group_pos: usize = 0;

        for meta in all {
            let group = (meta.registry.clone(), meta.package_name.clone());
            if current_group.as_ref() != Some(&group) {
                current_group = Some(group);
                group_pos = 0;
            }
            group_pos += 1;
            if group_pos <= n {
                continue; // within keep window
            }
            if Self::is_held(&held, &meta) {
                report.held += 1;
                continue;
            }
            if self.drop_artifact(&meta, report, "keep_latest_n").await {
                count += 1;
            }
        }
        if count > 0 {
            tracing::info!(count, registry = %self.config.registry, dry_run = report.dry_run, "eviction(keep_latest_n): evicted old versions");
        }
        Ok(count)
    }

    /// Evict one batch of LRU candidates. Returns `(evicted_count, new_total)`.
    ///
    /// Held candidates sort last (RFC 0014 §4.1): the size cap exists to
    /// stop the disk filling, so the hold does not exempt from it — but a
    /// present artifact goes before an artifact upstream no longer has.
    async fn evict_lru_batch(
        &self,
        candidates: Vec<ArtifactMeta>,
        mut total: u64,
        cap: u64,
        report: &mut EvictionReport,
    ) -> (usize, u64) {
        let held = self.held_keys().await;
        let (kept, present): (Vec<ArtifactMeta>, Vec<ArtifactMeta>) = candidates
            .into_iter()
            .partition(|m| Self::is_held(&held, m));
        let mut count = 0;
        let mut candidates = present;
        let held_count = kept.len();
        candidates.extend(kept);
        let present_count = candidates.len() - held_count;
        let mut held_evicted = 0usize;
        for (i, meta) in candidates.into_iter().enumerate() {
            if total <= cap {
                break;
            }
            if i >= present_count {
                held_evicted += 1;
            }
            let size = meta.size_bytes.unwrap_or(0);
            if !self.drop_artifact(&meta, report, "lru").await {
                continue;
            }
            total = total.saturating_sub(size);
            count += 1;
        }
        if held_evicted == 0 && total <= cap {
            // The present candidates were enough: the held ones were skipped
            // in their favour.
            report.held += held_count;
        }
        (count, total)
    }

    /// Evict the LRU artifacts until total storage for the registry is under `max_size_bytes`.
    pub async fn run_lru_size_cap(&self, report: &mut EvictionReport) -> Result<usize, CoreError> {
        let cap = match self.config.max_size_bytes {
            Some(c) => c,
            None => return Ok(0),
        };
        let mut total = self
            .artifact_meta
            .total_size_bytes(&self.config.registry)
            .await?;
        if total <= cap {
            return Ok(0);
        }

        // **A dry run takes one page and stops.** The loop below re-queries
        // `list_lru` after each batch, which is correct only because the batch
        // it just evicted is gone from the table. A preview deletes nothing, so
        // the same rows come back every time and the walk would count the same
        // artifacts over and over — a preview that over-reports what it would
        // take is worse than one that admits it stopped early.
        if report.dry_run {
            let page = MAX_REPORTED_KEYS as i64;
            let candidates = self
                .artifact_meta
                .list_lru(&self.config.registry, page)
                .await?;
            let full_page = candidates.len() as i64 >= page;
            let (count, remaining) = self.evict_lru_batch(candidates, total, cap, report).await;
            if remaining > cap && full_page {
                report.incomplete_because = Some(format!(
                    "the size-cap preview stopped after {count} artifacts, with the registry still                      {} bytes over the cap. A live run would keep going; re-read this after one.",
                    remaining - cap
                ));
            }
            return Ok(count);
        }

        let mut count = 0;
        // Fetch up to 256 LRU candidates at a time to avoid huge result sets.
        loop {
            if total.saturating_sub(cap) == 0 {
                break;
            }
            let candidates = self
                .artifact_meta
                .list_lru(&self.config.registry, 256)
                .await?;
            if candidates.is_empty() {
                break;
            }
            let (batch, new_total) = self.evict_lru_batch(candidates, total, cap, report).await;
            count += batch;
            total = new_total;
            // A batch that evicted nothing (e.g. every delete failed) makes no
            // progress; stop rather than re-fetching the same candidates forever.
            if batch == 0 {
                break;
            }
        }
        if count > 0 {
            tracing::info!(count, registry = %self.config.registry, "eviction(lru): evicted artifacts");
        }
        Ok(count)
    }

    /// Compare artifact keys in storage against the artifact_meta table. Delete
    /// storage entries that have no corresponding meta row (orphaned blobs from
    /// crashed writes or manual deletions from the DB).
    ///
    /// # A dry run does not advance the state machine
    ///
    /// The two-pass grace below is the whole safety property: a blob is deleted
    /// only if it looked orphaned on the *previous* run too. So a preview must
    /// leave `coherence_pending` exactly as it found it. A dry run that carried
    /// its findings forward would arm the deletions it was asked to merely
    /// describe — run it twice to be careful, and the second run deletes
    /// everything the first one "would have". That is the opposite of what an
    /// operator reaching for `--dry-run` is asking for, so the pending set is
    /// only written on a live run.
    ///
    /// The report separates the two strikes for the same reason: `deleted_keys`
    /// is what went (or would go now), `first_seen_keys` is what a *second* run
    /// would take.
    pub async fn run_coherence_check(
        &self,
        dry_run: bool,
        identity: &Identity,
    ) -> Result<CoherenceReport, CoreError> {
        // Artifact keys are stored as "artifact:{registry}/{name}/{version}".
        // We need the prefix that matches all artifact keys for this registry.
        let key_prefix = if self.config.registry.is_empty() {
            "artifact:".to_owned()
        } else {
            format!("artifact:{}/", self.config.registry)
        };
        let storage_keys = self.storage.list_keys(&key_prefix).await?;
        let meta_rows = self
            .artifact_meta
            .list_artifacts(&self.config.registry)
            .await?;
        let meta_keys: std::collections::HashSet<String> =
            meta_rows.into_iter().map(|m| m.artifact_key).collect();

        // Two-pass grace to close the write/delete race with `fetch_and_cache`:
        // a blob is only deleted if it looked orphaned on the PREVIOUS run too.
        // `fetch_and_cache` writes the blob (`store`) and records its meta row in
        // two steps; that window is milliseconds, always far shorter than the
        // interval between coherence runs, so a legitimately-cached blob always
        // has its meta row by the next run and is dropped from the pending set
        // before it could ever be deleted. Only a genuinely orphaned blob (a
        // crashed write, or a manual DB deletion) stays orphaned across two runs.
        let mut prev_pending = self.coherence_pending.lock().await;
        let mut still_orphaned: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut report = CoherenceReport {
            storage_keys: storage_keys.len(),
            meta_rows: meta_keys.len(),
            dry_run,
            ..Default::default()
        };
        for key in &storage_keys {
            if !self.is_orphaned(key, &meta_keys).await {
                continue;
            }
            let keep_pending = if prev_pending.contains(key) {
                // Orphaned on two consecutive runs → delete.
                self.delete_orphan(key, dry_run, &mut report).await
            } else {
                // First run we've seen this key orphaned — defer deletion, carry
                // it forward so a concurrent in-flight cache write can complete.
                report.record_first_seen(key);
                true
            };
            if keep_pending {
                still_orphaned.insert(key.clone());
            }
        }
        // **Only a live run writes the pending set.** See the note on this
        // function: a preview that carried its findings forward would arm the
        // very deletions it was asked to describe.
        if !dry_run {
            *prev_pending = still_orphaned;
        }
        drop(prev_pending);

        self.record_coherence_run(&report, identity).await;
        Ok(report)
    }

    /// Record the coherence sweep, registry-scoped, one event.
    ///
    /// Its own action rather than [`AccessAction::CacheEvictRun`]: this deletes
    /// blobs *nothing references*, which is a different fact about the system
    /// than a policy trimming a cache, and an auditor has to be able to tell
    /// "the policy took it" from "it was already unreachable".
    /// Does this storage key currently look orphaned?
    ///
    /// `false` means "leave it alone" — either a meta row exists, or the
    /// re-check itself failed, in which case we fail safe toward keeping data
    /// and do not carry the key forward either.
    async fn is_orphaned(&self, key: &str, meta_keys: &std::collections::HashSet<String>) -> bool {
        if meta_keys.contains(key) {
            return false;
        }
        // Fresh point lookup: a meta row recorded after the snapshot taken by
        // the caller means the blob is live — never delete it.
        match self.artifact_meta.get_artifact_checksum(key).await {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(e) => {
                tracing::warn!(key, error = %e, "coherence: meta re-check failed, skipping");
                false
            }
        }
    }

    /// Second strike: `key` was orphaned on the previous run too. Deletes it
    /// (or, on a dry run, records what a live run would delete) and returns
    /// whether it must stay in the pending set for a retry next run.
    async fn delete_orphan(&self, key: &str, dry_run: bool, report: &mut CoherenceReport) -> bool {
        if dry_run {
            report.record_deleted(key);
            return false;
        }
        tracing::warn!(key, "coherence: orphaned storage object (2 runs), deleting");
        if let Err(e) = self.storage.delete(key).await {
            tracing::warn!(key, error = %e, "coherence: failed to delete orphaned object");
            // Deletion failed — keep it pending so we retry next run.
            true
        } else {
            report.record_deleted(key);
            false
        }
    }

    async fn record_coherence_run(&self, report: &CoherenceReport, identity: &Identity) {
        tracing::info!(
            registry = %self.config.registry,
            dry_run = report.dry_run,
            storage_keys = report.storage_keys,
            meta_rows = report.meta_rows,
            deleted = report.orphaned_deleted,
            first_seen = report.first_seen_orphaned,
            user_id = identity.user_id.as_deref().unwrap_or(""),
            "cache coherence sweep finished"
        );
        let Some(repo) = self.packages.as_ref() else {
            return;
        };
        let action = if report.dry_run {
            AccessAction::CacheCoherenceDryRun
        } else {
            AccessAction::CacheCoherenceRun
        };
        let package_id = (!self.config.registry.is_empty())
            .then(|| PackageId::new(&self.config.registry, "", ""));
        let event = AccessEvent {
            id: uuid::Uuid::new_v4(),
            user_id: identity.user_id.clone(),
            user_role: identity.role.clone(),
            package_id,
            action,
            result: AccessResult::Allowed,
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
        };
        if let Err(e) = repo.record_access(event).await {
            tracing::warn!(error = %e, "audit log write failed for cache coherence sweep");
        }
    }
}

#[cfg(test)]
mod tests;
