//! PostgreSQL stores for RFC 0018: verdicts with their findings, the leased
//! scan queue, and worker heartbeats.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use batlehub_core::entities::{
    Finding, FindingKind, FindingRow, FindingsFilter, PackageId, ReasonCode, ScanJob,
    ScanJobFilter, ScanTrigger, Severity, Verdict, VerdictState, EXHAUSTED_PREFIX,
};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{LeaderLock, QueuedCount, ScanQueue, VerdictRepository, WorkerRegistry};

use super::DbResultExt;

fn parse_err(what: &str, e: impl std::fmt::Display) -> CoreError {
    CoreError::Other(anyhow::anyhow!("{what}: {e}"))
}

/// The advisory lock release N's upgrade counts run under (RFC 0041 §6.8):
/// a transaction lock, so it is gone when the count is, and no replica
/// becomes a second rescan leader by holding it.
const UPGRADE_COUNT_KEY: i64 = 0x0041_c0c0_0000;

/// A `(registry, count)` query run once per process start on one replica:
/// under `pg_try_advisory_xact_lock` with a 30 s `statement_timeout`. `None`
/// when another replica holds the lock.
pub(crate) async fn count_under_lock(
    pool: &PgPool,
    sql: &'static str,
) -> Result<Option<Vec<(String, u64)>>, CoreError> {
    let mut tx = pool.begin().await.db_err()?;
    sqlx::query("SET LOCAL statement_timeout = '30s'")
        .execute(&mut *tx)
        .await
        .db_err()?;
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(UPGRADE_COUNT_KEY)
        .fetch_one(&mut *tx)
        .await
        .db_err()?;
    if !held {
        return Ok(None);
    }
    let rows = sqlx::query(sql).fetch_all(&mut *tx).await.db_err()?;
    tx.commit().await.db_err()?;
    Ok(Some(
        rows.iter()
            .map(|r| (r.get::<String, _>(0), r.get::<i64, _>(1) as u64))
            .collect(),
    ))
}

/// Three parallel arrays for an `UNNEST` batch.
fn unzip_coords(coords: &[PackageId]) -> (Vec<&str>, Vec<&str>, Vec<&str>) {
    let mut out = (
        Vec::with_capacity(coords.len()),
        Vec::with_capacity(coords.len()),
        Vec::with_capacity(coords.len()),
    );
    for c in coords {
        out.0.push(c.registry.as_str());
        out.1.push(c.name.as_str());
        out.2.push(c.version.as_str());
    }
    out
}

/// Registry, name and version — not the `artifact` a caller's id may carry.
fn same_coordinate(a: &PackageId, b: &PackageId) -> bool {
    a.registry == b.registry && a.name == b.name && a.version == b.version
}

/// `prefix` as a `LIKE … ESCAPE '\'` pattern that matches it literally.
fn like_prefix(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// `artifact_verdicts` + `artifact_findings` (migrations 049, 050).
pub struct PgVerdictRepository {
    pool: PgPool,
}

impl PgVerdictRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn findings_for(&self, pkg: &PackageId) -> Result<Vec<Finding>, CoreError> {
        let rows = sqlx::query(
            "SELECT scanner, kind, code, severity, reference, summary, confidence, available_at, raw
             FROM artifact_findings
             WHERE registry = $1 AND package_name = $2 AND version = $3
             ORDER BY created_at, scanner",
        )
        .bind(&pkg.registry)
        .bind(&pkg.name)
        .bind(&pkg.version)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.into_iter()
            .map(|r| {
                let kind: String = r.get("kind");
                let code: String = r.get("code");
                let severity: String = r.get("severity");
                Ok(Finding {
                    scanner: r.get("scanner"),
                    kind: kind
                        .parse::<FindingKind>()
                        .map_err(|e| parse_err("artifact_findings.kind", e))?,
                    code: ReasonCode::parse_stored(&code),
                    severity: Severity::parse_stored(&severity),
                    reference: r.try_get("reference").ok(),
                    summary: r.get("summary"),
                    confidence: r.try_get::<i16, _>("confidence").ok().map(|c| c as u8),
                    available_at: r.try_get("available_at").ok(),
                    raw: r.try_get("raw").unwrap_or(serde_json::Value::Null),
                    judged_as: None,
                })
            })
            .collect()
    }

    fn row_to_verdict(
        r: &sqlx::postgres::PgRow,
        findings: Vec<Finding>,
    ) -> Result<Verdict, CoreError> {
        let state: String = r.get("state");
        let codes: Vec<String> = r.get("reason_codes");
        Ok(Verdict {
            package: PackageId::new(
                r.get::<String, _>("registry"),
                r.get::<String, _>("package_name"),
                r.get::<String, _>("version"),
            ),
            state: state
                .parse()
                .map_err(|e| parse_err("artifact_verdicts.state", e))?,
            reason_codes: codes.iter().map(|c| ReasonCode::parse_stored(c)).collect(),
            findings,
            policy_ref: r.get("policy_ref"),
            available_at: r.try_get("available_at").ok(),
            evaluated_at: r.get("evaluated_at"),
            last_scanned_at: r.try_get("last_scanned_at").ok(),
            scanners_done: r.get("scanners_done"),
        })
    }
}

