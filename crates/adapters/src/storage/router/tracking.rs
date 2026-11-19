use std::sync::Arc;

use batlehub_core::error::CoreError;
use batlehub_core::ports::StorageBackend;

use super::StorageRouter;

impl StorageRouter {
    /// Returns the backend recorded in `artifact_storage` for a logical key,
    /// or `None` if the key was never recorded.
    pub(super) async fn recorded_backend_for_key(
        &self,
        key: &str,
    ) -> Result<Option<Arc<dyn StorageBackend>>, CoreError> {
        use sqlx::Row;
        let result =
            sqlx::query("SELECT backend_name FROM artifact_storage WHERE storage_key = $1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| CoreError::Storage(e.to_string()))?;

        Ok(result
            .and_then(|r| r.try_get::<String, _>("backend_name").ok())
            .and_then(|name| self.backends.get(&name).cloned()))
    }

    /// Look up the physical `content_key` for a logical artifact key via the dedup tables.
    /// Returns `(content_key, backend_arc)` if a dedup entry exists.
    pub(super) async fn dedup_content_key(
        &self,
        logical_key: &str,
    ) -> Result<Option<(String, Arc<dyn StorageBackend>)>, CoreError> {
        let c = &crate::db::read_cache::CACHES.dedup;
        let mapping = match c.get(&logical_key.to_owned()) {
            Some(hit) => Some(hit),
            None => {
                let ticket = c.ticket();
                let found = self.dedup_mapping_uncached(logical_key).await?;
                // A miss is not cached: the first write of this key inserts
                // rows no trigger reports.
                if let Some(m) = &found {
                    c.put(ticket, logical_key.to_owned(), m.clone());
                }
                found
            }
        };
        let Some((content_key, backend_name)) = mapping else {
            return Ok(None);
        };
        let backend = self
            .backends
            .get(&backend_name)
            .cloned()
            .or_else(|| self.backends.get(&self.default_name).cloned());

        Ok(backend.map(|b| (content_key, b)))
    }

    /// `(content_key, backend_name)` for a logical key, from the database.
    async fn dedup_mapping_uncached(
        &self,
        logical_key: &str,
    ) -> Result<Option<(String, String)>, CoreError> {
        use sqlx::Row;
        let row = sqlx::query(
            r#"
            SELECT di.content_key, COALESCE(ast.backend_name, $2) AS backend_name
            FROM artifact_dedup_refs dr
            JOIN artifact_dedup_index di USING (content_hash)
            LEFT JOIN artifact_storage ast ON ast.storage_key = dr.logical_key
            WHERE dr.logical_key = $1
            "#,
        )
        .bind(logical_key)
        .bind(&self.default_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CoreError::Storage(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let content_key: String = row
            .try_get("content_key")
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        let backend_name: String = row
            .try_get("backend_name")
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(Some((content_key, backend_name)))
    }

    pub(super) async fn record_backend(
        &self,
        key: &str,
        backend_name: &str,
        size_bytes: Option<u64>,
    ) {
        let _ = sqlx::query(
            r#"
            INSERT INTO artifact_storage (storage_key, backend_name, stored_at, size_bytes)
            VALUES ($1, $2, NOW(), $3)
            ON CONFLICT (storage_key) DO UPDATE
                SET backend_name = EXCLUDED.backend_name,
                    size_bytes = EXCLUDED.size_bytes
            "#,
        )
        .bind(key)
        .bind(backend_name)
        .bind(size_bytes.map(|s| s as i64))
        .execute(&self.pool)
        .await
        .inspect_err(
            |e| tracing::warn!(error = %e, key, backend_name, "failed to record artifact backend"),
        );
    }

    pub(super) async fn lazy_update_size(&self, key: &str, size: u64) {
        let _ = sqlx::query(
            "UPDATE artifact_storage SET size_bytes = $1 WHERE storage_key = $2 AND size_bytes IS NULL",
        )
        .bind(size as i64)
        .bind(key)
        .execute(&self.pool)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, key, "failed to update artifact size"));
    }

    /// Execute the dedup-path delete: removes the logical→hash ref, decrements
    /// ref_count, and on the last reference removes the index row and the
    /// physical blob.
    ///
    /// Returns `false` if no dedup entry exists for this key (caller falls
    /// through to the legacy path).
    pub(super) async fn delete_dedup_entry(&self, key: &str) -> Result<bool, CoreError> {
        let backend_name: Option<String> = sqlx::query_scalar(
            r#"
            SELECT COALESCE(ast.backend_name, $2)
            FROM artifact_dedup_refs dr
            LEFT JOIN artifact_storage ast ON ast.storage_key = dr.logical_key
            WHERE dr.logical_key = $1
            "#,
        )
        .bind(key)
        .bind(&self.default_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CoreError::Storage(e.to_string()))?;
        let Some(backend_name) = backend_name else {
            return Ok(false);
        };

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CoreError::Storage(e.to_string()))?;

        // The ref is claimed inside the transaction: a concurrent delete of the
        // same key finds nothing here and must not decrement a second time.
        let content_hash: Option<String> = sqlx::query_scalar(
            "DELETE FROM artifact_dedup_refs WHERE logical_key = $1 RETURNING content_hash",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| CoreError::Storage(e.to_string()))?;
        let Some(content_hash) = content_hash else {
            return Ok(true);
        };

        let (new_ref_count, content_key): (i32, String) = sqlx::query_as(
            "UPDATE artifact_dedup_index SET ref_count = ref_count - 1 \
             WHERE content_hash = $1 RETURNING ref_count, content_key",
        )
        .bind(&content_hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| CoreError::Storage(e.to_string()))?;

        if new_ref_count <= 0 {
            sqlx::query("DELETE FROM artifact_dedup_index WHERE content_hash = $1")
                .bind(&content_hash)
                .execute(&mut *tx)
                .await
                .map_err(|e| CoreError::Storage(e.to_string()))?;
            // Before the commit, while the index row stays locked: a concurrent
            // store of the same bytes waits on it, so its blob write lands after
            // this delete, never before (the order `commit_new_hash` relies on).
            let backend = self.resolve_backend(&backend_name);
            let _ = backend.delete(&content_key).await.inspect_err(
                |e| tracing::warn!(error = %e, key = %content_key, "failed to delete physical blob"),
            );
        }

        tx.commit()
            .await
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(true)
    }

    /// Returns all logical keys matching `prefix` from both the dedup refs table and
    /// the legacy artifact_storage table (for pre-dedup artifacts).
    pub(super) async fn logical_keys_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<String>, CoreError> {
        use sqlx::Row;
        let like = super::like_prefix_pattern(prefix);
        let rows = sqlx::query(
            r#"
            SELECT logical_key AS key FROM artifact_dedup_refs WHERE logical_key LIKE $1 ESCAPE '\'
            UNION
            SELECT storage_key AS key FROM artifact_storage
              WHERE storage_key LIKE $1 ESCAPE '\'
                AND storage_key NOT IN (SELECT logical_key FROM artifact_dedup_refs WHERE logical_key LIKE $1 ESCAPE '\')
            "#,
        )
        .bind(&like)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<String, _>("key").ok())
            .collect())
    }
}
