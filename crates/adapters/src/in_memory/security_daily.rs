use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use tokio::sync::RwLock;

use batlehub_core::error::CoreError;
use batlehub_core::ports::{DailyEvents, DailyState, SecurityDailyRepository, SecurityDailyRow};

/// In-memory `security_daily` (RFC 0050 §6.2). There is no in-memory audit
/// log to count from, so the source side is set by the test: `state` is what
/// `compute_state` answers, `events` what `compute_events` answers per
/// registry (absent = zeros), and `held_from` the first day the sources still
/// hold — earlier days compute as `None`, as a purge leaves them.
#[derive(Debug, Default)]
pub struct InMemorySecurityDaily {
    rows: RwLock<BTreeMap<(String, NaiveDate), SecurityDailyRow>>,
    pub state: RwLock<Vec<(String, DailyState)>>,
    pub events: RwLock<BTreeMap<String, DailyEvents>>,
    pub held_from: RwLock<Option<NaiveDate>>,
    /// How many times `range` ran: the source-cache tests count reads.
    pub range_calls: AtomicU64,
}

impl InMemorySecurityDaily {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn row(registry: &str, day: NaiveDate) -> SecurityDailyRow {
        SecurityDailyRow {
            registry: registry.to_owned(),
            day,
            state: None,
            events: DailyEvents::default(),
        }
    }
}

#[async_trait]
impl SecurityDailyRepository for InMemorySecurityDaily {
    async fn compute_state(&self) -> Result<Vec<(String, DailyState)>, CoreError> {
        Ok(self.state.read().await.clone())
    }

    async fn compute_events(
        &self,
        day: NaiveDate,
        registries: &[String],
    ) -> Result<Vec<DailyEvents>, CoreError> {
        if self.held_from.read().await.is_some_and(|from| day < from) {
            return Ok(vec![DailyEvents::default(); registries.len()]);
        }
        let events = self.events.read().await;
        Ok(registries
            .iter()
            .map(|r| {
                events.get(r).cloned().unwrap_or(DailyEvents {
                    refusals: Some(0),
                    refusals_by_code: Some(BTreeMap::new()),
                    flags_opened: Some(0),
                    flags_revoked: Some(0),
                    jobs_done: Some(0),
                    jobs_failed: Some(0),
                })
            })
            .collect())
    }

    async fn upsert_events(
        &self,
        registry: &str,
        day: NaiveDate,
        e: &DailyEvents,
    ) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        let row = rows
            .entry((registry.to_owned(), day))
            .or_insert_with(|| Self::row(registry, day));
        // As Postgres: a NULL never erases a stored number, and refusals and
        // jobs keep the larger count (a purged day's recompute is its tail).
        let ev = &mut row.events;
        let larger = |new: Option<u64>, old: Option<u64>| new.max(old);
        if e.refusals.is_some() && (ev.refusals.is_none() || e.refusals >= ev.refusals) {
            ev.refusals_by_code = e.refusals_by_code.clone().or(ev.refusals_by_code.take());
        }
        ev.refusals = larger(e.refusals, ev.refusals);
        ev.flags_opened = e.flags_opened.or(ev.flags_opened);
        ev.flags_revoked = e.flags_revoked.or(ev.flags_revoked);
        ev.jobs_done = larger(e.jobs_done, ev.jobs_done);
        ev.jobs_failed = larger(e.jobs_failed, ev.jobs_failed);
        Ok(())
    }

    async fn upsert_state(
        &self,
        registry: &str,
        day: NaiveDate,
        s: &DailyState,
    ) -> Result<(), CoreError> {
        self.rows
            .write()
            .await
            .entry((registry.to_owned(), day))
            .or_insert_with(|| Self::row(registry, day))
            .state = Some(s.clone());
        Ok(())
    }

    async fn range(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<SecurityDailyRow>, CoreError> {
        self.range_calls.fetch_add(1, Ordering::Relaxed);
        let mut out: Vec<SecurityDailyRow> = self
            .rows
            .read()
            .await
            .values()
            .filter(|r| r.day >= from && r.day <= to)
            .cloned()
            .collect();
        out.sort_by(|a, b| (a.day, &a.registry).cmp(&(b.day, &b.registry)));
        Ok(out)
    }

    async fn prune_before(&self, day: NaiveDate) -> Result<u64, CoreError> {
        let mut rows = self.rows.write().await;
        let before = rows.len();
        rows.retain(|(_, d), _| *d >= day);
        Ok((before - rows.len()) as u64)
    }
}
