use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::{DateTime, Utc};

// ponytail: fixed thresholds/smoothing instead of per-registry config; make
// configurable (a `[registries.health]` block, like other policy knobs) if an
// operator actually needs to tune these per upstream.
const DEGRADED_ERROR_RATE_PERMILLE: u64 = 250; // 25% of recent upstream calls erroring
const DEGRADED_LATENCY_MS: u64 = 5_000; // 5s EMA upstream latency
const EMA_SMOOTHING: u64 = 8; // new sample gets 1/8 weight

pub struct RegistryCounters {
    artifact_hits: AtomicU64,
    artifact_misses: AtomicU64,
    /// Allowed reads of a *version listing* — a packument, a flat index, a
    /// `maven-metadata.xml`. Counted rather than filed as audit rows: a
    /// `cargo build` over a 400-crate graph is 400 listing fetches, and one
    /// `AccessEvent` per listing puts rows in the audit trail that transferred
    /// no bytes. Denials keep their own rows; see `ProxyService::version_document`.
    listing_reads: AtomicU64,
    /// EMA of upstream error occurrence, fixed-point 0-1000 (0 = never errors, 1000 = always errors).
    upstream_error_rate_permille: AtomicU64,
    /// EMA of upstream call latency in milliseconds.
    upstream_latency_ms: AtomicU64,
    degraded: AtomicBool,
}

fn ema_update(current: &AtomicU64, sample: u64) {
    let _ = current.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
        let delta = (sample as i64 - old as i64) / EMA_SMOOTHING as i64;
        Some((old as i64 + delta) as u64)
    });
}

impl RegistryCounters {
    fn new() -> Self {
        Self {
            artifact_hits: AtomicU64::new(0),
            artifact_misses: AtomicU64::new(0),
            listing_reads: AtomicU64::new(0),
            upstream_error_rate_permille: AtomicU64::new(0),
            upstream_latency_ms: AtomicU64::new(0),
            degraded: AtomicBool::new(false),
        }
    }

    pub fn hits(&self) -> u64 {
        self.artifact_hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.artifact_misses.load(Ordering::Relaxed)
    }

    pub fn listing_reads(&self) -> u64 {
        self.listing_reads.load(Ordering::Relaxed)
    }

    fn record_outcome(&self, ok: bool) {
        let sample = if ok { 0 } else { 1000 };
        ema_update(&self.upstream_error_rate_permille, sample);
    }

    fn record_latency(&self, ms: u64) {
        ema_update(&self.upstream_latency_ms, ms);
    }

    pub fn upstream_error_rate_permille(&self) -> u64 {
        self.upstream_error_rate_permille.load(Ordering::Relaxed)
    }

    pub fn upstream_latency_ms(&self) -> u64 {
        self.upstream_latency_ms.load(Ordering::Relaxed)
    }

    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    /// Recomputes the degraded flag from the current EMAs. Returns `(now, was)` so
    /// the caller can log/emit a gauge only on a state transition.
    fn refresh_degraded(&self) -> (bool, bool) {
        let now = self.upstream_error_rate_permille() >= DEGRADED_ERROR_RATE_PERMILLE
            || self.upstream_latency_ms() >= DEGRADED_LATENCY_MS;
        let was = self.degraded.swap(now, Ordering::Relaxed);
        (now, was)
    }
}

/// In-memory per-registry counters for the stats dashboard.
/// Reset on process restart; Prometheus metrics are the durable store.
pub struct ProxyMetrics {
    pub started_at: DateTime<Utc>,
    registries: HashMap<String, RegistryCounters>,
}

impl ProxyMetrics {
    pub fn new(registry_names: &[String]) -> Self {
        let registries = registry_names
            .iter()
            .map(|name| (name.clone(), RegistryCounters::new()))
            .collect();
        Self {
            started_at: Utc::now(),
            registries,
        }
    }