#[async_trait]
impl VerdictRepository for PgVerdictRepository {
    async fn upsert(&self, v: &Verdict) -> Result<(), CoreError> {
        let mut tx = self.pool.begin().await.db_err()?;
        let codes: Vec<&str> = v.reason_codes.iter().map(|c| c.as_str()).collect();
        sqlx::query(
            "INSERT INTO artifact_verdicts
                 (registry, package_name, version, state, reason_codes, policy_ref,
                  available_at, evaluated_at, last_scanned_at, scanners_done)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (registry, package_name, version) DO UPDATE SET
                 state = EXCLUDED.state,
                 reason_codes = EXCLUDED.reason_codes,
                 policy_ref = EXCLUDED.policy_ref,
                 available_at = EXCLUDED.available_at,
                 evaluated_at = EXCLUDED.evaluated_at,
                 last_scanned_at = EXCLUDED.last_scanned_at,
                 scanners_done = EXCLUDED.scanners_done",
        )
        .bind(&v.package.registry)
        .bind(&v.package.name)
        .bind(&v.package.version)
        .bind(v.state.as_str())
        .bind(&codes)
        .bind(&v.policy_ref)
        .bind(v.available_at)
        .bind(v.evaluated_at)
        .bind(v.last_scanned_at)
        .bind(&v.scanners_done)
        .execute(&mut *tx)
        .await
        .db_err()?;
        sqlx::query(
            "DELETE FROM artifact_findings WHERE registry = $1 AND package_name = $2 AND version = $3",
        )
        .bind(&v.package.registry)
        .bind(&v.package.name)
        .bind(&v.package.version)
        .execute(&mut *tx)
        .await
        .db_err()?;
        for f in &v.findings {
            sqlx::query(
                "INSERT INTO artifact_findings
                     (id, registry, package_name, version, scanner, kind, code, severity,
                      reference, summary, confidence, available_at, raw)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            )
            .bind(Uuid::new_v4())
            .bind(&v.package.registry)
            .bind(&v.package.name)
            .bind(&v.package.version)
            .bind(&f.scanner)
            .bind(f.kind.as_str())
            .bind(f.code.as_str())
            .bind(f.severity.as_str())
            .bind(&f.reference)
            .bind(&f.summary)
            .bind(f.confidence.map(|c| c as i16))
            .bind(f.available_at)
            .bind(&f.raw)
            .execute(&mut *tx)
            .await
            .db_err()?;
        }
        tx.commit().await.db_err()?;
        Ok(())
    }

