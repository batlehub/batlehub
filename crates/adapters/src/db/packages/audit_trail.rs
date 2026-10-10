//! Postgres [`AuditTrailStore`] (RFC 0036 §6.3–6.4) and the advisory-lock
//! [`LeaderLock`] its jobs run under.
//!
//! The chain's integrity rests on one rule, enforced in [`apply`]: a batch
//! takes `audit_seals` in `SHARE ROW EXCLUSIVE` mode (which conflicts with
//! itself, so writers queue), checks the head it was built on, applies its row
//! changes, and — when it carries a record — recomputes the window's digest
//! *inside the transaction* before appending. A record can therefore never
//! describe rows other than the ones committed beside it.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use tokio::sync::Mutex;
use uuid::Uuid;

use batlehub_core::entities::audit_seal::rows_digest;
use batlehub_core::entities::{AccessEvent, SealKind, SealRecord};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{
    AuditTrailStore, CandidateQuery, ErasedElsewhere, LeaderLock, RowOp, TrailBatch,
};

use super::explore::map_access_event;
use super::PgPackageRepository;
use crate::db::DbResultExt;

/// The access class, spelled as `access_events.action` stores it.
const ACCESS_ACTIONS: [&str; 2] = ["download", "view_metadata"];

fn record_from_row(r: &sqlx::postgres::PgRow) -> Result<SealRecord, CoreError> {
    let kind: String = r.get("kind");
    Ok(SealRecord {
        seq: r.get("seq"),
        kind: SealKind::parse(&kind)
            .ok_or_else(|| CoreError::Database(format!("invalid seal kind in db: '{kind}'")))?,
        window_start: r.get("window_start"),
        window_end: r.get("window_end"),
        row_count: r.get::<i64, _>("row_count").max(0) as u64,
        affected: r.get::<i64, _>("affected").max(0) as u64,
        rows_digest: r.get("rows_digest"),
        digest: r.get("digest"),
        prev_digest: r.get("prev_digest"),
        signature: r.get("signature"),
        key_id: r.get("key_id"),
    })
}

async fn rows_in<'e>(
    exec: impl sqlx::PgExecutor<'e>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<AccessEvent>, CoreError> {
    let rows = sqlx::query(
        r#"
        SELECT
            id, user_id, user_role, registry, package_name, package_version,
            package_artifact, action, outcome, deny_reason, created_at,
            ip_address, user_agent, throttled_count, detail
        FROM access_events
        WHERE created_at >= $1 AND created_at < $2
        ORDER BY created_at, id
        "#,
    )
    .bind(start)
    .bind(end)
    .fetch_all(exec)
    .await
    .db_err()?;
    rows.iter().map(map_access_event).collect()
}

#[async_trait]
impl AuditTrailStore for PgPackageRepository {
    async fn seal_records(&self) -> Result<Vec<SealRecord>, CoreError> {
        let rows = sqlx::query(
            "SELECT seq, kind, window_start, window_end, row_count, affected, rows_digest,
                    digest, prev_digest, signature, key_id
             FROM audit_seals ORDER BY seq",
        )
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter().map(record_from_row).collect()
    }

    async fn rows_in(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        rows_in(&self.pool, start, end).await
    }

