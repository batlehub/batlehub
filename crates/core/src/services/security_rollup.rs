//! The daily security rollup (RFC 0050 §5.2): one leader per estate writes
//! `security_daily` at startup and then hourly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Duration, NaiveDate, Utc};

use crate::error::CoreError;
use crate::ports::{ScanQueue, SecurityDailyRepository};
use crate::services::hot_config::HotConfigLock;

/// The advisory lock the rollup runs under: its own, so it never contends
/// with the rescan leader (`RESCAN_LEADER_KEY`).
pub const SECURITY_ROLLUP_LEADER_KEY: i64 = 0x0050_5ec0_da11;

/// The most missing `(registry, day)` rows one run backfills (decision 31).
pub const GAP_FILL_PER_RUN: usize = 14;

/// How far back the gap fill looks when retention is `0` (keep forever).
const FOREVER_FILL_DAYS: u32 = 365;

pub struct SecurityRollupService {
    pub repo: Arc<dyn SecurityDailyRepository>,
    /// Only for `try_lead`: the leader lock every scheduled job here uses.
    pub leader: Arc<dyn ScanQueue>,
    /// Which registries get a row: every configured one.
    pub hot: HotConfigLock,
    /// `[stats] security_history_retention_days`; `0` keeps every day.
    pub retention_days: u32,
}

/// What one run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollupOutcome {
    NotLeader,
    Ran {
        rows: usize,
        filled: usize,
        pruned: u64,
    },
}

impl SecurityRollupService {
    /// One run at `now`; idempotent on `(registry, day)`. Records the §10.1
    /// metrics and its own log line.
    pub async fn run_once(&self, now: DateTime<Utc>) -> Result<RollupOutcome, CoreError> {
        let started = Instant::now();
        let result = self.run(now).await;
        let outcome = match &result {
            Ok(RollupOutcome::NotLeader) => "skipped_not_leader",
            Ok(RollupOutcome::Ran { .. }) => "ok",
            Err(_) => "error",
        };
        metrics::counter!("batlehub_security_rollup_runs_total", "outcome" => outcome).increment(1);
        metrics::histogram!("batlehub_security_rollup_duration_seconds")
            .record(started.elapsed().as_secs_f64());
        match &result {
            Ok(RollupOutcome::Ran {
                rows,
                filled,
                pruned,
            }) => {
                tracing::info!(rows, filled, pruned, "security rollup: done");
            }
            Ok(RollupOutcome::NotLeader) => {}
            Err(e) => tracing::warn!(error = %e, "security rollup: failed"),
        }
        result
    }

    async fn run(&self, now: DateTime<Utc>) -> Result<RollupOutcome, CoreError> {
        if !self.leader.try_lead(SECURITY_ROLLUP_LEADER_KEY).await? {
            return Ok(RollupOutcome::NotLeader);
        }
        let mut registries: Vec<String> =
            self.hot.read().await.registries.keys().cloned().collect();
        registries.sort();
        let today = now.date_naive();
        let yesterday = today - Duration::days(1);
        // Each step stands alone: one that fails is logged and counted, and
        // the others (the backfill, the prune) still run.
        let mut failed = 0;
        let mut step = |what: &'static str, r: Result<(), CoreError>| {
            if let Err(e) = r {
                failed += 1;
                tracing::warn!(step = what, error = %e, "security rollup: a step failed");
            }
        };

        // 1. Event columns of yesterday and today; state columns of today only.
        let mut rows = 0;
        for day in [yesterday, today] {
            let r = self.write_events(day, &registries).await.map(|n| rows += n);
            step("events", r);
        }
        let r = self.write_state(today, &registries).await;
        step("state", r);

        // 2. The gaps: at most 14 missing rows of the window, oldest first.
        let filled = match self.fill_gaps(today, yesterday, &registries).await {
            Ok(n) => n,
            Err(e) => {
                step("gap fill", Err(e));
                0
            }
        };

        // 3. Pruning.
        let pruned = match self.retention_days {
            0 => 0,
            n => match self
                .repo
                .prune_before(today - Duration::days(i64::from(n.saturating_sub(1))))
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    step("prune", Err(e));
                    0
                }
            },
        };
        if failed > 0 {
            return Err(CoreError::Other(anyhow::anyhow!(
                "{failed} rollup step(s) failed; the others ran"
            )));
        }
        Ok(RollupOutcome::Ran {
            rows,
            filled,
            pruned,
        })
    }

    async fn write_events(
        &self,
        day: NaiveDate,
        registries: &[String],
    ) -> Result<usize, CoreError> {
        let events = self.repo.compute_events(day, registries).await?;
        for (registry, e) in registries.iter().zip(&events) {
            self.repo.upsert_events(registry, day, e).await?;
        }
        Ok(events.len())
    }

    async fn write_state(&self, today: NaiveDate, registries: &[String]) -> Result<(), CoreError> {
        let state: BTreeMap<String, _> = self.repo.compute_state().await?.into_iter().collect();
        for registry in registries {
            let s = state.get(registry).cloned().unwrap_or_default();
            self.repo.upsert_state(registry, today, &s).await?;
        }
        Ok(())
    }

    /// Backfills at most [`GAP_FILL_PER_RUN`] missing rows; how many it took.
    async fn fill_gaps(
        &self,
        today: NaiveDate,
        yesterday: NaiveDate,
        registries: &[String],
    ) -> Result<usize, CoreError> {
        let window = match self.retention_days {
            0 => FOREVER_FILL_DAYS,
            n => n,
        };
        let from = today - Duration::days(i64::from(window.saturating_sub(1)));
        let present: BTreeSet<(NaiveDate, String)> = self
            .repo
            .range(from, today)
            .await?
            .into_iter()
            .map(|r| (r.day, r.registry))
            .collect();
        let mut missing: BTreeMap<NaiveDate, Vec<String>> = BTreeMap::new();
        let mut taken = 0;
        let mut day = from;
        'days: while day < yesterday {
            for registry in registries {
                if taken == GAP_FILL_PER_RUN {
                    break 'days;
                }
                if !present.contains(&(day, registry.clone())) {
                    missing.entry(day).or_default().push(registry.clone());
                    taken += 1;
                }
            }
            day += Duration::days(1);
        }
        for (day, regs) in &missing {
            let events = self.repo.compute_events(*day, regs).await?;
            for (registry, e) in regs.iter().zip(&events) {
                // An all-NULL row still lands: the day is known to be beyond
                // its sources, and the next run does not ask again.
                self.repo.upsert_events(registry, *day, e).await?;
            }
        }
        Ok(taken)
    }
}