    async fn get(&self, pkg: &PackageId) -> Result<Option<Verdict>, CoreError> {
        let row = sqlx::query(
            "SELECT registry, package_name, version, state, reason_codes, policy_ref,
                    available_at, evaluated_at, last_scanned_at, scanners_done
             FROM artifact_verdicts
             WHERE registry = $1 AND package_name = $2 AND version = $3",
        )
        .bind(&pkg.registry)
        .bind(&pkg.name)
        .bind(&pkg.version)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let findings = self.findings_for(pkg).await?;
        Ok(Some(Self::row_to_verdict(&row, findings)?))
    }

    /// Findings are not loaded: a status reads the state and the codes.
    async fn get_many(&self, coords: &[PackageId]) -> Result<Vec<Option<Verdict>>, CoreError> {
        if coords.is_empty() {
            return Ok(Vec::new());
        }
        let (regs, names, vers) = unzip_coords(coords);
        let rows = sqlx::query(
            "SELECT v.registry, v.package_name, v.version, v.state, v.reason_codes, v.policy_ref,
                    v.available_at, v.evaluated_at, v.last_scanned_at, v.scanners_done
             FROM (SELECT DISTINCT * FROM UNNEST($1::text[], $2::text[], $3::text[])) AS c(r, n, v2)
             JOIN artifact_verdicts v
               ON v.registry = c.r AND v.package_name = c.n AND v.version = c.v2",
        )
        .bind(&regs)
        .bind(&names)
        .bind(&vers)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        let found = rows
            .iter()
            .map(|r| Self::row_to_verdict(r, Vec::new()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(coords
            .iter()
            .map(|c| {
                found
                    .iter()
                    .find(|v| same_coordinate(c, &v.package))
                    .cloned()
            })
            .collect())
    }

    /// Findings are not loaded: the listing filter reads states and clocks.
    async fn list_for_package(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<Verdict>, CoreError> {
        let rows = sqlx::query(
            "SELECT registry, package_name, version, state, reason_codes, policy_ref,
                    available_at, evaluated_at, last_scanned_at, scanners_done
             FROM artifact_verdicts
             WHERE registry = $1 AND package_name = $2",
        )
        .bind(registry)
        .bind(package)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter()
            .map(|r| Self::row_to_verdict(r, Vec::new()))
            .collect()
    }

    async fn list_due_for_rescan(
        &self,
        registry: &str,
        before: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<PackageId>, CoreError> {
        let rows = sqlx::query(
            "SELECT registry, package_name, version
             FROM artifact_verdicts
             WHERE registry = $1
               AND (last_scanned_at IS NULL OR last_scanned_at < $2)
             ORDER BY last_scanned_at ASC NULLS FIRST, evaluated_at ASC
             LIMIT $3",
        )
        .bind(registry)
        .bind(before)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .iter()
            .map(|r| {
                PackageId::new(
                    r.get::<String, _>("registry"),
                    r.get::<String, _>("package_name"),
                    r.get::<String, _>("version"),
                )
            })
            .collect())
    }

    async fn count_unrated_by_registry(&self) -> Result<Option<Vec<(String, u64)>>, CoreError> {
        count_under_lock(
            &self.pool,
            "SELECT registry, COUNT(DISTINCT (package_name, version))
             FROM artifact_findings WHERE severity = 'unknown' GROUP BY registry",
        )
        .await
    }

    /// One statement: the page and, through `COUNT(*) OVER ()`, the total.
    /// `severity` is `TEXT`, so it is ranked by a `CASE` that reads a value
    /// it does not know as critical, as `Severity::parse_stored` does.
    async fn list_findings(
        &self,
        filter: &FindingsFilter,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<FindingRow>, u64), CoreError> {
        if filter.registries.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let rows = sqlx::query(
            r"WITH f AS (
                SELECT f.registry, f.package_name, f.version, f.scanner, f.kind, f.code,
                       f.severity, f.reference, f.summary, f.confidence, f.available_at,
                       f.id, v.state,
                       CASE lower(f.severity)
                           WHEN 'unknown' THEN 0 WHEN 'low' THEN 1
                           WHEN 'medium' THEN 2 WHEN 'moderate' THEN 2
                           WHEN 'high' THEN 3 ELSE 4 END AS rank
                FROM artifact_findings f
                JOIN artifact_verdicts v
                  ON v.registry = f.registry AND v.package_name = f.package_name
                 AND v.version = f.version
                WHERE f.registry = ANY($1)
                  AND ($3::text IS NULL OR f.scanner = $3)
                  AND ($4::text IS NULL OR f.reference LIKE $4 ESCAPE '\')
                  AND ($5::text IS NULL OR v.state = $5)
              )
              SELECT *, COUNT(*) OVER () AS total FROM f
              WHERE ($2::int IS NULL OR rank >= $2)
              ORDER BY CASE WHEN $6::bool THEN reference END NULLS LAST, rank DESC,
                       reference NULLS LAST, registry, package_name, version, scanner, id
              LIMIT $7 OFFSET $8",
        )
        .bind(&filter.registries)
        // `Severity` is declared unknown → critical, the ranks of the `CASE`.
        .bind(filter.min_severity.map(|s| s as i32))
        .bind(filter.scanner.as_deref())
        .bind(filter.reference_prefix.as_deref().map(like_prefix))
        .bind(filter.verdict.map(|v| v.as_str()))
        .bind(filter.by_reference)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        let total = rows.first().map_or(0, |r| r.get::<i64, _>("total") as u64);
        let items = rows
            .iter()
            .map(|r| {
                let kind: String = r.get("kind");
                let code: String = r.get("code");
                let severity: String = r.get("severity");
                let state: String = r.get("state");
                Ok(FindingRow {
                    package: PackageId::new(
                        r.get::<String, _>("registry"),
                        r.get::<String, _>("package_name"),
                        r.get::<String, _>("version"),
                    ),
                    finding: Finding {
                        scanner: r.get("scanner"),
                        kind: kind
                            .parse::<FindingKind>()
                            .map_err(|e| parse_err("artifact_findings.kind", e))?,
                        code: ReasonCode::parse_stored(&code),
                        severity: Severity::parse_stored(&severity),
                        reference: r.try_get("reference").ok(),
                        summary: r.get("summary"),
                        confidence: r.try_get::<i16, _>("confidence").ok().map(|c| c as u8),
                        available_at: r.try_get("available_at").ok(),
                        raw: serde_json::Value::Null,
                        judged_as: None,
                    },
                    verdict: state
                        .parse()
                        .map_err(|e| parse_err("artifact_verdicts.state", e))?,
                })
            })
            .collect::<Result<Vec<_>, CoreError>>()?;
        Ok((items, total))
    }

    async fn count_by_registry_and_state(
        &self,
    ) -> Result<Vec<(String, VerdictState, u64)>, CoreError> {
        // `artifact_verdicts_state_idx` is `(registry, state)`: one index scan.
        let rows = sqlx::query(
            "SELECT registry, state, COUNT(*) AS n FROM artifact_verdicts GROUP BY 1, 2",
        )
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter()
            .map(|r| {
                let state: String = r.get("state");
                Ok((
                    r.get("registry"),
                    state
                        .parse()
                        .map_err(|e| parse_err("artifact_verdicts.state", e))?,
                    r.get::<i64, _>("n") as u64,
                ))
            })
            .collect()
    }

    async fn list_by_state(
        &self,
        registry: &str,
        state: VerdictState,
        limit: u64,
    ) -> Result<Vec<Verdict>, CoreError> {
        let rows = sqlx::query(
            "SELECT registry, package_name, version, state, reason_codes, policy_ref,
                    available_at, evaluated_at, last_scanned_at, scanners_done
             FROM artifact_verdicts
             WHERE registry = $1 AND state = $2
             ORDER BY evaluated_at DESC
             LIMIT $3",
        )
        .bind(registry)
        .bind(state.as_str())
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let pkg = PackageId::new(
                r.get::<String, _>("registry"),
                r.get::<String, _>("package_name"),
                r.get::<String, _>("version"),
            );
            let findings = self.findings_for(&pkg).await?;
            out.push(Self::row_to_verdict(&r, findings)?);
        }
        Ok(out)
    }
}

/// `scan_jobs` (migration 051), leased with `FOR UPDATE SKIP LOCKED`.
pub struct PgScanQueue {
    pool: PgPool,
    /// One advisory-lock leader per key asked for (RFC 0018 §6.3), the same
    /// election the audit trail runs (TD-0002 §6): holding one key is not
    /// leading another.
    leaders: std::sync::Mutex<HashMap<i64, Arc<super::PgAdvisoryLeader>>>,
    /// Signalled by [`ScanQueue::enqueue`] when it creates a row, so this
    /// process's own worker wakes without polling. `notify_one` keeps a permit
    /// when nobody is waiting, so a job enqueued mid-pass is not missed.
    work: tokio::sync::Notify,
}

impl PgScanQueue {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            leaders: Default::default(),
            work: tokio::sync::Notify::new(),
        }
    }