    async fn candidates(
        &self,
        query: &CandidateQuery,
        created_before: Option<DateTime<Utc>>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        let access: Vec<String> = ACCESS_ACTIONS.iter().map(ToString::to_string).collect();
        // One statement for the three shapes: each arm is guarded by the
        // parameter that selects it, so the planner keeps one plan per arm.
        let (kind, before, access_before, security_before, user) = match query {
            CandidateQuery::Pseudonymise { before } => (1, Some(*before), None, None, None),
            CandidateQuery::Expire {
                access_before,
                security_before,
            } => (2, None, *access_before, *security_before, None),
            CandidateQuery::Subject { user_id } => (3, None, None, None, Some(user_id.clone())),
        };
        let rows = sqlx::query(
            r#"
            SELECT
                id, user_id, user_role, registry, package_name, package_version,
                package_artifact, action, outcome, deny_reason, created_at,
                ip_address, user_agent, throttled_count, detail
            FROM access_events
            WHERE ($1::timestamptz IS NULL OR created_at < $1)
              AND CASE $2::int
                WHEN 1 THEN action = ANY($3) AND created_at < $4
                    AND (user_agent IS NOT NULL
                         OR (ip_address IS NOT NULL AND position('/' IN ip_address) = 0))
                WHEN 2 THEN (action = ANY($3) AND created_at < $5)
                         OR (NOT action = ANY($3) AND created_at < $6)
                -- `detail_names_subject` in core, in SQL: the export a row was
                -- about, and the grant an admin wrote for the subject, matched
                -- as a whole token so `alice` never takes `alice2`'s rows.
                ELSE user_id = $7
                    OR (action = 'gdpr_export'
                        AND (detail = 'subject=' || $7
                             OR starts_with(detail, 'subject=' || $7 || ' ')))
                    OR (action IN ('grant_write', 'grant_revoke')
                        AND (detail = 'subject=user:' || $7
                             OR starts_with(detail, 'subject=user:' || $7 || ' ')))
              END
            ORDER BY created_at
            LIMIT $8
            "#,
        )
        .bind(created_before)
        .bind(kind)
        .bind(&access)
        .bind(before)
        .bind(access_before)
        .bind(security_before)
        .bind(user)
        .bind(limit.min(i64::MAX as u64) as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.iter().map(map_access_event).collect()
    }

    async fn apply(&self, batch: TrailBatch) -> Result<(), CoreError> {
        let mut tx = self.pool.begin().await.db_err()?;
        if batch.expect_head.is_some() || batch.record.is_some() {
            sqlx::query("LOCK TABLE audit_seals IN SHARE ROW EXCLUSIVE MODE")
                .execute(&mut *tx)
                .await
                .db_err()?;
            let head: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM audit_seals")
                .fetch_one(&mut *tx)
                .await
                .db_err()?;
            if let Some(want) = batch.expect_head {
                if want != head {
                    return Err(CoreError::Conflict(format!(
                        "the seal chain moved: head is {head}, the batch was built on {want}"
                    )));
                }
            }
        }

        let mut deletes: Vec<Uuid> = Vec::new();
        let (mut ids, mut users, mut ips, mut agents, mut details) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for op in &batch.ops {
            match op {
                RowOp::Delete(id) => deletes.push(*id),
                RowOp::Replace(e) => {
                    ids.push(e.id);
                    users.push(e.user_id.clone());
                    ips.push(e.ip_address.clone());
                    agents.push(e.user_agent.clone());
                    details.push(e.detail.clone());
                }
            }
        }
        if !deletes.is_empty() {
            sqlx::query("DELETE FROM access_events WHERE id = ANY($1)")
                .bind(&deletes)
                .execute(&mut *tx)
                .await
                .db_err()?;
        }
        if !ids.is_empty() {
            sqlx::query(
                r#"
                UPDATE access_events AS a
                SET user_id = u.user_id, ip_address = u.ip, user_agent = u.ua, detail = u.detail
                FROM unnest($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[])
                     AS u(id, user_id, ip, ua, detail)
                WHERE a.id = u.id
                "#,
            )
            .bind(&ids)
            .bind(&users)
            .bind(&ips)
            .bind(&agents)
            .bind(&details)
            .execute(&mut *tx)
            .await
            .db_err()?;
        }

        if let Some(r) = &batch.record {
            let rows = rows_in(&mut *tx, r.window_start, r.window_end).await?;
            if rows_digest(&rows) != r.rows_digest {
                return Err(CoreError::Conflict(
                    "the window's rows changed since the record was built".into(),
                ));
            }
            sqlx::query(
                r#"
                INSERT INTO audit_seals
                    (seq, kind, window_start, window_end, row_count, affected,
                     rows_digest, digest, prev_digest, signature, key_id)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                "#,
            )
            .bind(r.seq)
            .bind(r.kind.as_str())
            .bind(r.window_start)
            .bind(r.window_end)
            .bind(r.row_count as i64)
            .bind(r.affected as i64)
            .bind(&r.rows_digest)
            .bind(&r.digest)
            .bind(&r.prev_digest)
            .bind(&r.signature)
            .bind(&r.key_id)
            .execute(&mut *tx)
            .await
            .db_err()?;
        }
        tx.commit().await.db_err()?;
        Ok(())
    }

    async fn open_decisions(&self, user_id: &str) -> Result<Vec<String>, CoreError> {
        let mut out = Vec::new();
        let blocked: Option<String> =
            sqlx::query_scalar("SELECT blocked_by FROM user_blocks WHERE user_id = $1")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .db_err()?;
        if let Some(by) = blocked {
            out.push(format!("an active account block, placed by {by}"));
        }
        let placed = sqlx::query(
            "SELECT registry, package_name, package_version FROM package_statuses
             WHERE blocked_by = $1 AND status = 'blocked' ORDER BY blocked_at DESC LIMIT 5",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        for r in placed {
            out.push(format!(
                "a block they placed on {}:{}@{}",
                r.get::<String, _>("registry"),
                r.get::<String, _>("package_name"),
                r.get::<String, _>("package_version"),
            ));
        }
        Ok(out)
    }

    async fn erase_elsewhere(
        &self,
        user_id: &str,
        pseudonym: &str,
    ) -> Result<ErasedElsewhere, CoreError> {
        let mut tx = self.pool.begin().await.db_err()?;
        // Renamed *and* revoked: a token that kept working under the pseudonym
        // would be the subject's credential under a name nobody can trace.
        let tokens = sqlx::query(
            "UPDATE user_tokens SET user_id = $2, revoked_at = COALESCE(revoked_at, NOW())
             WHERE user_id = $1",
        )
        .bind(user_id)
        .bind(pseudonym)
        .execute(&mut *tx)
        .await
        .db_err()?
        .rows_affected();
        let mut blocks = 0;
        for sql in [
            "UPDATE user_blocks SET user_id = $2 WHERE user_id = $1",
            "UPDATE user_blocks SET blocked_by = $2 WHERE blocked_by = $1",
            "UPDATE package_statuses SET blocked_by = $2 WHERE blocked_by = $1",
        ] {
            blocks += sqlx::query(sql)
                .bind(user_id)
                .bind(pseudonym)
                .execute(&mut *tx)
                .await
                .db_err()?
                .rows_affected();
        }
        tx.commit().await.db_err()?;
        Ok(ErasedElsewhere { tokens, blocks })
    }

    async fn subject_export(&self, user_id: &str) -> Result<serde_json::Value, CoreError> {
        let events = sqlx::query(
            r#"
            SELECT
                id, user_id, user_role, registry, package_name, package_version,
                package_artifact, action, outcome, deny_reason, created_at,
                ip_address, user_agent, throttled_count, detail
            FROM access_events WHERE user_id = $1 ORDER BY created_at
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .db_err()?
        .iter()
        .map(map_access_event)
        .collect::<Result<Vec<_>, _>>()?;

        // Each section is the rows as stored, as JSON — except the token hash,
        // which is a credential and is never exported.
        let mut out = serde_json::json!({ "user_id": user_id, "access_events": events });
        for (section, sql) in [
            ("tokens", "SELECT COALESCE(jsonb_agg(to_jsonb(t) - 'token_hash'), '[]'::jsonb) FROM user_tokens t WHERE user_id = $1"),
            ("account_blocks", "SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb) FROM user_blocks t WHERE user_id = $1 OR blocked_by = $1"),
            ("package_blocks_placed", "SELECT COALESCE(jsonb_agg(jsonb_build_object('registry', registry, 'package', package_name, 'version', package_version, 'status', status, 'blocked_at', blocked_at)), '[]'::jsonb) FROM package_statuses WHERE blocked_by = $1"),
            ("quota_usage", "SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb) FROM quota_usage t WHERE user_id = $1"),
            // Exempt from erasure (RFC 0036 §11 q5) — listed so the subject
            // sees what is kept, and why.
            ("publications_retained", "SELECT COALESCE(jsonb_agg(jsonb_build_object('registry', registry, 'name', name, 'version', version, 'published_at', published_at)), '[]'::jsonb) FROM local_packages WHERE published_by = $1"),
            ("ownership_retained", "SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb) FROM package_owners t WHERE principal_type = 'user' AND principal_id = $1"),
            ("grants", "SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb) FROM grants t WHERE subject = 'user:' || $1 OR granted_by = $1"),
        ] {
            let value: serde_json::Value = sqlx::query_scalar(sql)
                .bind(user_id)
                .fetch_one(&self.pool)
                .await
                .db_err()?;
            out[section] = value;
        }
        Ok(out)
    }
}

/// A PostgreSQL advisory lock held on a dedicated connection: whoever holds
/// it is the leader until the connection drops.
pub struct PgAdvisoryLeader {
    pool: PgPool,
    key: i64,
    held: Mutex<Option<sqlx::pool::PoolConnection<sqlx::Postgres>>>,
}

impl PgAdvisoryLeader {
    pub fn new(pool: PgPool, key: i64) -> Self {
        Self {
            pool,
            key,
            held: Mutex::new(None),
        }
    }
}

#[async_trait]
impl LeaderLock for PgAdvisoryLeader {
    async fn try_lead(&self) -> Result<bool, CoreError> {
        let mut held = self.held.lock().await;
        if let Some(conn) = held.as_mut() {
            match sqlx::query("SELECT 1").execute(&mut **conn).await {
                Ok(_) => return Ok(true),
                Err(e) => {
                    tracing::warn!(error = %e, key = self.key, "leader connection lost; re-electing");
                    *held = None;
                }
            }
        }
        let mut conn = self.pool.acquire().await.db_err()?;
        let won: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(self.key)
            .fetch_one(&mut *conn)
            .await
            .db_err()?;
        if won {
            *held = Some(conn);
        }
        Ok(won)
    }
}
