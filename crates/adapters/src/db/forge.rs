//! PostgreSQL stores for the forge ports (RFC 0019 §5.2): remembered ref
//! resolutions and the shared rate-limit budget.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Row};

use batlehub_core::entities::RefKind;
use batlehub_core::error::CoreError;
use batlehub_core::ports::{
    budget_allows, BudgetRole, RateLimitBudget, RateLimitObservation, RefResolutionRepository,
    StoredRefResolution,
};

use super::DbResultExt;

/// `ref_resolutions` (migration 047).
pub struct PgRefResolutionRepository {
    pool: PgPool,
}

impl PgRefResolutionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RefResolutionRepository for PgRefResolutionRepository {
    async fn get(
        &self,
        registry: &str,
        owner_repo: &str,
        git_ref: &str,
    ) -> Result<Option<StoredRefResolution>, CoreError> {
        let row = sqlx::query(
            "SELECT ref_kind, sha, resolved_at, previous_sha
             FROM ref_resolutions
             WHERE registry = $1 AND owner_repo = $2 AND git_ref = $3",
        )
        .bind(registry)
        .bind(owner_repo)
        .bind(git_ref)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let kind: String = row.get("ref_kind");
        let kind: RefKind = kind
            .parse()
            .map_err(|e: String| CoreError::Other(anyhow::anyhow!("ref_resolutions: {e}")))?;
        Ok(Some(StoredRefResolution {
            kind,
            sha: row.get("sha"),
            resolved_at: row.get("resolved_at"),
            previous: row.try_get("previous_sha").ok(),
        }))
    }

    async fn list_for_repo(
        &self,
        registry: &str,
        owner_repo: &str,
    ) -> Result<Vec<(String, StoredRefResolution)>, CoreError> {
        let rows = sqlx::query(
            "SELECT git_ref, ref_kind, sha, resolved_at, previous_sha
             FROM ref_resolutions
             WHERE registry = $1 AND owner_repo = $2
             ORDER BY resolved_at DESC, git_ref
             LIMIT 200",
        )
        .bind(registry)
        .bind(owner_repo)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        rows.into_iter()
            .map(|row| {
                let kind: String = row.get("ref_kind");
                let kind: RefKind = kind.parse().map_err(|e: String| {
                    CoreError::Other(anyhow::anyhow!("ref_resolutions: {e}"))
                })?;
                Ok((
                    row.get::<String, _>("git_ref"),
                    StoredRefResolution {
                        kind,
                        sha: row.get("sha"),
                        resolved_at: row.get("resolved_at"),
                        previous: row.try_get("previous_sha").ok(),
                    },
                ))
            })
            .collect()
    }

    async fn upsert(
        &self,
        registry: &str,
        owner_repo: &str,
        git_ref: &str,
        resolution: &StoredRefResolution,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO ref_resolutions
                 (registry, owner_repo, git_ref, ref_kind, sha, resolved_at, previous_sha)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (registry, owner_repo, git_ref) DO UPDATE SET
                 ref_kind = EXCLUDED.ref_kind,
                 sha = EXCLUDED.sha,
                 resolved_at = EXCLUDED.resolved_at,
                 previous_sha = EXCLUDED.previous_sha",
        )
        .bind(registry)
        .bind(owner_repo)
        .bind(git_ref)
        .bind(resolution.kind.as_str())
        .bind(&resolution.sha)
        .bind(resolution.resolved_at)
        .bind(&resolution.previous)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }
}

/// `rate_limit_budget` (migration 048).
pub struct PgRateLimitBudget {
    pool: PgPool,
}

impl PgRateLimitBudget {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RateLimitBudget for PgRateLimitBudget {
    async fn acquire(
        &self,
        registry: &str,
        token_fingerprint: &str,
        role: BudgetRole,
    ) -> Result<bool, CoreError> {
        let row = sqlx::query(
            "SELECT remaining, rate_limit, reset_at
             FROM rate_limit_budget
             WHERE registry = $1 AND token_fingerprint = $2",
        )
        .bind(registry)
        .bind(token_fingerprint)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;
        let observation = row.map(|r| RateLimitObservation {
            remaining: r.get("remaining"),
            limit: r.get("rate_limit"),
            reset_at: r.get("reset_at"),
        });
        Ok(budget_allows(observation, role, Utc::now()))
    }

    async fn observe(
        &self,
        registry: &str,
        token_fingerprint: &str,
        observation: RateLimitObservation,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO rate_limit_budget
                 (registry, token_fingerprint, remaining, rate_limit, reset_at, observed_at)
             VALUES ($1, $2, $3, $4, $5, NOW())
             ON CONFLICT (registry, token_fingerprint) DO UPDATE SET
                 remaining = EXCLUDED.remaining,
                 rate_limit = EXCLUDED.rate_limit,
                 reset_at = EXCLUDED.reset_at,
                 observed_at = NOW()",
        )
        .bind(registry)
        .bind(token_fingerprint)
        .bind(observation.remaining)
        .bind(observation.limit)
        .bind(observation.reset_at)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }
}