    fn row_to_job(r: &sqlx::postgres::PgRow) -> Result<ScanJob, CoreError> {
        let trigger: String = r.get("trigger");
        Ok(ScanJob {
            id: r.get("id"),
            package: PackageId::new(
                r.get::<String, _>("registry"),
                r.get::<String, _>("package_name"),
                r.get::<String, _>("version"),
            ),
            published_at: r.try_get("published_at").ok(),
            artifact_sha256: r.try_get("artifact_sha256").ok(),
            trigger: trigger
                .parse()
                .map_err(|e| parse_err("scan_jobs.trigger", e))?,
            attempts: r.get::<i32, _>("attempts") as u32,
            leased_until: r.try_get("leased_until").ok(),
            created_at: r.get("created_at"),
            deferred_since: r
                .try_get::<Option<String>, _>("last_error")
                .ok()
                .flatten()
                .and_then(|e| deferred_since(&e)),
            leased_by: r.try_get::<Option<String>, _>("leased_by").ok().flatten(),
            last_error: r.try_get::<Option<String>, _>("last_error").ok().flatten(),
            completed_at: r
                .try_get::<Option<DateTime<Utc>>, _>("completed_at")
                .ok()
                .flatten(),
        })
    }
}

/// The prefix `defer` writes into `last_error`: the anchor of the 24-hour
/// deferral budget (RFC 0041 decision 44), in a column that already exists.
const DEFERRED_SINCE: &str = "deferred since ";

