use crate::db::DbResultExt;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use batlehub_core::{
    entities::Role,
    error::CoreError,
    ports::{TokenOwner, UserToken, UserTokenRepository},
};

use crate::db::packages::PgPackageRepository;

fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::Anonymous => "anonymous",
        Role::User => "user",
        Role::Admin => "admin",
    }
}

/// Read a `UserToken` from a row selecting the standard column list.
fn token_from_row(r: &sqlx::postgres::PgRow) -> Result<UserToken, CoreError> {
    Ok(UserToken {
        id: r.get("id"),
        user_id: r.get("user_id"),
        provider: r.get("provider"),
        name: r.get("name"),
        role: r
            .get::<&str, _>("role")
            .parse()
            .map_err(|e| CoreError::Database(format!("invalid role in db: {e}")))?,
        expires_at: r.get("expires_at"),
        created_at: r.get("created_at"),
        revoked_at: r.get("revoked_at"),
        last_used_at: r.get("last_used_at"),
        last_used_ip: r.get("last_used_ip"),
        groups: r.get("groups"),
    })
}

#[async_trait]
impl UserTokenRepository for PgPackageRepository {
    async fn create_token(
        &self,
        id: Uuid,
        owner: &TokenOwner,
        name: &str,
        token_hash: &str,
        role: Role,
        expires_at: DateTime<Utc>,
        groups: &[String],
    ) -> Result<UserToken, CoreError> {
        let row = sqlx::query(
            r#"
            INSERT INTO user_tokens
                (id, user_id, provider, name, token_hash, role, expires_at, created_at, groups)
            VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), $8)
            RETURNING id, user_id, provider, name, role, expires_at, created_at, revoked_at, last_used_at, last_used_ip, groups
            "#,
        )
        .bind(id)
        .bind(&owner.user_id)
        .bind(&owner.provider)
        .bind(name)
        .bind(token_hash)
        .bind(role_to_str(&role))
        .bind(expires_at)
        .bind(groups)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("uq_user_token_name") {
                    return CoreError::Conflict(format!("a token named '{}' already exists", name));
                }
            }
            CoreError::Database(e.to_string())
        })?;

        token_from_row(&row)
    }

    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<UserToken>, CoreError> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, provider, name, role, expires_at, created_at, revoked_at, last_used_at, last_used_ip, groups
            FROM user_tokens
            WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > NOW()
            "#,
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        row.as_ref().map(token_from_row).transpose()
    }

    /// Scoped to `(provider, user_id)`, not `user_id` alone: the same string
    /// from a different provider is a different principal.
    async fn list_for_user(&self, owner: &TokenOwner) -> Result<Vec<UserToken>, CoreError> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_id, provider, name, role, expires_at, created_at, revoked_at, last_used_at, last_used_ip, groups
            FROM user_tokens
            WHERE user_id = $1 AND provider = $2
              AND revoked_at IS NULL AND expires_at > NOW()
            ORDER BY created_at DESC
            "#,
        )
        .bind(&owner.user_id)
        .bind(&owner.provider)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        rows.iter().map(token_from_row).collect()
    }

    /// Throttled in SQL as well as in the caller: `UserTokenAuthProvider` skips
    /// the call entirely for a minute after a hit, and this `WHERE` means two
    /// replicas that both decide to write still produce one update.
    async fn touch_last_used(&self, id: Uuid, source_ip: Option<&str>) -> Result<(), CoreError> {
        sqlx::query(
            r#"
            UPDATE user_tokens
            SET last_used_at = NOW(), last_used_ip = COALESCE($2, last_used_ip)
            WHERE id = $1
              AND (last_used_at IS NULL OR last_used_at < NOW() - INTERVAL '1 minute')
            "#,
        )
        .bind(id)
        .bind(source_ip)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn revoke(&self, id: Uuid, owner: &TokenOwner) -> Result<bool, CoreError> {
        let result = sqlx::query(
            r#"
            UPDATE user_tokens
            SET revoked_at = NOW()
            WHERE id = $1 AND user_id = $2 AND provider = $3 AND revoked_at IS NULL
            RETURNING id
            "#,
        )
        .bind(id)
        .bind(&owner.user_id)
        .bind(&owner.provider)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        Ok(result.is_some())
    }
}
