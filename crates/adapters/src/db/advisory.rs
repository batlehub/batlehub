//! PostgreSQL [`AdvisoryRepository`] (RFC 0002 §6.2, recast by §13):
//! `package_flags` (migration 054), `registry_scan_state` (055), and the
//! exposure report as one statement over `access_events`.
//!
//! The report's join is `(registry, package_name)` plus `version = '*' OR
//! version = package_version`, which `idx_access_events_pkg` (migration
//! 021) already serves; no new index on the access log was needed.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use batlehub_core::{
    entities::{
        ExposurePage, ExposureQuery, ExposureRow, ExposureWhen, FlagEffect, FlagFilter, FlagKind,
        FlagSourceCoverage, PackageFlag, RegistryScanState, Severity,
    },
    error::CoreError,
    ports::AdvisoryRepository,
};

use super::DbResultExt;

pub struct PgAdvisoryRepository {
    pool: PgPool,
}

impl PgAdvisoryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn effect_rank(e: FlagEffect) -> i16 {
    match e {
        FlagEffect::Inform => 0,
        FlagEffect::Warn => 1,
        FlagEffect::Gate => 2,
        FlagEffect::HardBlock => 3,
    }
}

fn row_to_flag(r: &sqlx::postgres::PgRow) -> PackageFlag {
    let kind: String = r.get("kind");
    let effect: String = r.get("effect");
    let severity: Option<String> = r.get("severity");
    PackageFlag {
        id: r.get("id"),
        source: r.get("source"),
        external_id: r.get("external_id"),
        registry: r.get("registry"),
        package_name: r.get("package_name"),
        version: r.get("version"),
        kind: FlagKind::parse(&kind),
        effect: FlagEffect::parse(&effect).unwrap_or(FlagEffect::Inform),
        severity: severity.and_then(|s| Severity::parse(&s)),
        summary: r.get("summary"),
        url: r.get("url"),
        first_seen: r.get("first_seen"),
        updated_at: r.get("updated_at"),
        expires_at: r.get("expires_at"),
        revoked_at: r.get("revoked_at"),
    }
}

// The fifteen columns every flag read selects. sqlx 0.9 refuses non-literal
// SQL, so the list is repeated per statement rather than shared.

#[async_trait]
impl AdvisoryRepository for PgAdvisoryRepository {
    async fn upsert_flag(&self, flag: PackageFlag) -> Result<(Uuid, bool), CoreError> {
        // `xmax = 0` is true for a row this statement inserted and false for
        // one it updated: the one-statement way to learn which happened.
        let row = sqlx::query(
            r#"
            INSERT INTO package_flags
                (id, source, external_id, registry, package_name, version, kind, effect,
                 severity, summary, url, first_seen, updated_at, expires_at, revoked_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NULL)
            ON CONFLICT (source, external_id) DO UPDATE
                SET registry     = EXCLUDED.registry,
                    package_name = EXCLUDED.package_name,
                    version      = EXCLUDED.version,
                    kind         = EXCLUDED.kind,
                    effect       = EXCLUDED.effect,
                    severity     = EXCLUDED.severity,
                    summary      = EXCLUDED.summary,
                    url          = EXCLUDED.url,
                    updated_at   = EXCLUDED.updated_at,
                    expires_at   = EXCLUDED.expires_at,
                    revoked_at   = NULL
            RETURNING id, (xmax = 0) AS created
            "#,
        )
        .bind(flag.id)
        .bind(&flag.source)
        .bind(&flag.external_id)
        .bind(&flag.registry)
        .bind(&flag.package_name)
        .bind(&flag.version)
        .bind(flag.kind.as_str())
        .bind(flag.effect.as_str())
        .bind(flag.severity.map(|s| s.as_str()))
        .bind(&flag.summary)
        .bind(&flag.url)
        .bind(flag.first_seen)
        .bind(flag.updated_at)
        .bind(flag.expires_at)
        .fetch_one(&self.pool)
        .await
        .db_err()?;
        Ok((row.get("id"), row.get("created")))
    }

