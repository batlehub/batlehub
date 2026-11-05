use async_trait::async_trait;
use chrono::{Duration, Utc};
use sqlx::Row;

use batlehub_core::{
    error::CoreError,
    ports::{LoginState, LoginStateStore},
};

use crate::db::packages::PgPackageRepository;
use crate::db::DbResultExt;

#[async_trait]
impl LoginStateStore for PgPackageRepository {
    async fn put(&self, state: &str, value: LoginState, ttl_secs: u32) -> Result<(), CoreError> {
        let expires_at = Utc::now() + Duration::seconds(i64::from(ttl_secs));
        sqlx::query(
            r#"
            INSERT INTO oidc_login_states
                (state, provider, code_verifier, nonce, spa_state, expires_at, return_to)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(state)
        .bind(&value.provider)
        .bind(&value.code_verifier)
        .bind(&value.nonce)
        .bind(&value.spa_state)
        .bind(expires_at)
        .bind(&value.return_to)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    /// `DELETE … RETURNING` rather than `SELECT` then `DELETE`: the delete is what
    /// makes the entry one-time, and doing it in one statement means two
    /// callbacks racing on the same `state` cannot both come away with a
    /// `LoginState`.
    ///
    /// `expires_at > NOW()` is in the `WHERE` so an expired row is deleted
    /// without being returned — a prune that has not run yet can never let a
    /// stale login through.
    async fn take(&self, state: &str) -> Result<Option<LoginState>, CoreError> {
        let row = sqlx::query(
            r#"
            DELETE FROM oidc_login_states
            WHERE state = $1 AND expires_at > NOW()
            RETURNING provider, code_verifier, nonce, spa_state, return_to
            "#,
        )
        .bind(state)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        Ok(row.map(|r| LoginState {
            provider: r.get("provider"),
            code_verifier: r.get("code_verifier"),
            nonce: r.get("nonce"),
            spa_state: r.get("spa_state"),
            return_to: r.get("return_to"),
        }))
    }

    async fn prune_expired(&self) -> Result<u64, CoreError> {
        let result = sqlx::query("DELETE FROM oidc_login_states WHERE expires_at <= NOW()")
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(result.rows_affected())
    }
}
