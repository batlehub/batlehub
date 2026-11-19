use crate::db::DbResultExt;
use async_trait::async_trait;
use sqlx::{PgPool, Row};

use batlehub_core::{
    entities::Identity,
    error::CoreError,
    ports::{BetaChannelEntry, BetaChannelPort},
};

pub struct PgBetaChannelStore {
    pool: PgPool,
}

impl PgBetaChannelStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl BetaChannelPort for PgBetaChannelStore {
    async fn is_member(&self, registry: &str, identity: &Identity) -> Result<bool, CoreError> {
        // `<provider>:<id>`, as written by the admin API: the same id from
        // another provider is another person.
        if let Some(ref uid) = identity.user_principal() {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM beta_channel_members \
                  WHERE registry = $1 AND principal_type = 'user' AND principal_id = $2)",
            )
            .bind(registry)
            .bind(uid)
            .fetch_one(&self.pool)
            .await
            .db_err()?;
            if exists {
                return Ok(true);
            }
        }

        if !identity.groups.is_empty() {
            let groups: Vec<&str> = identity.groups.iter().map(String::as_str).collect();
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM beta_channel_members \
                  WHERE registry = $1 AND principal_type = 'group' AND principal_id = ANY($2))",
            )
            .bind(registry)
            .bind(&groups)
            .fetch_one(&self.pool)
            .await
            .db_err()?;
            if exists {
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn add_member(&self, registry: &str, entry: BetaChannelEntry) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO beta_channel_members \
                (registry, principal_type, principal_id, granted_by) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(registry)
        .bind(&entry.principal_type)
        .bind(&entry.principal_id)
        .bind(&entry.granted_by)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db) = e {
                if db.constraint() == Some("uq_beta_channel_member") {
                    return CoreError::Conflict(format!(
                        "{} '{}' is already a beta-channel member of '{registry}'",
                        entry.principal_type, entry.principal_id
                    ));
                }
            }
            CoreError::Database(e.to_string())
        })?;
        Ok(())
    }

    async fn remove_member(
        &self,
        registry: &str,
        principal_type: &str,
        principal_id: &str,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "DELETE FROM beta_channel_members \
             WHERE registry = $1 AND principal_type = $2 AND principal_id = $3",
        )
        .bind(registry)
        .bind(principal_type)
        .bind(principal_id)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn list_members(&self, registry: &str) -> Result<Vec<BetaChannelEntry>, CoreError> {
        let rows = sqlx::query(
            "SELECT principal_type, principal_id, granted_by \
             FROM beta_channel_members \
             WHERE registry = $1 \
             ORDER BY granted_at ASC",
        )
        .bind(registry)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        Ok(rows
            .into_iter()
            .map(|r| BetaChannelEntry {
                principal_type: r.get("principal_type"),
                principal_id: r.get("principal_id"),
                granted_by: r.get("granted_by"),
            })
            .collect())
    }
}
