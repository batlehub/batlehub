mod run;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use crate::ports::{ArtifactCacheMeta, RegistryClient, StorageBackend, WarmCoordinator};
use crate::services::metrics::ProxyMetrics;

/// How long a warm-up claim is held in the coordinator. Long enough to cover the
/// full fetch+store cycle for large artifacts; short enough to unblock other replicas
/// when the winning replica crashes mid-download.
const WARM_CLAIM_TTL: Duration = Duration::from_secs(600);

/// One version or path that did not warm, and why.
///
/// RFC 0004-bis A3. Every one of these was already `tracing::warn!`-ed with the
/// registry, package, version and error attached — so the information existed,
/// went to the server log, and stopped there. An operator warming eleven
/// packages read `errors: 3` and had no way to learn *which* three without
/// shell access to the instance they are administering through a console.
#[derive(Debug, Clone)]
pub struct WarmFailure {
    /// Package name, or the upstream path for a path-addressed registry.
    pub package: String,
    /// The version that failed. `None` when the failure was listing the
    /// versions, so no single version is at fault.
    pub version: Option<String>,
    /// What went wrong, as the log line records it.
    pub error: String,
}

/// Result of a warming run (a single package or a batch).
#[derive(Debug, Default, Clone)]
pub struct WarmingReport {
    /// Artifact versions fetched and stored during this run.
    pub warmed: usize,
    /// Artifact versions already present in storage (skipped).
    pub skipped: usize,
    /// Versions that failed to fetch or store.
    pub errors: usize,
    /// One entry per counted error. `errors` stays the authority on the count —
    /// a panicked task increments it with nothing to name.
    pub failures: Vec<WarmFailure>,
}

impl WarmingReport {
    /// A report for one failure, counted and named.
    pub(crate) fn failed(
        package: impl Into<String>,
        version: Option<String>,
        error: impl std::fmt::Display,
    ) -> Self {
        Self {
            errors: 1,
            failures: vec![WarmFailure {
                package: package.into(),
                version,
                error: error.to_string(),
            }],
            ..Default::default()
        }
    }
}

impl std::ops::AddAssign for WarmingReport {
    fn add_assign(&mut self, mut other: Self) {
        self.warmed += other.warmed;
        self.skipped += other.skipped;
        self.errors += other.errors;
        self.failures.append(&mut other.failures);
    }
}

/// Pre-fetches artifact versions from an upstream registry and stores them in
/// the local cache so they are available with zero latency on first request.
///
/// `Clone` is derived so `warm_one_version`'s spawn sites can pass a single
/// `self.clone()` (four cheap `Arc` bumps + a `String`/two `usize`s) instead of
/// naming each field individually at every call site.
#[derive(Clone)]
pub struct WarmingService {
    pub client: Arc<dyn RegistryClient>,
    pub storage: Arc<dyn StorageBackend>,
    pub artifact_meta: Arc<dyn ArtifactCacheMeta>,
    pub registry_name: String,
    /// How many of the most-recent versions to warm per package.
    /// Ignored when the package string includes a pinned version (e.g. `"lodash@4.17.21"`).
    pub latest_n: usize,
    /// Maximum concurrent artifact downloads.
    pub concurrency: usize,
    /// Cross-replica coordination: prevents multiple replicas from downloading
    /// the same artifact simultaneously. Defaults to `NoopWarmCoordinator`.
    pub coordinator: Arc<dyn WarmCoordinator>,
    /// Shared with `ProxyService` so warming traffic feeds the same
    /// upstream-health signal as regular proxy reads.
    pub metrics: Arc<ProxyMetrics>,
    /// The platforms to warm, for the kinds whose artifact is one file per
    /// platform (`sdkman`, `nodedist` — RFC 0010 §6.9). Empty means the
    /// platform this server runs on; ignored by every other kind.
    pub platforms: Vec<String>,
}
