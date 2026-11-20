//! `security_daily` (migration 071): the daily security rollup behind
//! `/security/trends` (RFC 0050 §5.2), and the source queries it is computed
//! from.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};

use batlehub_core::entities::EXHAUSTED_PREFIX;
use batlehub_core::error::CoreError;
use batlehub_core::ports::{DailyEvents, DailyState, SecurityDailyRepository, SecurityDailyRow};

use super::DbResultExt;

/// How long completed `scan_jobs` rows are kept (`housekeeping.rs`'s sweep):
/// beyond it, the job columns are `NULL`, never zero.
const SCAN_JOBS_KEPT_DAYS: i64 = 7;

pub struct PgSecurityDailyRepository {
    pool: PgPool,
}

impl PgSecurityDailyRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// A transaction whose statements each give up after 30 s (§5.2).
    async fn bounded(&self) -> Result<Transaction<'static, Postgres>, CoreError> {
        let mut tx = self.pool.begin().await.db_err()?;
        sqlx::query("SET LOCAL statement_timeout = '30s'")
            .execute(&mut *tx)
            .await
            .db_err()?;
        Ok(tx)
    }
}

fn day_bounds(day: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let start = day.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
    (start, start + Duration::days(1))
}

fn as_i64(n: Option<u64>) -> Option<i64> {
    n.map(|v| i64::try_from(v).unwrap_or(i64::MAX))
}

fn as_u64(n: Option<i64>) -> Option<u64> {
    n.map(|v| u64::try_from(v).unwrap_or(0))
}

#[async_trait]
impl SecurityDailyRepository for PgSecurityDailyRepository {
    async fn compute_state(&self) -> Result<Vec<(String, DailyState)>, CoreError> {
        let mut tx = self.bounded().await?;
        let states = sqlx::query(
            "SELECT registry, state, COUNT(*) AS n FROM artifact_verdicts GROUP BY 1, 2",
        )
        .fetch_all(&mut *tx)
        .await
        .db_err()?;
        let findings = sqlx::query(
            "SELECT f.registry, f.severity, COUNT(*) AS n
             FROM artifact_findings f
             JOIN artifact_verdicts v
               ON v.registry = f.registry AND v.package_name = f.package_name
              AND v.version = f.version
             WHERE v.state <> 'allowed'
             GROUP BY 1, 2",
        )
        .fetch_all(&mut *tx)
        .await
        .db_err()?;
        tx.commit().await.db_err()?;
        let mut out: BTreeMap<String, DailyState> = BTreeMap::new();
        for r in &states {
            let entry = out.entry(r.get("registry")).or_default();
            let n = r.get::<i64, _>("n").max(0) as u64;
            match r.get::<String, _>("state").as_str() {
                "allowed" => entry.allowed += n,
                "warned" => entry.warned += n,
                "quarantined" => entry.quarantined += n,
                "denied" => entry.denied += n,
                _ => {}
            }
        }
        for r in &findings {
            let entry = out.entry(r.get("registry")).or_default();
            let n = r.get::<i64, _>("n").max(0) as u64;
            match r.get::<String, _>("severity").as_str() {
                "critical" => entry.findings_critical += n,
                "high" => entry.findings_high += n,
                "medium" => entry.findings_medium += n,
                "low" => entry.findings_low += n,
                _ => entry.findings_unknown += n,
            }
        }
        Ok(out.into_iter().collect())
    }

    async fn compute_events(
        &self,
        day: NaiveDate,
        registries: &[String],
    ) -> Result<Vec<DailyEvents>, CoreError> {
        let (from, to) = day_bounds(day);
        let today = Utc::now().date_naive();
        let mut tx = self.bounded().await?;
        // The audit log's purge removes the access class only (where the
        // refusals live): a day before that class's oldest row is a day it no
        // longer holds — the surviving security-class rows say nothing of it.
        // shortcut: the oldest day itself may be partly purged and is counted;
        // `upsert_events` keeps the larger count, so it never lowers one.
        let oldest_event: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT MIN(created_at) FROM access_events
             WHERE action IN ('download', 'view_metadata')",
        )
        .fetch_one(&mut *tx)
        .await
        .db_err()?;
        let access_held = oldest_event.is_none_or(|t| t.date_naive() <= day);
        let jobs_held = day > today - Duration::days(SCAN_JOBS_KEPT_DAYS);

