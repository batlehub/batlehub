//! Where the misses of an air-gapped instance are remembered (RFC 0008 §6.2).

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::entities::{BundleImport, ContentMiss, MissFilter, RecordedMiss};
use crate::error::CoreError;

#[async_trait]
pub trait MissRecorder: Send + Sync {
    /// Record one miss, or bump the counter and `last_seen` of the row that
    /// already names it.
    ///
    /// **Fire-and-forget at the call site**: a failure to record must never
    /// turn a `503` into a `500`. The caller logs and carries on.
    async fn record(&self, miss: &ContentMiss, now: DateTime<Utc>) -> Result<(), CoreError>;

    async fn list(&self, filter: &MissFilter) -> Result<Vec<RecordedMiss>, CoreError>;

    async fn count(&self, filter: &MissFilter) -> Result<u64, CoreError>;

    /// Forget rows last seen before `before`, in `registry` or everywhere.
    /// Returns how many went.
    async fn purge(&self, before: DateTime<Utc>, registry: Option<&str>) -> Result<u64, CoreError>;
}

/// The history of what came across the gap (RFC 0008 §6.4).
///
/// A disconnected instance has no upstream to point at, so this *is* the
/// provenance of everything it holds.
#[async_trait]
pub trait BundleHistory: Send + Sync {
    async fn record(&self, import: &BundleImport) -> Result<(), CoreError>;

    /// Newest first.
    async fn list(&self, limit: u64) -> Result<Vec<BundleImport>, CoreError>;

    /// Whether this bundle id has already been imported — an import is
    /// idempotent by id, so carrying the same bundle twice is not two
    /// histories.
    async fn seen(&self, bundle_id: &str) -> Result<bool, CoreError>;
}