    async fn revoke_flag(
        &self,
        source: &str,
        external_id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        let res = sqlx::query(
            "UPDATE package_flags SET revoked_at = $3, updated_at = $3
             WHERE source = $1 AND external_id = $2 AND revoked_at IS NULL",
        )
        .bind(source)
        .bind(external_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(res.rows_affected() > 0)
    }

    async fn get_flag(
        &self,
        source: &str,
        external_id: &str,
    ) -> Result<Option<PackageFlag>, CoreError> {
        let row = sqlx::query(
            "SELECT id, source, external_id, registry, package_name, version, kind, effect, severity, summary, url, first_seen, updated_at, expires_at, revoked_at
             FROM package_flags WHERE source = $1 AND external_id = $2",
        )
        .bind(source)
        .bind(external_id)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        Ok(row.as_ref().map(row_to_flag))
    }

    async fn list_flags(&self, filter: &FlagFilter) -> Result<Vec<PackageFlag>, CoreError> {
        let limit = if filter.limit == 0 {
            i64::MAX
        } else {
            filter.limit as i64
        };
        let rows = sqlx::query(
            r#"
            SELECT id, source, external_id, registry, package_name, version, kind, effect, severity, summary, url, first_seen, updated_at, expires_at, revoked_at
            FROM package_flags
            WHERE ($1::text IS NULL OR registry = $1)
              AND ($2::text IS NULL OR package_name = $2)
              AND ($3::text IS NULL OR source = $3)
              AND ($4::text IS NULL OR effect = $4)
              AND ($5::boolean
                   OR (revoked_at IS NULL AND (expires_at IS NULL OR expires_at > NOW())))
            ORDER BY updated_at DESC, id
            LIMIT $6 OFFSET $7
            "#,
        )
        .bind(&filter.registry)
        .bind(&filter.package_name)
        .bind(&filter.source)
        .bind(filter.effect.map(|e| e.as_str()))
        .bind(filter.include_dead)
        .bind(limit)
        .bind(filter.offset as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows.iter().map(row_to_flag).collect())
    }

    async fn count_flags(&self, filter: &FlagFilter) -> Result<u64, CoreError> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) AS n FROM package_flags
            WHERE ($1::text IS NULL OR registry = $1)
              AND ($2::text IS NULL OR package_name = $2)
              AND ($3::text IS NULL OR source = $3)
              AND ($4::text IS NULL OR effect = $4)
              AND ($5::boolean
                   OR (revoked_at IS NULL AND (expires_at IS NULL OR expires_at > NOW())))
            "#,
        )
        .bind(&filter.registry)
        .bind(&filter.package_name)
        .bind(&filter.source)
        .bind(filter.effect.map(|e| e.as_str()))
        .bind(filter.include_dead)
        .fetch_one(&self.pool)
        .await
        .db_err()?;
        Ok(row.get::<i64, _>("n").max(0) as u64)
    }

    async fn live_flags_for_package(
        &self,
        registry: &str,
        name: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<PackageFlag>, CoreError> {
        let rows = sqlx::query(
            "SELECT id, source, external_id, registry, package_name, version, kind, effect, severity, summary, url, first_seen, updated_at, expires_at, revoked_at
             FROM package_flags
             WHERE registry = $1 AND package_name = $2 AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > $3)",
        )
        .bind(registry)
        .bind(name)
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows.iter().map(row_to_flag).collect())
    }

    async fn list_exposure(&self, query: &ExposureQuery) -> Result<ExposurePage, CoreError> {
        let limit = if query.limit == 0 {
            10_000i64
        } else {
            query.limit as i64
        };
        let when = match query.when {
            ExposureWhen::Any => "any",
            ExposureWhen::BeforeFlag => "before",
            ExposureWhen::AfterFlag => "after",
        };
        let (c_last, c_consumer, c_registry, c_name, c_version, c_flag) = match &query.after {
            Some(c) => (
                Some(c.last_pull),
                Some(c.consumer.clone()),
                Some(c.registry.clone()),
                Some(c.package_name.clone()),
                Some(c.version.clone()),
                Some(c.flag_id),
            ),
            None => (None, None, None, None, None, None),
        };
        // One row per consumer × coordinate × flag, newest pull first; the
        // cursor is the last row of the previous page, compared as a tuple
        // on the same order (`last_pull` descending, the key ascending).
        let rows = sqlx::query(
            r#"
            SELECT
                COALESCE(e.user_id, 'anonymous')            AS consumer,
                MIN(e.user_role)                            AS consumer_role,
                e.registry, e.package_name, e.package_version AS version,
                f.id AS flag_id, f.source, f.external_id, f.kind, f.effect, f.severity,
                f.summary, f.first_seen,
                COUNT(*)::bigint                            AS pulls,
                COUNT(*) FILTER (WHERE e.created_at < f.first_seen)::bigint AS pulls_before,
                MIN(e.created_at)                           AS first_pull,
                MAX(e.created_at)                           AS last_pull
            FROM access_events e
            JOIN package_flags f
              ON f.registry = e.registry
             AND f.package_name = e.package_name
             AND (f.version = '*' OR f.version = e.package_version)
            WHERE e.action = 'download' AND e.outcome = 'allowed'
              AND ($1::timestamptz IS NULL OR e.created_at >= $1)
              AND ($2::timestamptz IS NULL OR e.created_at <= $2)
              AND ($3::text IS NULL OR e.registry = $3)
              AND ($4::text IS NULL OR e.package_name = $4)
              AND ($5::text IS NULL OR f.source = $5)
              AND ($6::smallint IS NULL OR
                   (CASE f.effect WHEN 'inform' THEN 0 WHEN 'warn' THEN 1
                                  WHEN 'gate' THEN 2 ELSE 3 END) >= $6)
            GROUP BY COALESCE(e.user_id, 'anonymous'), e.registry, e.package_name,
                     e.package_version, f.id
            HAVING ($7::text = 'any'
                    OR ($7 = 'before' AND COUNT(*) FILTER (WHERE e.created_at < f.first_seen) > 0)
                    OR ($7 = 'after'  AND COUNT(*) FILTER (WHERE e.created_at >= f.first_seen) > 0))
               AND ($8::timestamptz IS NULL
                    OR MAX(e.created_at) < $8
                    OR (MAX(e.created_at) = $8
                        AND (COALESCE(e.user_id, 'anonymous'), e.registry, e.package_name,
                             e.package_version, f.id::text)
                            > ($9, $10, $11, $12, $13::text)))
            ORDER BY last_pull DESC, consumer, e.registry, e.package_name, e.package_version, f.id
            LIMIT $14
            "#,
        )
        .bind(query.from)
        .bind(query.to)
        .bind(&query.registry)
        .bind(&query.package_name)
        .bind(&query.source)
        .bind(query.min_effect.map(effect_rank))
        .bind(when)
        .bind(c_last)
        .bind(c_consumer)
        .bind(c_registry)
        .bind(c_name)
        .bind(c_version)
        .bind(c_flag.map(|u| u.to_string()))
        .bind(limit + 1)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        let mut out: Vec<ExposureRow> = rows
            .iter()
            .map(|r| {
                let kind: String = r.get("kind");
                let effect: String = r.get("effect");
                let severity: Option<String> = r.get("severity");
                ExposureRow {
                    consumer: r.get("consumer"),
                    consumer_role: r.get("consumer_role"),
                    registry: r.get("registry"),
                    package_name: r.get("package_name"),
                    version: r.get("version"),
                    flag_id: r.get("flag_id"),
                    source: r.get("source"),
                    external_id: r.get("external_id"),
                    kind: FlagKind::parse(&kind),
                    effect: FlagEffect::parse(&effect).unwrap_or(FlagEffect::Inform),
                    severity: severity.and_then(|s| Severity::parse(&s)),
                    summary: r.get("summary"),
                    flag_first_seen: r.get("first_seen"),
                    pulls: r.get::<i64, _>("pulls").max(0) as u64,
                    pulls_before_flag: r.get::<i64, _>("pulls_before").max(0) as u64,
                    first_pull: r.get("first_pull"),
                    last_pull: r.get("last_pull"),
                }
            })
            .collect();
        let next = (out.len() as i64 > limit).then(|| out[limit as usize - 1].cursor().encode());
        out.truncate(limit as usize);
        Ok(ExposurePage { rows: out, next })
    }

    async fn source_coverage(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<FlagSourceCoverage>, CoreError> {
        let rows = sqlx::query(
            r#"
            SELECT source,
                   COUNT(*) FILTER (WHERE revoked_at IS NULL
                                      AND (expires_at IS NULL OR expires_at > $1))::bigint AS live,
                   MAX(updated_at) AS last_push_at
            FROM package_flags
            GROUP BY source
            ORDER BY source
            "#,
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .iter()
            .map(|r| FlagSourceCoverage {
                source: r.get("source"),
                live_flags: r.get::<i64, _>("live").max(0) as u64,
                last_push_at: r.get("last_push_at"),
            })
            .collect())
    }

    async fn record_scan_state(&self, state: &RegistryScanState) -> Result<(), CoreError> {
        sqlx::query(
            r#"
            INSERT INTO registry_scan_state
                (registry, last_scan_at, artifacts_scanned, findings, errors)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (registry) DO UPDATE
                SET last_scan_at      = EXCLUDED.last_scan_at,
                    artifacts_scanned = EXCLUDED.artifacts_scanned,
                    findings          = EXCLUDED.findings,
                    errors            = EXCLUDED.errors
            "#,
        )
        .bind(&state.registry)
        .bind(state.last_scan_at)
        .bind(state.artifacts_scanned as i64)
        .bind(state.findings as i64)
        .bind(state.errors as i64)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn list_scan_state(&self) -> Result<Vec<RegistryScanState>, CoreError> {
        let rows = sqlx::query(
            "SELECT registry, last_scan_at, artifacts_scanned, findings, errors
             FROM registry_scan_state ORDER BY registry",
        )
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows
            .iter()
            .map(|r| RegistryScanState {
                registry: r.get("registry"),
                last_scan_at: r.get("last_scan_at"),
                artifacts_scanned: r.get::<i64, _>("artifacts_scanned").max(0) as u64,
                findings: r.get::<i64, _>("findings").max(0) as u64,
                errors: r.get::<i64, _>("errors").max(0) as u64,
            })
            .collect())
    }
}
