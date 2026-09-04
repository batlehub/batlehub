//! `upstream_status` on Postgres (RFC 0014 §6.6).
//!
//! The `''` version sentinel lives here and only here: `Option<String>` is
//! what core and the API see.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use batlehub_core::{
    entities::{
        hold_key, truncate_error, MissObservation, UpstreamKey, UpstreamState, UpstreamStatus,
        UpstreamStatusFilter,
    },
    error::CoreError,
    ports::UpstreamStatusPort,
};

use crate::db::DbResultExt;

pub struct PgUpstreamStatusStore {
    pool: PgPool,
}

impl PgUpstreamStatusStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "registry, package_name, version, state, first_missed_at, last_checked_at, \
                       confirmed_at, consecutive_misses, last_error";

fn sentinel(version: Option<&str>) -> &str {
    version.unwrap_or("")
}

fn row_to_status(r: &sqlx::postgres::PgRow) -> Result<UpstreamStatus, CoreError> {
    let version: String = r.get("version");
    let state: String = r.get("state");
    Ok(UpstreamStatus {
        registry: r.get("registry"),
        package_name: r.get("package_name"),
        version: (!version.is_empty()).then_some(version),
        state: state
            .parse::<UpstreamState>()
            .map_err(|e| CoreError::Database(format!("upstream_status.state: {e}")))?,
        first_missed_at: r.get("first_missed_at"),
        last_checked_at: r.get("last_checked_at"),
        confirmed_at: r.try_get("confirmed_at").ok(),
        consecutive_misses: r.get::<i32, _>("consecutive_misses").max(0) as u32,
        last_error: r.try_get("last_error").ok(),
    })
}

#[async_trait]
impl UpstreamStatusPort for PgUpstreamStatusStore {
    async fn record_miss(&self, obs: MissObservation<'_>) -> Result<UpstreamStatus, CoreError> {
        let error = obs.error.map(truncate_error);
        let row = sqlx::query(
            "INSERT INTO upstream_status
                 (registry, package_name, version, state, first_missed_at, last_checked_at,
                  confirmed_at, consecutive_misses, last_error)
             VALUES ($1, $2, $3, 'missing', $4, $4, NULL, 1, $5)
             ON CONFLICT (registry, package_name, version) DO UPDATE
                 SET consecutive_misses = upstream_status.consecutive_misses + 1,
                     last_checked_at    = EXCLUDED.last_checked_at,
                     last_error         = COALESCE(EXCLUDED.last_error, upstream_status.last_error)
             RETURNING registry, package_name, version, state, first_missed_at, last_checked_at,
                       confirmed_at, consecutive_misses, last_error",
        )
        .bind(obs.key.registry)
        .bind(obs.key.package_name)
        .bind(sentinel(obs.key.version))
        .bind(obs.at)
        .bind(error)
        .fetch_one(&self.pool)
        .await
        .db_err()?;
        row_to_status(&row)
    }

    async fn confirm(&self, key: &UpstreamKey<'_>, at: DateTime<Utc>) -> Result<(), CoreError> {
        let done = sqlx::query(
            "UPDATE upstream_status
                SET state = 'disappeared', confirmed_at = COALESCE(confirmed_at, $4)
              WHERE registry = $1 AND package_name = $2 AND version = $3",
        )
        .bind(key.registry)
        .bind(key.package_name)
        .bind(sentinel(key.version))
        .bind(at)
        .execute(&self.pool)
        .await
        .db_err()?;
        if done.rows_affected() == 0 {
            return Err(CoreError::NotFound(format!(
                "no upstream_status row for {}/{}",
                key.registry, key.package_name
            )));
        }
        Ok(())
    }

    async fn clear(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError> {
        let row = sqlx::query(
            "DELETE FROM upstream_status
              WHERE registry = $1 AND package_name = $2 AND version = $3
              RETURNING registry, package_name, version, state, first_missed_at, last_checked_at,
                        confirmed_at, consecutive_misses, last_error",
        )
        .bind(key.registry)
        .bind(key.package_name)
        .bind(sentinel(key.version))
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        row.as_ref().map(row_to_status).transpose()
    }

    async fn get(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError> {
        let row = sqlx::query(
            "SELECT registry, package_name, version, state, first_missed_at, last_checked_at,
                    confirmed_at, consecutive_misses, last_error
               FROM upstream_status
              WHERE registry = $1 AND package_name = $2 AND version = $3",
        )
        .bind(key.registry)
        .bind(key.package_name)
        .bind(sentinel(key.version))
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        row.as_ref().map(row_to_status).transpose()
    }

    async fn list(&self, filter: UpstreamStatusFilter) -> Result<Vec<UpstreamStatus>, CoreError> {
        let limit = if filter.limit == 0 {
            i64::MAX
        } else {
            filter.limit as i64
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM upstream_status
              WHERE ($1::text IS NULL OR registry = $1)
                AND ($2::text IS NULL OR state = $2)
              ORDER BY first_missed_at, registry, package_name, version
              LIMIT $3 OFFSET $4"
        )))
        .bind(filter.registry)
        .bind(filter.state.map(|s| s.as_str().to_owned()))
        .bind(limit)
        .bind(filter.offset as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter().map(row_to_status).collect()
    }

    async fn count(&self, filter: UpstreamStatusFilter) -> Result<u64, CoreError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM upstream_status
              WHERE ($1::text IS NULL OR registry = $1)
                AND ($2::text IS NULL OR state = $2)",
        )
        .bind(filter.registry)
        .bind(filter.state.map(|s| s.as_str().to_owned()))
        .fetch_one(&self.pool)
        .await
        .db_err()?;
        Ok(n.max(0) as u64)
    }

    async fn disappeared_keys(&self, registry: &str) -> Result<HashSet<String>, CoreError> {
        let rows = sqlx::query(
            "SELECT package_name, version FROM upstream_status
              WHERE registry = $1 AND state = 'disappeared'",
        )
        .bind(registry)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .iter()
            .map(|r| {
                let name: String = r.get("package_name");
                let version: String = r.get("version");
                hold_key(&name, (!version.is_empty()).then_some(version.as_str()))
            })
            .collect())
    }
}
