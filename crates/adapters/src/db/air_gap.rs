//! PostgreSQL [`MissRecorder`] (RFC 0008 §6.3): `missing_content`,
//! migration 056.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use batlehub_core::{
    entities::{
        BundleImport, ContentMiss, MissFilter, MissKind, RecordedMiss, MAX_MISSES_PER_REGISTRY,
    },
    error::CoreError,
    ports::{BundleHistory, MissRecorder},
};

use super::DbResultExt;

pub struct PgMissRecorder {
    pool: PgPool,
}

impl PgMissRecorder {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_miss(r: &sqlx::postgres::PgRow) -> RecordedMiss {
    let kind: String = r.get("kind");
    RecordedMiss {
        registry: r.get("registry"),
        storage_key: r.get("storage_key"),
        kind: MissKind::parse(&kind).unwrap_or(MissKind::Artifact),
        coordinate: r.get("coordinate"),
        first_seen: r.get("first_seen"),
        last_seen: r.get("last_seen"),
        count: r.get::<i64, _>("count").max(0) as u64,
    }
}

#[async_trait]
impl MissRecorder for PgMissRecorder {
    async fn record(&self, miss: &ContentMiss, now: DateTime<Utc>) -> Result<(), CoreError> {
        // One statement: the primary key is what makes "recorded once per
        // unique key" a property of the table rather than of the caller.
        let res = sqlx::query(
            r#"
            INSERT INTO missing_content
                (registry, storage_key, kind, coordinate, first_seen, last_seen, count)
            VALUES ($1, $2, $3, $4, $5, $5, 1)
            ON CONFLICT (registry, storage_key) DO UPDATE
                SET last_seen = EXCLUDED.last_seen,
                    count     = missing_content.count + 1
            RETURNING (xmax = 0) AS created
            "#,
        )
        .bind(&miss.registry)
        .bind(&miss.storage_key)
        .bind(miss.kind.as_str())
        .bind(&miss.coordinate)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .db_err()?;

        // Only a *new* row can push the registry over its cap, so the sweep
        // runs on an insert and not on the far commoner bump.
        if res.get::<bool, _>("created") {
            sqlx::query(
                r#"
                DELETE FROM missing_content
                WHERE registry = $1
                  AND storage_key IN (
                      SELECT storage_key FROM missing_content
                      WHERE registry = $1
                      ORDER BY last_seen DESC
                      OFFSET $2
                  )
                "#,
            )
            .bind(&miss.registry)
            .bind(MAX_MISSES_PER_REGISTRY as i64)
            .execute(&self.pool)
            .await
            .db_err()?;
        }
        Ok(())
    }

    async fn list(&self, filter: &MissFilter) -> Result<Vec<RecordedMiss>, CoreError> {
        let limit = if filter.limit == 0 {
            i64::MAX
        } else {
            filter.limit as i64
        };
        let rows = sqlx::query(
            r#"
            SELECT registry, storage_key, kind, coordinate, first_seen, last_seen, count
            FROM missing_content
            WHERE ($1::text IS NULL OR registry = $1)
              AND ($2::text IS NULL OR kind = $2)
            ORDER BY count DESC, last_seen DESC, storage_key
            LIMIT $3 OFFSET $4
            "#,
        )
        .bind(&filter.registry)
        .bind(filter.kind.map(|k| k.as_str()))
        .bind(limit)
        .bind(filter.offset as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows.iter().map(row_to_miss).collect())
    }

    async fn count(&self, filter: &MissFilter) -> Result<u64, CoreError> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) AS n FROM missing_content
            WHERE ($1::text IS NULL OR registry = $1)
              AND ($2::text IS NULL OR kind = $2)
            "#,
        )
        .bind(&filter.registry)
        .bind(filter.kind.map(|k| k.as_str()))
        .fetch_one(&self.pool)
        .await
        .db_err()?;
        Ok(row.get::<i64, _>("n").max(0) as u64)
    }

    async fn purge(&self, before: DateTime<Utc>, registry: Option<&str>) -> Result<u64, CoreError> {
        let res = sqlx::query(
            "DELETE FROM missing_content
             WHERE last_seen < $1 AND ($2::text IS NULL OR registry = $2)",
        )
        .bind(before)
        .bind(registry)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(res.rows_affected())
    }
}

/// `bundle_imports` (migration 057).
pub struct PgBundleHistory {
    pool: PgPool,
}

impl PgBundleHistory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl BundleHistory for PgBundleHistory {
    async fn record(&self, import: &BundleImport) -> Result<(), CoreError> {
        sqlx::query(
            r#"
            INSERT INTO bundle_imports
                (bundle_id, signer_key, imported_at, imported_by, entries, blobs, rejected,
                 rejected_sample, created_from)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (bundle_id) DO UPDATE
                SET imported_at     = EXCLUDED.imported_at,
                    imported_by     = EXCLUDED.imported_by,
                    entries         = EXCLUDED.entries,
                    blobs           = EXCLUDED.blobs,
                    rejected        = EXCLUDED.rejected,
                    rejected_sample = EXCLUDED.rejected_sample
            "#,
        )
        .bind(&import.bundle_id)
        .bind(&import.signer_key)
        .bind(import.imported_at)
        .bind(&import.imported_by)
        .bind(import.entries as i64)
        .bind(import.blobs as i64)
        .bind(import.rejected as i64)
        .bind(&import.rejected_sample)
        .bind(&import.created_from)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn list(&self, limit: u64) -> Result<Vec<BundleImport>, CoreError> {
        let rows = sqlx::query(
            "SELECT bundle_id, signer_key, imported_at, imported_by, entries, blobs, rejected,
                    rejected_sample, created_from
             FROM bundle_imports ORDER BY imported_at DESC LIMIT $1",
        )
        .bind(if limit == 0 { 100 } else { limit as i64 })
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .iter()
            .map(|r| BundleImport {
                bundle_id: r.get("bundle_id"),
                signer_key: r.get("signer_key"),
                imported_at: r.get("imported_at"),
                imported_by: r.get("imported_by"),
                entries: r.get::<i64, _>("entries").max(0) as u64,
                blobs: r.get::<i64, _>("blobs").max(0) as u64,
                rejected: r.get::<i64, _>("rejected").max(0) as u64,
                rejected_sample: r.get("rejected_sample"),
                created_from: r.get("created_from"),
            })
            .collect())
    }

    async fn seen(&self, bundle_id: &str) -> Result<bool, CoreError> {
        let row = sqlx::query("SELECT 1 AS one FROM bundle_imports WHERE bundle_id = $1")
            .bind(bundle_id)
            .fetch_optional(&self.pool)
            .await
            .db_err()?;
        Ok(row.is_some())
    }
}
