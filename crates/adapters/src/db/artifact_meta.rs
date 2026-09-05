use crate::db::DbResultExt;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use batlehub_core::{
    error::CoreError,
    ports::{ArtifactCacheMeta, ArtifactInventory, ArtifactMeta, ArtifactMetaRecord},
};

pub struct PgArtifactMetaRepository {
    pool: PgPool,
}

impl PgArtifactMetaRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ArtifactCacheMeta for PgArtifactMetaRepository {
    async fn record_artifact(&self, rec: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        sqlx::query(
            r#"
            INSERT INTO artifact_cache_meta
                (artifact_key, registry, package_name, version, size_bytes, checksum, cached_at, last_accessed_at)
            VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW())
            ON CONFLICT (artifact_key) DO UPDATE
                SET size_bytes       = EXCLUDED.size_bytes,
                    checksum         = EXCLUDED.checksum,
                    cached_at        = EXCLUDED.cached_at,
                    last_accessed_at = NOW()
            "#,
        )
        .bind(rec.key)
        .bind(rec.registry)
        .bind(rec.package_name)
        .bind(rec.version)
        .bind(rec.size.map(|s| s as i64))
        .bind(rec.checksum)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn get_artifact_checksum(&self, key: &str) -> Result<Option<String>, CoreError> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT checksum FROM artifact_cache_meta WHERE artifact_key = $1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await
                .db_err()?;
        Ok(row.and_then(|(checksum,)| checksum))
    }

    async fn touch_artifact(&self, key: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE artifact_cache_meta SET last_accessed_at = NOW() WHERE artifact_key = $1",
        )
        .bind(key)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn is_artifact_expired(
        &self,
        key: &str,
        older_than: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        // Returns true when: (a) the artifact IS expired, or (b) no metadata row exists.
        // Case (b) covers artifacts written before the artifact_cache_meta migration was applied;
        // treating them as expired forces a re-fetch rather than serving them stale forever.
        let fresh: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM artifact_cache_meta WHERE artifact_key = $1 AND cached_at >= $2)",
        )
        .bind(key)
        .bind(older_than)
        .fetch_one(&self.pool)
        .await
        .db_err()?;
        Ok(!fresh)
    }

    async fn delete_artifact_meta(&self, key: &str) -> Result<(), CoreError> {
        sqlx::query("DELETE FROM artifact_cache_meta WHERE artifact_key = $1")
            .bind(key)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(())
    }

    async fn list_registry_artifacts(
        &self,
        registry: &str,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        ArtifactInventory::list_artifacts(self, registry).await
    }

    async fn list_package_artifacts(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = sqlx::query(
            "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta WHERE registry = $1 AND package_name = $2 ORDER BY cached_at DESC",
        )
        .bind(registry)
        .bind(package)
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows.into_iter().map(row_to_meta).collect())
    }
}

#[async_trait]
impl ArtifactInventory for PgArtifactMetaRepository {
    async fn list_artifacts(&self, registry: &str) -> Result<Vec<ArtifactMeta>, CoreError> {
        let query = if registry.is_empty() {
            sqlx::query("SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta ORDER BY cached_at DESC")
                .fetch_all(&self.pool)
        } else {
            sqlx::query("SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta WHERE registry = $1 ORDER BY cached_at DESC")
                .bind(registry)
                .fetch_all(&self.pool)
        };
        let rows = query.await.db_err()?;
        Ok(rows.into_iter().map(row_to_meta).collect())
    }

    async fn list_artifacts_by_package(&self) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = sqlx::query(
            "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta ORDER BY registry, package_name, cached_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .db_err()?;
        Ok(rows.into_iter().map(row_to_meta).collect())
    }

    async fn list_expired_by_ttl(
        &self,
        registry: &str,
        older_than: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = if registry.is_empty() {
            sqlx::query(
                "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta WHERE cached_at < $1",
            )
            .bind(older_than)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta WHERE registry = $1 AND cached_at < $2",
            )
            .bind(registry)
            .bind(older_than)
            .fetch_all(&self.pool)
            .await
        }
        .db_err()?;
        Ok(rows.into_iter().map(row_to_meta).collect())
    }

    async fn list_idle(
        &self,
        registry: &str,
        idle_since: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = if registry.is_empty() {
            sqlx::query(
                "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta WHERE last_accessed_at < $1",
            )
            .bind(idle_since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta WHERE registry = $1 AND last_accessed_at < $2",
            )
            .bind(registry)
            .bind(idle_since)
            .fetch_all(&self.pool)
            .await
        }
        .db_err()?;
        Ok(rows.into_iter().map(row_to_meta).collect())
    }

    async fn total_size_bytes(&self, registry: &str) -> Result<u64, CoreError> {
        let row = if registry.is_empty() {
            sqlx::query("SELECT COALESCE(SUM(size_bytes), 0)::BIGINT AS total FROM artifact_cache_meta")
                .fetch_one(&self.pool)
                .await
        } else {
            sqlx::query("SELECT COALESCE(SUM(size_bytes), 0)::BIGINT AS total FROM artifact_cache_meta WHERE registry = $1")
                .bind(registry)
                .fetch_one(&self.pool)
                .await
        }
        .db_err()?;
        let total: i64 = row.try_get("total").unwrap_or(0);
        Ok(total as u64)
    }

    async fn list_lru(&self, registry: &str, limit: i64) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = if registry.is_empty() {
            sqlx::query(
                "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta ORDER BY last_accessed_at ASC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT artifact_key, registry, package_name, version, size_bytes, cached_at, last_accessed_at FROM artifact_cache_meta WHERE registry = $1 ORDER BY last_accessed_at ASC LIMIT $2",
            )
            .bind(registry)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        }
        .db_err()?;
        Ok(rows.into_iter().map(row_to_meta).collect())
    }
}

fn row_to_meta(row: sqlx::postgres::PgRow) -> ArtifactMeta {
    ArtifactMeta {
        artifact_key: row.get("artifact_key"),
        registry: row.get("registry"),
        package_name: row.get("package_name"),
        version: row.get("version"),
        size_bytes: row.try_get::<i64, _>("size_bytes").ok().map(|s| s as u64),
        cached_at: row.get("cached_at"),
        last_accessed_at: row.get("last_accessed_at"),
    }
}
