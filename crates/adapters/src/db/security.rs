//! PostgreSQL stores for RFC 0018: verdicts with their findings, the leased
//! scan queue, and worker heartbeats.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use batlehub_core::entities::{
    Finding, FindingKind, PackageId, ReasonCode, ScanJob, ScanTrigger, Severity, Verdict,
    VerdictState,
};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{QueuedCount, ScanQueue, VerdictRepository, WorkerRegistry};

use super::DbResultExt;

fn parse_err(what: &str, e: impl std::fmt::Display) -> CoreError {
    CoreError::Other(anyhow::anyhow!("{what}: {e}"))
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
                    code: code
                        .parse::<ReasonCode>()
                        .map_err(|e| parse_err("artifact_findings.code", e))?,
                    severity: Severity::parse(&severity).unwrap_or(Severity::Unknown),
                    reference: r.try_get("reference").ok(),
                    summary: r.get("summary"),
                    confidence: r.try_get::<i16, _>("confidence").ok().map(|c| c as u8),
                    available_at: r.try_get("available_at").ok(),
                    raw: r.try_get("raw").unwrap_or(serde_json::Value::Null),
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
            reason_codes: codes
                .iter()
                .map(|c| {
                    c.parse::<ReasonCode>()
                        .map_err(|e| parse_err("artifact_verdicts.reason_codes", e))
                })
                .collect::<Result<_, _>>()?,
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
}

impl PgScanQueue {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
        })
    }
}

const JOB_COLUMNS: &str = "id, registry, package_name, version, published_at, artifact_sha256, trigger, attempts, leased_until, created_at";

#[async_trait]
impl ScanQueue for PgScanQueue {
    async fn enqueue(
        &self,
        package: &PackageId,
        published_at: Option<DateTime<Utc>>,
        trigger: ScanTrigger,
    ) -> Result<bool, CoreError> {
        let result = sqlx::query(
            "INSERT INTO scan_jobs (id, registry, package_name, version, published_at, trigger, priority)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(&package.registry)
        .bind(&package.name)
        .bind(&package.version)
        .bind(published_at)
        .bind(trigger.as_str())
        .bind(trigger.priority())
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(result.rows_affected() == 1)
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
                 ORDER BY priority, created_at
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

    async fn complete(&self, job_id: Uuid) -> Result<(), CoreError> {
        sqlx::query("UPDATE scan_jobs SET completed_at = NOW(), leased_until = NULL WHERE id = $1")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(())
    }

    async fn fail(&self, job_id: Uuid, error: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE scan_jobs SET leased_until = NULL, leased_by = NULL, last_error = $2
             WHERE id = $1 AND completed_at IS NULL",
        )
        .bind(job_id)
        .bind(error)
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
            "SELECT registry, trigger, COUNT(*) AS n FROM scan_jobs
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