        let mut refusals: HashMap<String, BTreeMap<String, u64>> = HashMap::new();
        if access_held {
            for r in sqlx::query(
                "SELECT registry, reason_code, COUNT(*) AS n FROM access_events
                 WHERE outcome = 'denied' AND reason_code IS NOT NULL
                   AND created_at >= $1 AND created_at < $2 AND registry = ANY($3)
                 GROUP BY 1, 2",
            )
            .bind(from)
            .bind(to)
            .bind(registries)
            .fetch_all(&mut *tx)
            .await
            .db_err()?
            {
                refusals
                    .entry(r.get("registry"))
                    .or_default()
                    .insert(r.get("reason_code"), r.get::<i64, _>("n").max(0) as u64);
            }
        }
        let mut flags: HashMap<String, (u64, u64)> = HashMap::new();
        for r in sqlx::query(
            "SELECT registry,
                    COUNT(*) FILTER (WHERE first_seen >= $1 AND first_seen < $2) AS opened,
                    COUNT(*) FILTER (WHERE revoked_at >= $1 AND revoked_at < $2) AS revoked
             FROM package_flags
             WHERE registry = ANY($3)
               AND ((first_seen >= $1 AND first_seen < $2)
                 OR (revoked_at >= $1 AND revoked_at < $2))
             GROUP BY 1",
        )
        .bind(from)
        .bind(to)
        .bind(registries)
        .fetch_all(&mut *tx)
        .await
        .db_err()?
        {
            flags.insert(
                r.get("registry"),
                (
                    r.get::<i64, _>("opened").max(0) as u64,
                    r.get::<i64, _>("revoked").max(0) as u64,
                ),
            );
        }
        let mut jobs: HashMap<String, (u64, u64)> = HashMap::new();
        if jobs_held {
            for r in sqlx::query(
                "SELECT registry,
                        COUNT(*) FILTER (WHERE last_error IS NULL OR NOT starts_with(last_error, $4)) AS done,
                        COUNT(*) FILTER (WHERE starts_with(last_error, $4)) AS failed
                 FROM scan_jobs
                 WHERE completed_at >= $1 AND completed_at < $2 AND registry = ANY($3)
                 GROUP BY 1",
            )
            .bind(from)
            .bind(to)
            .bind(registries)
            .bind(EXHAUSTED_PREFIX)
            .fetch_all(&mut *tx)
            .await
            .db_err()?
            {
                jobs.insert(
                    r.get("registry"),
                    (
                        r.get::<i64, _>("done").max(0) as u64,
                        r.get::<i64, _>("failed").max(0) as u64,
                    ),
                );
            }
        }
        tx.commit().await.db_err()?;
        Ok(registries
            .iter()
            .map(|reg| {
                let codes = refusals.get(reg).cloned().unwrap_or_default();
                let (opened, revoked) = flags.get(reg).copied().unwrap_or_default();
                let (done, failed) = jobs.get(reg).copied().unwrap_or_default();
                DailyEvents {
                    refusals: access_held.then(|| codes.values().sum()),
                    refusals_by_code: access_held.then_some(codes),
                    flags_opened: Some(opened),
                    flags_revoked: Some(revoked),
                    jobs_done: jobs_held.then_some(done),
                    jobs_failed: jobs_held.then_some(failed),
                }
            })
            .collect())
    }

    async fn upsert_events(
        &self,
        registry: &str,
        day: NaiveDate,
        e: &DailyEvents,
    ) -> Result<(), CoreError> {
        let by_code = e
            .refusals_by_code
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|err| CoreError::Other(anyhow::anyhow!("refusals_by_code: {err}")))?;
        sqlx::query(
            "INSERT INTO security_daily
                 (registry, day, refusals, refusals_by_code, flags_opened, flags_revoked,
                  jobs_done, jobs_failed)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (registry, day) DO UPDATE SET
                 -- A day's refusals and jobs only grow until a purge or a sweep
                 -- removes rows: the larger count is the day's, never the tail.
                 -- GREATEST ignores NULL, so a NULL never erases a number.
                 refusals_by_code = CASE
                     WHEN security_daily.refusals IS NULL
                       OR EXCLUDED.refusals >= security_daily.refusals
                     THEN COALESCE(EXCLUDED.refusals_by_code, security_daily.refusals_by_code)
                     ELSE security_daily.refusals_by_code END,
                 refusals = GREATEST(EXCLUDED.refusals, security_daily.refusals),
                 flags_opened = COALESCE(EXCLUDED.flags_opened, security_daily.flags_opened),
                 flags_revoked = COALESCE(EXCLUDED.flags_revoked, security_daily.flags_revoked),
                 jobs_done = GREATEST(EXCLUDED.jobs_done, security_daily.jobs_done),
                 jobs_failed = GREATEST(EXCLUDED.jobs_failed, security_daily.jobs_failed)",
        )
        .bind(registry)
        .bind(day)
        .bind(as_i64(e.refusals))
        .bind(by_code)
        .bind(as_i64(e.flags_opened))
        .bind(as_i64(e.flags_revoked))
        .bind(as_i64(e.jobs_done))
        .bind(as_i64(e.jobs_failed))
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn upsert_state(
        &self,
        registry: &str,
        day: NaiveDate,
        s: &DailyState,
    ) -> Result<(), CoreError> {
        let v = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO security_daily
                 (registry, day, allowed, warned, quarantined, denied, findings_critical,
                  findings_high, findings_medium, findings_low, findings_unknown)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (registry, day) DO UPDATE SET
                 allowed = EXCLUDED.allowed, warned = EXCLUDED.warned,
                 quarantined = EXCLUDED.quarantined, denied = EXCLUDED.denied,
                 findings_critical = EXCLUDED.findings_critical,
                 findings_high = EXCLUDED.findings_high,
                 findings_medium = EXCLUDED.findings_medium,
                 findings_low = EXCLUDED.findings_low,
                 findings_unknown = EXCLUDED.findings_unknown",
        )
        .bind(registry)
        .bind(day)
        .bind(v(s.allowed))
        .bind(v(s.warned))
        .bind(v(s.quarantined))
        .bind(v(s.denied))
        .bind(v(s.findings_critical))
        .bind(v(s.findings_high))
        .bind(v(s.findings_medium))
        .bind(v(s.findings_low))
        .bind(v(s.findings_unknown))
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn range(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<SecurityDailyRow>, CoreError> {
        let rows = sqlx::query(
            "SELECT * FROM security_daily WHERE day >= $1 AND day <= $2 ORDER BY day, registry",
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter()
            .map(|r| {
                let n = |c: &str| as_u64(r.get::<Option<i64>, _>(c));
                let state = n("allowed").map(|allowed| DailyState {
                    allowed,
                    warned: n("warned").unwrap_or(0),
                    quarantined: n("quarantined").unwrap_or(0),
                    denied: n("denied").unwrap_or(0),
                    findings_critical: n("findings_critical").unwrap_or(0),
                    findings_high: n("findings_high").unwrap_or(0),
                    findings_medium: n("findings_medium").unwrap_or(0),
                    findings_low: n("findings_low").unwrap_or(0),
                    findings_unknown: n("findings_unknown").unwrap_or(0),
                });
                let by_code: Option<serde_json::Value> = r.get("refusals_by_code");
                let refusals_by_code = by_code
                    .map(serde_json::from_value::<BTreeMap<String, u64>>)
                    .transpose()
                    .map_err(|e| CoreError::Other(anyhow::anyhow!("refusals_by_code: {e}")))?;
                Ok(SecurityDailyRow {
                    registry: r.get("registry"),
                    day: r.get("day"),
                    state,
                    events: DailyEvents {
                        refusals: n("refusals"),
                        refusals_by_code,
                        flags_opened: n("flags_opened"),
                        flags_revoked: n("flags_revoked"),
                        jobs_done: n("jobs_done"),
                        jobs_failed: n("jobs_failed"),
                    },
                })
            })
            .collect()
    }

    async fn prune_before(&self, day: NaiveDate) -> Result<u64, CoreError> {
        let done = sqlx::query("DELETE FROM security_daily WHERE day < $1")
            .bind(day)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(done.rows_affected())
    }
}
