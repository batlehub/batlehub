use crate::db::DbResultExt;
use async_trait::async_trait;
use sqlx::{PgPool, Row};

use batlehub_core::{
    entities::Identity,
    error::CoreError,
    ports::{OwnerEntry, OwnershipPort},
};

pub struct PgOwnershipStore {
    pool: PgPool,
}

impl PgOwnershipStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OwnershipPort for PgOwnershipStore {
    async fn initialize_owner(
        &self,
        registry: &str,
        package: &str,
        user_id: &str,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO package_owners \
                (registry, package_name, principal_type, principal_id, role) \
             VALUES ($1, $2, 'user', $3, 'admin') \
             ON CONFLICT (registry, package_name, principal_type, principal_id) DO NOTHING",
        )
        .bind(registry)
        .bind(package)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn can_publish(
        &self,
        registry: &str,
        package: &str,
        identity: &Identity,
    ) -> Result<bool, CoreError> {
        // One statement for the three questions: no owners yet (then any
        // authenticated user may publish), the caller owns it, or one of the
        // caller's groups does. A `NULL` user id and an empty group list each
        // match nothing, as the separate checks skipped them.
        let groups: Vec<&str> = identity.groups.iter().map(String::as_str).collect();
        sqlx::query_scalar(
            "SELECT NOT EXISTS(SELECT 1 FROM package_owners \
                                WHERE registry = $1 AND package_name = $2) \
                 OR EXISTS(SELECT 1 FROM package_owners \
                            WHERE registry = $1 AND package_name = $2 \
                              AND principal_type = 'user' AND principal_id = $3) \
                 OR EXISTS(SELECT 1 FROM package_owners \
                            WHERE registry = $1 AND package_name = $2 \
                              AND principal_type = 'group' AND principal_id = ANY($4))",
        )
        .bind(registry)
        .bind(package)
        .bind(identity.user_id.as_deref())
        .bind(&groups)
        .fetch_one(&self.pool)
        .await
        .db_err()
    }

    async fn add_owner(
        &self,
        registry: &str,
        package: &str,
        entry: OwnerEntry,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "INSERT INTO package_owners \
                (registry, package_name, principal_type, principal_id, role, granted_by) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(registry)
        .bind(package)
        .bind(&entry.principal_type)
        .bind(&entry.principal_id)
        .bind(&entry.role)
        .bind(&entry.granted_by)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db) = e {
                if db.constraint() == Some("uq_package_owner") {
                    return CoreError::Conflict(format!(
                        "{} '{}' is already an owner of '{}/{}'",
                        entry.principal_type, entry.principal_id, registry, package
                    ));
                }
            }
            CoreError::Database(e.to_string())
        })?;
        Ok(())
    }

    async fn remove_owner(
        &self,
        registry: &str,
        package: &str,
        principal_type: &str,
        principal_id: &str,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "DELETE FROM package_owners \
             WHERE registry = $1 AND package_name = $2 \
               AND principal_type = $3 AND principal_id = $4",
        )
        .bind(registry)
        .bind(package)
        .bind(principal_type)
        .bind(principal_id)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn list_owners(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<OwnerEntry>, CoreError> {
        let rows = sqlx::query(
            "SELECT principal_type, principal_id, role, granted_by \
             FROM package_owners \
             WHERE registry = $1 AND package_name = $2 \
             ORDER BY granted_at ASC",
        )
        .bind(registry)
        .bind(package)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        Ok(rows
            .into_iter()
            .map(|r| OwnerEntry {
                principal_type: r.get("principal_type"),
                principal_id: r.get("principal_id"),
                role: r.get("role"),
                granted_by: r.get("granted_by"),
            })
            .collect())
    }

    /// One statement rather than the port's read-then-delete-each default: the
    /// caller is releasing a name, and a loop that fails partway leaves some of
    /// the previous owner's authority standing over a package someone else may
    /// already have taken.
    async fn remove_all_owners(&self, registry: &str, package: &str) -> Result<(), CoreError> {
        sqlx::query("DELETE FROM package_owners WHERE registry = $1 AND package_name = $2")
            .bind(registry)
            .bind(package)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(())
    }

    async fn list_owned_by(&self, identity: &Identity) -> Result<Vec<(String, String)>, CoreError> {
        // An anonymous caller with no groups owns nothing, and the query below
        // would otherwise compare `principal_id` against NULL for every row.
        if identity.user_id.is_none() && identity.groups.is_empty() {
            return Ok(vec![]);
        }

        let rows = sqlx::query(
            "SELECT DISTINCT registry, package_name \
             FROM package_owners \
             WHERE (principal_type = 'user'  AND principal_id = $1) \
                OR (principal_type = 'group' AND principal_id = ANY($2::text[])) \
             ORDER BY registry, package_name",
        )
        .bind(&identity.user_id)
        .bind(&identity.groups)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        Ok(rows
            .into_iter()
            .map(|r| (r.get("registry"), r.get("package_name")))
            .collect())
    }
}
