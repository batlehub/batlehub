/// Extension trait that converts a `sqlx::Error` into `CoreError::Database` with a single
/// `.db_err()` suffix, eliminating the repeated
/// `.map_err(|e| CoreError::Database(e.to_string()))` boilerplate across all adapters.
#[cfg(feature = "db-postgres")]
pub(crate) trait DbResultExt<T> {
    fn db_err(self) -> Result<T, batlehub_core::error::CoreError>;
}

#[cfg(feature = "db-postgres")]
impl<T> DbResultExt<T> for Result<T, sqlx::Error> {
    fn db_err(self) -> Result<T, batlehub_core::error::CoreError> {
        self.map_err(|e| batlehub_core::error::CoreError::Database(e.to_string()))
    }
}

/// Time a DB call and record it under `batlehub_db_query_duration_seconds`.
///
/// This is applied at a small, deliberately partial set of hot-path call
/// sites (the highest-traffic write and a cheap health-check read), not
/// exhaustively across every `sqlx::query` call in the codebase — there is no
/// existing chokepoint that all ~120 call sites funnel through, so full
/// coverage would require a much larger refactor. Extend coverage here
/// incrementally as more paths turn out to matter. `pub` (not `pub(crate)`) so
/// `crates/web`'s `/healthz` handler can share it instead of re-implementing
/// the same start/await/record sequence.
#[cfg(feature = "db-postgres")]
pub async fn timed_query<T>(
    query_name: &'static str,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let start = std::time::Instant::now();
    let result = fut.await;
    metrics::histogram!("batlehub_db_query_duration_seconds", "query" => query_name)
        .record(start.elapsed().as_secs_f64());
    result
}

#[cfg(feature = "db-postgres")]
pub mod artifact_meta;

#[cfg(feature = "db-postgres")]
pub mod readme;

#[cfg(feature = "db-postgres")]
pub mod sbom;

#[cfg(feature = "db-postgres")]
pub mod banner;

#[cfg(feature = "db-postgres")]
pub mod config_change;
pub mod forge;
pub mod security;

#[cfg(feature = "db-postgres")]
pub mod packages;

#[cfg(feature = "db-postgres")]
pub mod stats_history;

#[cfg(feature = "db-postgres")]
pub mod vulnerability;

#[cfg(feature = "db-postgres")]
pub mod advisory;

#[cfg(feature = "db-postgres")]
pub mod air_gap;

// ── Domain subfolders, mirroring `batlehub_core::ports`'s auth/governance/ops/storage split ──
// (registry-domain concerns stay flat above, as `packages`/`artifact_meta` already did before
// this split — there was no separate `registry/` port module to mirror there.)

#[cfg(feature = "db-postgres")]
pub mod auth;

#[cfg(feature = "db-postgres")]
pub mod governance;

#[cfg(feature = "db-postgres")]
pub mod ops;

#[cfg(feature = "db-postgres")]
pub mod storage;

#[cfg(feature = "db-postgres")]
pub use artifact_meta::PgArtifactMetaRepository;

#[cfg(feature = "db-postgres")]
pub use banner::PgBannerStore;

#[cfg(feature = "db-postgres")]
pub use config_change::PgConfigChangeRepository;
pub use forge::{PgRateLimitBudget, PgRefResolutionRepository};
pub use security::{PgScanQueue, PgVerdictRepository, PgWorkerRegistry};

#[cfg(feature = "db-postgres")]
pub use governance::beta_channel::PgBetaChannelStore;

#[cfg(feature = "db-postgres")]
pub use governance::grants::PgGrantRepository;
pub use governance::ownership::PgOwnershipStore;
pub use governance::policy::PgPolicyRepository;
pub use governance::signing_keys::PgSigningKeyStore;

#[cfg(feature = "db-postgres")]
pub use governance::team_namespace::PgTeamNamespaceStore;

#[cfg(feature = "db-postgres")]
pub use governance::user_block::{InMemoryUserBlockRepository, PgUserBlockRepository};

#[cfg(feature = "db-postgres")]
pub use ops::quota::PgQuotaRepository;

#[cfg(feature = "db-postgres")]
pub use packages::PgPackageRepository;

pub use ops::upstream_status::PgUpstreamStatusStore;
#[cfg(feature = "db-postgres")]
pub use stats_history::PgStatsHistoryRepository;

#[cfg(feature = "db-postgres")]
pub use readme::{
    column_text_config, ensure_readme_text_config, text_config_names, PgReadmeRepository,
    DEFAULT_TEXT_CONFIG,
};

#[cfg(feature = "db-postgres")]
pub use sbom::PgSbomRepository;

#[cfg(feature = "db-postgres")]
pub use storage::storage_admin::PgStorageAdminRepository;

#[cfg(feature = "db-postgres")]
pub use vulnerability::PgVulnerabilityRepository;

#[cfg(feature = "db-postgres")]
pub use advisory::PgAdvisoryRepository;

#[cfg(feature = "db-postgres")]
pub use air_gap::{PgBundleHistory, PgMissRecorder};
