//! PostgreSQL [`ImportHistory`] (RFC 0021 §6.5): `release_import_runs`,
//! migration 059.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use batlehub_core::{entities::ImportRun, error::CoreError, ports::ImportHistory};

use super::DbResultExt;

/// `release_import_runs` (migration 059).
pub struct PgImportHistory {
    pool: PgPool,
}

impl PgImportHistory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ImportHistory for PgImportHistory {
    async fn record(&self, run: &ImportRun) -> Result<(), CoreError> {
        sqlx::query(
            r#"
            INSERT INTO release_import_runs
                (id, registry, repo, started_at, finished_at, imported, skipped, errors,
                 triggered_by, failures)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(run.id)
        .bind(&run.registry)
        .bind(&run.repo)
        .bind(run.started_at)
        .bind(run.finished_at)
        .bind(run.imported as i64)
        .bind(run.skipped as i64)
        .bind(run.errors as i64)
        .bind(&run.triggered_by)
        .bind(&run.failures)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn latest_for(&self, registry: &str) -> Result<Vec<ImportRun>, CoreError> {
        // `DISTINCT ON` rather than a window function: one registry can have
        // several imports configured into it and they fail independently, so
        // the console needs the newest run *per repo*, not the newest run.
        // Postgres orders the `DISTINCT ON` key first by definition, hence the
        // outer sort — the console reads newest first.
        let rows = sqlx::query(
            r#"
            SELECT * FROM (
                SELECT DISTINCT ON (repo)
                       id, registry, repo, started_at, finished_at,
                       imported, skipped, errors, triggered_by, failures
                FROM release_import_runs
                WHERE registry = $1
                ORDER BY repo, started_at DESC
            ) newest
            ORDER BY started_at DESC
            "#,
        )
        .bind(registry)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .iter()
            .map(|r| ImportRun {
                id: r.get("id"),
                registry: r.get("registry"),
                repo: r.get("repo"),
                started_at: r.get("started_at"),
                finished_at: r.get("finished_at"),
                imported: r.get::<i64, _>("imported").max(0) as u64,
                skipped: r.get::<i64, _>("skipped").max(0) as u64,
                errors: r.get::<i64, _>("errors").max(0) as u64,
                triggered_by: r.get("triggered_by"),
                failures: r.get("failures"),
            })
            .collect())
    }
}
