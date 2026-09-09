//! Storage for pushed vulnerability flags and the exposure they imply
//! (RFC 0002 §6.2, recast by §13).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::entities::{
    ExposurePage, ExposureQuery, FlagFilter, FlagSourceCoverage, PackageFlag, RegistryScanState,
};
use crate::error::CoreError;

#[async_trait]
pub trait AdvisoryRepository: Send + Sync {
    /// Store or update one flag, keyed by `(source, external_id)`. Returns
    /// the stored id and whether a row was created. A re-push of a revoked
    /// flag revives it: the tombstone is cleared and `first_seen` kept.
    async fn upsert_flag(&self, flag: PackageFlag) -> Result<(Uuid, bool), CoreError>;

    /// Tombstone a flag. Returns `false` when no live flag matched.
    async fn revoke_flag(
        &self,
        source: &str,
        external_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, CoreError>;

    async fn get_flag(
        &self,
        source: &str,
        external_id: &str,
    ) -> Result<Option<PackageFlag>, CoreError>;

    async fn list_flags(&self, filter: &FlagFilter) -> Result<Vec<PackageFlag>, CoreError>;

    async fn count_flags(&self, filter: &FlagFilter) -> Result<u64, CoreError>;

    /// Every live flag on any version of `(registry, name)`; the caller
    /// matches the version with [`PackageFlag::covers`].
    async fn live_flags_for_package(
        &self,
        registry: &str,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<PackageFlag>, CoreError>;

    /// The exposure report page (RFC 0002 §4.6): allowed downloads in the
    /// window joined to the flags covering them, one row per consumer ×
    /// coordinate × flag, `last_pull DESC`.
    async fn list_exposure(&self, query: &ExposureQuery) -> Result<ExposurePage, CoreError>;

    /// Live-flag count and last push per source.
    async fn source_coverage(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<FlagSourceCoverage>, CoreError>;

    /// The SBOM re-scan finished a pass over `state.registry`.
    async fn record_scan_state(&self, state: &RegistryScanState) -> Result<(), CoreError>;

    async fn list_scan_state(&self) -> Result<Vec<RegistryScanState>, CoreError>;
}
