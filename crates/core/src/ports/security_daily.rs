//! `security_daily`: the daily security rollup behind `/security/trends`
//! (RFC 0050 §5.2).

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::NaiveDate;

use crate::error::CoreError;

/// The **state** columns: what the verdict store held when the row was
/// written. Only ever written for the current day, so a past day never
/// drifts (§5.2's invariant).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DailyState {
    pub allowed: u64,
    pub warned: u64,
    pub quarantined: u64,
    pub denied: u64,
    pub findings_critical: u64,
    pub findings_high: u64,
    pub findings_medium: u64,
    pub findings_low: u64,
    pub findings_unknown: u64,
}

/// The **event** columns: what happened during the day. `None` is a day the
/// source no longer holds (purged, pruned), never a zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DailyEvents {
    pub refusals: Option<u64>,
    pub refusals_by_code: Option<BTreeMap<String, u64>>,
    pub flags_opened: Option<u64>,
    pub flags_revoked: Option<u64>,
    pub jobs_done: Option<u64>,
    pub jobs_failed: Option<u64>,
}

/// One `(registry, day)` row; `state` is `None` on a day no state was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityDailyRow {
    pub registry: String,
    pub day: NaiveDate,
    pub state: Option<DailyState>,
    pub events: DailyEvents,
}

#[async_trait]
pub trait SecurityDailyRepository: Send + Sync {
    /// The state columns per registry, counted now.
    async fn compute_state(&self) -> Result<Vec<(String, DailyState)>, CoreError>;

    /// The event columns of `day` (UTC), one entry per name in `registries`,
    /// in order; each column `None` when `day` is beyond its source's
    /// retention.
    async fn compute_events(
        &self,
        day: NaiveDate,
        registries: &[String],
    ) -> Result<Vec<DailyEvents>, CoreError>;

    /// Write a day's event columns, creating the row if needed.
    async fn upsert_events(
        &self,
        registry: &str,
        day: NaiveDate,
        events: &DailyEvents,
    ) -> Result<(), CoreError>;

    /// Write a day's state columns, creating the row if needed.
    async fn upsert_state(
        &self,
        registry: &str,
        day: NaiveDate,
        state: &DailyState,
    ) -> Result<(), CoreError>;

    /// Rows with `from <= day <= to`, oldest first.
    async fn range(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<SecurityDailyRow>, CoreError>;

    /// Delete rows strictly before `day`; the number deleted.
    async fn prune_before(&self, day: NaiveDate) -> Result<u64, CoreError>;
}