fn deferred_since(last_error: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(last_error.strip_prefix(DEFERRED_SINCE)?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

const JOB_COLUMNS: &str = "id, registry, package_name, version, published_at, artifact_sha256, trigger, attempts, leased_until, leased_by, created_at, last_error, completed_at";

impl PgScanQueue {
    /// One lease pick: up to `n` open, unleased, unexhausted jobs in
    /// `order`, locked with `SKIP LOCKED` so any number of workers share
    /// the table without two of them taking one job.
    async fn lease_ordered(
        &self,
        worker_id: &str,
        registries: &[String],
        n: u32,
        lease_secs: u64,
        max_attempts: u32,
        order: &str,
    ) -> Result<Vec<ScanJob>, CoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE scan_jobs SET
                 leased_until = NOW() + make_interval(secs => $3),
                 leased_by = $1,
                 attempts = attempts + 1
             WHERE id IN (
                 SELECT id FROM scan_jobs
                 WHERE completed_at IS NULL
                   AND (leased_until IS NULL OR leased_until < NOW())
                   AND attempts < $4
                   AND (cardinality($5::text[]) = 0 OR registry = ANY($5))
                 ORDER BY {order}
                 LIMIT $2
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING {JOB_COLUMNS}"
        )))
        .bind(worker_id)
        .bind(n as i64)
        .bind(lease_secs as f64)
        .bind(max_attempts as i32)
        .bind(registries)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter().map(Self::row_to_job).collect()
    }
}

