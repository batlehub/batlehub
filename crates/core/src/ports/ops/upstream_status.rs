//! The `upstream_status` store (RFC 0014 §6.1). Under `ops/` rather than
//! `registry/`: this is an operational record about the estate, not part of
//! any registry protocol.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::entities::{MissObservation, UpstreamKey, UpstreamStatus, UpstreamStatusFilter};
use crate::error::CoreError;

#[async_trait]
pub trait UpstreamStatusPort: Send + Sync {
    /// Record one miss: insert at `consecutive_misses = 1`, or increment.
    /// Returns the resulting row so the caller sees the count without a
    /// second read — the sweep needs it immediately to decide whether the
    /// confirmation thresholds are met.
    async fn record_miss(&self, obs: MissObservation<'_>) -> Result<UpstreamStatus, CoreError>;

    /// Move the row to `disappeared` at `at`. A no-op on a row that is
    /// already confirmed, and an error on a row that does not exist: a
    /// confirmation without a miss is a bug in the caller, not a state.
    async fn confirm(&self, key: &UpstreamKey<'_>, at: DateTime<Utc>) -> Result<(), CoreError>;

    /// Delete the row. Returns what it was, so the caller can tell a
    /// reappearance (`Some`) from a package that was never missing (`None`).
    async fn clear(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError>;

    async fn get(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError>;

    async fn list(&self, filter: UpstreamStatusFilter) -> Result<Vec<UpstreamStatus>, CoreError>;

    async fn count(&self, filter: UpstreamStatusFilter) -> Result<u64, CoreError>;

    /// Keys currently `disappeared` in `registry`, as
    /// [`crate::entities::hold_key`] forms, for the eviction hold. A set so
    /// the eviction pass makes one query per registry, not one per candidate.
    async fn disappeared_keys(&self, registry: &str) -> Result<HashSet<String>, CoreError>;
}
