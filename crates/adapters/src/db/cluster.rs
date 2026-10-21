//! [`ClusterStatePort`] on the `system_kv` table every replica already shares.
//!
//! Two kinds of key:
//! - `{topic}` → `{"gen": n}`, advanced by [`ClusterStatePort::bump`];
//! - `config:replica:{name}` → `{"fingerprint"}`, rewritten by each replica on a
//!   timer, its `updated_at` saying when.
//!
//! Postgres rather than the `[cache]` store: it is present in every
//! deployment, and a `memory` cache is not shared at all. No migration — the
//! table has held the banner since 019.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use batlehub_core::{
    error::CoreError,
    ports::{ClusterStatePort, ReplicaConfig},
};

use crate::db::DbResultExt;

const REPLICA_PREFIX: &str = "config:replica:";

pub struct PgClusterState {
    pool: PgPool,
}

impl PgClusterState {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ClusterStatePort for PgClusterState {
    async fn bump(&self, topic: &str) -> Result<(), CoreError> {
        sqlx::query(
            r#"
            INSERT INTO system_kv (key, value, updated_at)
            VALUES ($1, '{"gen": 1}'::jsonb, NOW())
            ON CONFLICT (key) DO UPDATE SET
                value = jsonb_build_object(
                    'gen', COALESCE((system_kv.value->>'gen')::bigint, 0) + 1),
                updated_at = NOW()
            "#,
        )
        .bind(topic)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn heartbeat(
        &self,
        replica: &str,
        fingerprint: &str,
        prefix: &str,
    ) -> Result<Vec<(String, i64)>, CoreError> {
        // The report, the pruning of rows left by pods a rollout replaced (a
        // day is far past any window a reader asks about) and the counters, in
        // one statement. The final `SELECT` reads the snapshot from before the
        // writes, which is all it needs: it does not read replica rows.
        //
        // `starts_with`, not `LIKE` (a `_` in a prefix is not a pattern) and not
        // a key range (under a non-C collation `:` may not sort where a range
        // assumes): `system_kv` holds a handful of rows, and a sequential scan
        // of it is the plan either way.
        let rows = sqlx::query(
            r#"
            WITH report AS (
                INSERT INTO system_kv (key, value, updated_at)
                VALUES ($1, jsonb_build_object('fingerprint', $2::text), NOW())
                ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()
                RETURNING 1
            ), pruned AS (
                DELETE FROM system_kv
                WHERE starts_with(key, $3) AND updated_at < NOW() - INTERVAL '1 day'
                RETURNING 1
            )
            SELECT key, COALESCE((value->>'gen')::bigint, 0) AS gen
            FROM system_kv WHERE starts_with(key, $4)
            "#,
        )
        .bind(format!("{REPLICA_PREFIX}{replica}"))
        .bind(fingerprint)
        .bind(REPLICA_PREFIX)
        .bind(prefix)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("key"), r.get("gen")))
            .collect())
    }

    async fn config_reports(&self, since: DateTime<Utc>) -> Result<Vec<ReplicaConfig>, CoreError> {
        let rows = sqlx::query(
            "SELECT key, value->>'fingerprint' AS fingerprint, updated_at FROM system_kv \
             WHERE starts_with(key, $1) AND updated_at >= $2 ORDER BY key",
        )
        .bind(REPLICA_PREFIX)
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let key: String = r.get("key");
                ReplicaConfig {
                    replica: key[REPLICA_PREFIX.len()..].to_owned(),
                    fingerprint: r
                        .get::<Option<String>, _>("fingerprint")
                        .unwrap_or_default(),
                    reported_at: r.get("updated_at"),
                }
            })
            .collect())
    }
}