#[async_trait]
impl ScanQueue for PgScanQueue {
    async fn enqueue(
        &self,
        package: &PackageId,
        published_at: Option<DateTime<Utc>>,
        trigger: ScanTrigger,
    ) -> Result<bool, CoreError> {
        // An open job that the new one outranks is promoted: its trigger and
        // priority rise, and a deferral (`leased_by IS NULL` with a future
        // `leased_until`) is cleared — a running lease is left alone. A
        // flag or a webhook is not lost behind a deferred rescan (RFC 0041
        // decision 44). `xmax = 0` tells an insert from that update.
        let row = sqlx::query(
            "INSERT INTO scan_jobs (id, registry, package_name, version, published_at, trigger, priority)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (registry, package_name, version) WHERE completed_at IS NULL
             DO UPDATE SET
                 trigger = EXCLUDED.trigger,
                 priority = EXCLUDED.priority,
                 leased_until = CASE WHEN scan_jobs.leased_by IS NULL THEN NULL
                                     ELSE scan_jobs.leased_until END
             WHERE EXCLUDED.priority < scan_jobs.priority
             RETURNING (xmax = 0) AS inserted",
        )
        .bind(Uuid::new_v4())
        .bind(&package.registry)
        .bind(&package.name)
        .bind(&package.version)
        .bind(published_at)
        .bind(trigger.as_str())
        .bind(trigger.priority())
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        let Some(row) = row else {
            return Ok(false);
        };
        self.work.notify_one();
        Ok(row.get::<bool, _>("inserted"))
    }

    async fn wait_for_work(&self, max: std::time::Duration) {
        let _ = tokio::time::timeout(max, self.work.notified()).await;
    }

    async fn lease(
        &self,
        worker_id: &str,
        registries: &[String],
        n: u32,
        lease_secs: u64,
        max_attempts: u32,
    ) -> Result<Vec<ScanJob>, CoreError> {
        // The subquery picks and locks; the update leases. `SKIP LOCKED` is what
        // lets any number of workers share the table without handing one job to
        // two of them.
        // The anti-starvation slot (RFC 0018 §4.2, decision 16): `n - 1`
        // slots go to the queue in priority order and one to the *lowest*
        // tier waiting, so a flood of `FirstSeen` never parks a backfill
        // forever. A job leased by the first pick is `leased_until > NOW()`
        // and invisible to the second, which is what keeps the two from
        // handing out the same row.
        let mut jobs = self
            .lease_ordered(
                worker_id,
                registries,
                n.saturating_sub(1).max(1),
                lease_secs,
                max_attempts,
                "priority, created_at",
            )
            .await?;
        // A first pick that came back short drained everything leasable, so
        // there is no lower tier left waiting and the second statement would
        // only find nothing — which, on an idle queue, it did every pass.
        let drained = jobs.len() < n.saturating_sub(1).max(1) as usize;
        if n >= 2 && !drained {
            let low = self
                .lease_ordered(
                    worker_id,
                    registries,
                    1,
                    lease_secs,
                    max_attempts,
                    "priority DESC, created_at",
                )
                .await?;
            jobs.extend(low);
        }
        Ok(jobs)
    }

    async fn try_lead(&self, key: i64) -> Result<bool, CoreError> {
        let leader = Arc::clone(
            self.leaders
                .lock()
                .expect("scan queue leaders lock poisoned")
                .entry(key)
                .or_insert_with(|| Arc::new(super::PgAdvisoryLeader::new(self.pool.clone(), key))),
        );
        leader.try_lead().await
    }