    pub fn record_artifact_hit(&self, registry: &str) {
        if let Some(c) = self.registries.get(registry) {
            c.artifact_hits.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_artifact_miss(&self, registry: &str) {
        if let Some(c) = self.registries.get(registry) {
            c.artifact_misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Count one allowed version-listing read.
    ///
    /// The counter is the durable record: `StatsRollupService` writes the
    /// *difference* since its last tick to `stats_history` once an hour, so this
    /// costs one atomic increment on the hottest path in the system and no
    /// per-request write at all.
    pub fn record_listing_read(&self, registry: &str) {
        if let Some(c) = self.registries.get(registry) {
            c.listing_reads.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Feeds the rolling upstream-error-rate EMA and warns (once per
    /// healthy→degraded transition) that cached data for this registry may be
    /// going stale while upstream keeps failing.
    pub fn record_upstream_outcome(&self, registry: &str, ok: bool) {
        if let Some(c) = self.registries.get(registry) {
            c.record_outcome(ok);
            self.refresh_degraded(registry, c);
        }
    }

    /// Feeds the rolling upstream-latency EMA (milliseconds).
    pub fn record_upstream_latency(&self, registry: &str, ms: u64) {
        if let Some(c) = self.registries.get(registry) {
            c.record_latency(ms);
            self.refresh_degraded(registry, c);
        }
    }

    fn refresh_degraded(&self, registry: &str, c: &RegistryCounters) {
        let (now, was) = c.refresh_degraded();
        metrics::gauge!("batlehub_upstream_health_degraded", "registry" => registry.to_string())
            .set(if now { 1.0 } else { 0.0 });
        if now && !was {
            tracing::warn!(
                registry,
                error_rate_permille = c.upstream_error_rate_permille(),
                latency_ms = c.upstream_latency_ms(),
                "upstream degraded (high error rate or slow responses); cached data may be stale"
            );
        } else if was && !now {
            tracing::info!(registry, "upstream recovered");
        }
    }

    pub fn all(&self) -> &HashMap<String, RegistryCounters> {
        &self.registries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &[&str]) -> Vec<String> {
        s.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn new_registries_start_at_zero() {
        let m = ProxyMetrics::new(&names(&["npm", "cargo"]));
        let npm = m.all().get("npm").unwrap();
        assert_eq!(npm.hits(), 0);
        assert_eq!(npm.misses(), 0);
        let cargo = m.all().get("cargo").unwrap();
        assert_eq!(cargo.hits(), 0);
        assert_eq!(cargo.misses(), 0);
    }

    #[test]
    fn record_hit_increments_only_hits() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        m.record_artifact_hit("npm");
        m.record_artifact_hit("npm");
        let c = m.all().get("npm").unwrap();
        assert_eq!(c.hits(), 2);
        assert_eq!(c.misses(), 0);
    }

    #[test]
    fn record_miss_increments_only_misses() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        m.record_artifact_miss("npm");
        let c = m.all().get("npm").unwrap();
        assert_eq!(c.hits(), 0);
        assert_eq!(c.misses(), 1);
    }

    #[test]
    fn listing_reads_are_counted_separately_from_artifact_traffic() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        m.record_listing_read("npm");
        m.record_listing_read("npm");
        m.record_artifact_hit("npm");

        let c = m.all().get("npm").unwrap();
        assert_eq!(c.listing_reads(), 2);
        assert_eq!(c.hits(), 1, "a listing is not a cache hit");
        assert_eq!(c.misses(), 0);
    }

    #[test]
    fn registries_tracked_independently() {
        let m = ProxyMetrics::new(&names(&["npm", "cargo"]));
        m.record_artifact_hit("npm");
        m.record_artifact_miss("cargo");
        m.record_artifact_miss("cargo");
        assert_eq!(m.all().get("npm").unwrap().hits(), 1);
        assert_eq!(m.all().get("npm").unwrap().misses(), 0);
        assert_eq!(m.all().get("cargo").unwrap().hits(), 0);
        assert_eq!(m.all().get("cargo").unwrap().misses(), 2);
    }

    #[test]
    fn unknown_registry_is_silently_ignored() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        m.record_artifact_hit("unknown"); // should not panic
        m.record_artifact_miss("unknown");
        assert!(m.all().get("unknown").is_none());
    }

    #[test]
    fn repeated_errors_mark_registry_degraded() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        for _ in 0..20 {
            m.record_upstream_outcome("npm", false);
        }
        let c = m.all().get("npm").unwrap();
        assert!(c.is_degraded());
        assert!(c.upstream_error_rate_permille() >= DEGRADED_ERROR_RATE_PERMILLE);
    }

    #[test]
    fn steady_successes_keep_registry_healthy() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        for _ in 0..20 {
            m.record_upstream_outcome("npm", true);
        }
        let c = m.all().get("npm").unwrap();
        assert!(!c.is_degraded());
        assert_eq!(c.upstream_error_rate_permille(), 0);
    }

    #[test]
    fn slow_latency_marks_registry_degraded_even_without_errors() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        for _ in 0..20 {
            m.record_upstream_outcome("npm", true);
            m.record_upstream_latency("npm", 10_000);
        }
        let c = m.all().get("npm").unwrap();
        assert!(c.is_degraded());
    }

    #[test]
    fn recovering_after_degraded_clears_the_flag() {
        let m = ProxyMetrics::new(&names(&["npm"]));
        for _ in 0..20 {
            m.record_upstream_outcome("npm", false);
        }
        assert!(m.all().get("npm").unwrap().is_degraded());
        for _ in 0..40 {
            m.record_upstream_outcome("npm", true);
        }
        assert!(!m.all().get("npm").unwrap().is_degraded());
    }
}