    async fn heartbeat(&self, job_id: Uuid, lease_secs: u64) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE scan_jobs SET leased_until = NOW() + make_interval(secs => $2)
             WHERE id = $1 AND completed_at IS NULL",
        )
        .bind(job_id)
        .bind(lease_secs as f64)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn complete(&self, job_id: Uuid, worker: Option<&str>) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE scan_jobs SET completed_at = NOW(), leased_until = NULL
             WHERE id = $1 AND ($2::text IS NULL OR leased_by = $2)",
        )
        .bind(job_id)
        .bind(worker)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn complete_exhausted(&self, job_id: Uuid, last_error: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE scan_jobs SET completed_at = NOW(), leased_until = NULL, last_error = $2
             WHERE id = $1 AND completed_at IS NULL",
        )
        .bind(job_id)
        .bind(last_error)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    /// One statement for the page: the open job on
    /// `scan_jobs_open_coordinate_idx`, the latest closed one on
    /// `scan_jobs_closed_coordinate_idx` (migration 068). `ord` keeps the
    /// answer in the caller's order and maps duplicates back.
    async fn jobs_for(
        &self,
        coords: &[PackageId],
    ) -> Result<Vec<(Option<ScanJob>, Option<ScanJob>)>, CoreError> {
        let mut out = vec![(None, None); coords.len()];
        if coords.is_empty() {
            return Ok(out);
        }
        let (regs, names, vers) = unzip_coords(coords);
        let cols = JOB_COLUMNS
            .split(", ")
            .map(|c| format!("j.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "WITH c(r, n, v) AS (SELECT DISTINCT * FROM UNNEST($1::text[], $2::text[], $3::text[]))
             SELECT TRUE AS open, {cols} FROM c
               JOIN scan_jobs j ON j.registry = c.r AND j.package_name = c.n AND j.version = c.v
              WHERE j.completed_at IS NULL
             UNION ALL
             SELECT FALSE AS open, {cols} FROM c
               CROSS JOIN LATERAL (
                 SELECT * FROM scan_jobs j
                  WHERE j.registry = c.r AND j.package_name = c.n AND j.version = c.v
                    AND j.completed_at IS NOT NULL
                  ORDER BY j.completed_at DESC
                  LIMIT 1
               ) j"
        )))
        .bind(&regs)
        .bind(&names)
        .bind(&vers)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        for r in &rows {
            let job = Self::row_to_job(r)?;
            let open: bool = r.get("open");
            for (i, c) in coords.iter().enumerate() {
                if same_coordinate(c, &job.package) {
                    if open {
                        out[i].0 = Some(job.clone());
                    } else {
                        out[i].1 = Some(job.clone());
                    }
                }
            }
        }
        Ok(out)
    }

    /// Open rows on the open-job partial indexes; `failed` takes the latest
    /// closed row per coordinate of `registries` (`DISTINCT ON`, on
    /// `scan_jobs_closed_coordinate_idx`) and keeps the exhausted ones with no
    /// open job. `COUNT(*) OVER ()` carries the total in the same statement.
    async fn list_jobs(
        &self,
        registries: &[String],
        filter: ScanJobFilter,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<ScanJob>, u64), CoreError> {
        if registries.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let live = "(leased_by IS NOT NULL AND leased_until > NOW())";
        let sql = match filter {
            ScanJobFilter::Failed => format!(
                "WITH last AS (
                   SELECT DISTINCT ON (registry, package_name, version) {JOB_COLUMNS}
                     FROM scan_jobs
                    WHERE completed_at IS NOT NULL AND registry = ANY($1)
                    ORDER BY registry, package_name, version, completed_at DESC)
                 SELECT *, COUNT(*) OVER () AS total FROM last l
                  WHERE left(l.last_error, length($4)) = $4
                    AND NOT EXISTS (
                      SELECT 1 FROM scan_jobs o
                       WHERE o.completed_at IS NULL AND o.registry = l.registry
                         AND o.package_name = l.package_name AND o.version = l.version)
                  ORDER BY l.completed_at DESC, l.id
                  LIMIT $2 OFFSET $3"
            ),
            open => {
                let state = match open {
                    ScanJobFilter::Queued => format!("AND NOT {live}"),
                    ScanJobFilter::Scanning => format!("AND {live}"),
                    _ => String::new(),
                };
                format!(
                    "SELECT {JOB_COLUMNS}, COUNT(*) OVER () AS total FROM scan_jobs
                      WHERE completed_at IS NULL AND registry = ANY($1)
                        {state}
                      ORDER BY {live} DESC, created_at, id
                      LIMIT $2 OFFSET $3"
                )
            }
        };
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(registries)
            .bind(limit as i64)
            .bind(offset as i64);
        if filter == ScanJobFilter::Failed {
            q = q.bind(EXHAUSTED_PREFIX);
        }
        let rows = q.fetch_all(&self.pool).await.db_err()?;
        let total = rows.first().map_or(0, |r| r.get::<i64, _>("total") as u64);
        Ok((
            rows.iter()
                .map(Self::row_to_job)
                .collect::<Result<_, _>>()?,
            total,
        ))
    }

    async fn fail(&self, job_id: Uuid, worker: &str, error: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE scan_jobs SET leased_until = NULL, leased_by = NULL, last_error = $3
             WHERE id = $1 AND leased_by = $2 AND completed_at IS NULL",
        )
        .bind(job_id)
        .bind(worker)
        .bind(error)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    /// The column is a not-before time, the attempt is given back,
    /// `last_error` anchors the 24-hour budget across restarts, and
    /// `leased_by` is cleared — which is what marks the row deferred rather
    /// than running, and keeps a stale worker from touching it again.
    async fn defer(
        &self,
        job_id: Uuid,
        worker: &str,
        not_before: DateTime<Utc>,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE scan_jobs SET
                 leased_until = $3,
                 leased_by = NULL,
                 attempts = GREATEST(attempts - 1, 0),
                 last_error = CASE WHEN last_error LIKE 'deferred since %' THEN last_error
                                   ELSE 'deferred since '
                                        || to_char(NOW() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
                              END
             WHERE id = $1 AND leased_by = $2 AND completed_at IS NULL",
        )
        .bind(job_id)
        .bind(worker)
        .bind(not_before)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn exhausted(&self, max_attempts: u32, n: u32) -> Result<Vec<ScanJob>, CoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {JOB_COLUMNS} FROM scan_jobs
             WHERE completed_at IS NULL
               AND attempts >= $1
               AND (leased_until IS NULL OR leased_until < NOW())
             ORDER BY created_at
             LIMIT $2"
        )))
        .bind(max_attempts as i32)
        .bind(n as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter().map(Self::row_to_job).collect()
    }

    async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError> {
        let rows = sqlx::query(
            "SELECT registry, trigger,
                    COUNT(*) FILTER (WHERE leased_until IS NULL OR leased_until < NOW()) AS n,
                    COUNT(*) FILTER (WHERE leased_by IS NULL AND leased_until >= NOW()) AS deferred
             FROM scan_jobs
             WHERE completed_at IS NULL
             GROUP BY registry, trigger",
        )
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter()
            .map(|r| {
                let trigger: String = r.get("trigger");
                Ok(QueuedCount {
                    registry: r.get("registry"),
                    trigger: trigger
                        .parse()
                        .map_err(|e| parse_err("scan_jobs.trigger", e))?,
                    count: r.get::<i64, _>("n") as u64,
                    deferred: r.get::<i64, _>("deferred") as u64,
                })
            })
            .collect()
    }
}

/// `worker_heartbeats` (migration 052).
pub struct PgWorkerRegistry {
    pool: PgPool,
}

impl PgWorkerRegistry {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl WorkerRegistry for PgWorkerRegistry {
    async fn heartbeat(&self, worker_id: &str, registries: &[String]) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO worker_heartbeats (worker_id, last_seen, registries)
             VALUES ($1, NOW(), $2)
             ON CONFLICT (worker_id) DO UPDATE SET last_seen = NOW(), registries = EXCLUDED.registries",
        )
        .bind(worker_id)
        .bind(registries)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn live_count(&self, within_secs: u64) -> Result<u64, CoreError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM worker_heartbeats WHERE last_seen > NOW() - make_interval(secs => $1)",
        )
        .bind(within_secs as f64)
        .fetch_one(&self.pool)
        .await
        .db_err()?;
        Ok(n as u64)
    }
}
