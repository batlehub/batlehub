use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use sha2::Digest;
use sqlx::PgPool;

use batlehub_core::{
    error::CoreError,
    ports::{ByteStream, StorageBackend, StorageMeta, StoreOutcome, StoredArtifact},
};

mod routing;
mod tracking;

pub use routing::route_key_to_backend;

/// Escape `%`, `_`, and `\` in `prefix` and append a trailing `%`, so it can be
/// bound to a `LIKE ... ESCAPE '\'` clause without literal wildcard characters
/// in the prefix (e.g. from a package name/version) matching more broadly than
/// intended.
fn like_prefix_pattern(prefix: &str) -> String {
    let mut escaped = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if c == '\\' || c == '%' || c == '_' {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped.push('%');
    escaped
}

/// Routes artifact storage operations across multiple named backends, with
/// content-addressable deduplication via the `artifact_dedup_index` and
/// `artifact_dedup_refs` tables.
///
/// **Dedup model:**
/// - Physical blobs are stored under `"blob/{sha256_hex}"` in the selected backend.
/// - `artifact_dedup_refs` maps each logical artifact key to a content hash.
/// - `artifact_dedup_index` maps each content hash to its physical key and ref count.
/// - When two logical keys share identical bytes, only one physical blob is written.
/// - Deleting a logical key decrements the ref count; the blob is deleted only when
///   the ref count reaches zero.
/// - Artifacts written before this feature was enabled have no dedup entries and are
///   served via the legacy path (direct logical-key lookup).
///
/// **What the router delegates, and what it answers itself.** Per-key operations
/// reach the backend underneath: `store`, `retrieve`, `exists` and `delete` all
/// resolve a backend and call it, `delete` on the physical `blob/<hash>` key
/// once the last reference is gone.
///
/// The three prefix operations do not. [`delete_by_prefix`], [`stat_by_prefix`]
/// and [`list_keys`] are answered from `artifact_dedup_refs` unioned with
/// `artifact_storage`, and never call their namesake on a backend. They cannot
/// delegate: the caller's prefix is a *logical* one such as `artifact:npm/`,
/// while the physical objects sit at `blob/<sha256>`, so a backend-side scan
/// would match nothing. `delete_by_prefix` accordingly resolves the logical keys
/// here and deletes them one at a time through [`delete`].
///
/// The practical consequence is that a leaf backend's own implementation of
/// those three is dead code in the wired server, since `initialize_storage`
/// wraps even a single backend in a router. It still has to be correct; see the
/// note on the `StorageBackend` trait.
///
/// [`delete_by_prefix`]: StorageBackend::delete_by_prefix
/// [`stat_by_prefix`]: StorageBackend::stat_by_prefix
/// [`list_keys`]: StorageBackend::list_keys
/// [`delete`]: StorageBackend::delete
pub struct StorageRouter {
    pub(super) backends: HashMap<String, Arc<dyn StorageBackend>>,
    pub(super) default_name: String,
    /// registry_name → backend_name from RegistryConfig.storage
    pub(super) registry_assignments: HashMap<String, String>,
    pub(super) pool: PgPool,
}

impl StorageRouter {
    pub fn new(
        backends: HashMap<String, Arc<dyn StorageBackend>>,
        default_name: String,
        registry_assignments: HashMap<String, String>,
        pool: PgPool,
    ) -> Self {
        Self {
            backends,
            default_name,
            registry_assignments,
            pool,
        }
    }

    fn backend_name_for_key(&self, key: &str) -> &str {
        routing::route_key_to_backend(key, &self.registry_assignments, &self.default_name)
    }

    pub(super) fn resolve_backend(&self, name: &str) -> &Arc<dyn StorageBackend> {
        self.backends
            .get(name)
            .or_else(|| self.backends.get(&self.default_name))
            .expect("default storage backend must always be present")
    }

    /// Resolve the backend for a legacy (pre-dedup) artifact `key`: the backend
    /// recorded in `artifact_storage` when it exists, or the backend the
    /// routing rules currently assign the key to otherwise (never recorded, or
    /// predates that bookkeeping). Shared by `retrieve`/`exists`/`delete`'s
    /// legacy-path fallback.
    async fn legacy_backend_for(&self, key: &str) -> Result<Arc<dyn StorageBackend>, CoreError> {
        match self.recorded_backend_for_key(key).await? {
            Some(b) => Ok(b),
            None => Ok(self.resolve_backend(self.backend_name_for_key(key)).clone()),
        }
    }

    /// Run the dedup bookkeeping transaction for a logical `target.key` whose
    /// bytes hash to `target.content_hash`, materializing the physical blob
    /// from `source` only when this is its first reference.
    ///
    /// Shared by [`store`](StorageBackend::store) (bytes already in hand) and
    /// [`store_streaming`](StorageBackend::store_streaming) (bytes already
    /// staged at a temporary key). The blob is materialized *inside* the open
    /// transaction so a backend failure rolls the dedup rows back rather than
    /// leaving them pointing at a missing blob. Any staged blob that turns out
    /// to be redundant (identical re-store, or a dedup hit) is deleted.
    async fn finalize_dedup(
        &self,
        target: &DedupTarget<'_>,
        backend_name: &str,
        backend: &Arc<dyn StorageBackend>,
        source: BlobSource,
    ) -> Result<(), CoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CoreError::Storage(format!("failed to begin transaction: {e}")))?;

        // Check whether this logical key already maps to a content hash.  A
        // re-store of identical bytes for the same key must NOT increment ref_count.
        let existing_hash: Option<String> = sqlx::query_scalar(
            "SELECT content_hash FROM artifact_dedup_refs WHERE logical_key = $1 FOR UPDATE",
        )
        .bind(target.key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| CoreError::Storage(e.to_string()))?;

        if existing_hash.as_deref() == Some(target.content_hash) {
            Self::reuse_identical_blob(tx, target.content_key, backend, source).await?;
        } else {
            self.commit_new_hash(tx, target, backend, existing_hash, source)
                .await?;
        }

        // Keep the legacy artifact_storage record for routing and size queries.
        self.record_backend(target.key, backend_name, target.size)
            .await;

        Ok(())
    }

    /// Re-store of identical bytes for a key that already maps to `content_hash`:
    /// the dedup rows are already correct, so roll the transaction back and only
    /// touch the physical blob (restore it if it went missing, else discard the
    /// redundant staged copy).
    async fn reuse_identical_blob(
        tx: sqlx::Transaction<'_, sqlx::Postgres>,
        content_key: &str,
        backend: &Arc<dyn StorageBackend>,
        source: BlobSource,
    ) -> Result<(), CoreError> {
        tx.rollback().await.ok();
        if backend.exists(content_key).await.unwrap_or(false) {
            // Identical bytes re-stored and the physical blob is present — nothing to do.
            source.discard_staged(backend).await;
        } else if let Err(e) = source.materialize(backend, content_key).await {
            // Same hash but the physical blob is gone (e.g. storage was cleared without
            // resetting the DB). Restore the blob without touching ref counts.
            source.discard_staged(backend).await;
            return Err(e);
        }
        Ok(())
    }

    /// A new (or changed) content hash for `key`: increment ref counts, remap the
    /// logical key, decrement the replaced hash, materialize the blob on first
    /// reference, and commit. The blob is written *inside* the transaction so a
    /// backend failure rolls the dedup rows back.
    async fn commit_new_hash(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        target: &DedupTarget<'_>,
        backend: &Arc<dyn StorageBackend>,
        existing_hash: Option<String>,
        source: BlobSource,
    ) -> Result<(), CoreError> {
        // Increment (or insert) ref count for the new hash.
        let count: i32 = sqlx::query_scalar(
            r#"
            INSERT INTO artifact_dedup_index (content_hash, content_key, ref_count, size_bytes)
            VALUES ($1, $2, 1, $3)
            ON CONFLICT (content_hash) DO UPDATE
                SET ref_count = artifact_dedup_index.ref_count + 1
            RETURNING ref_count
            "#,
        )
        .bind(target.content_hash)
        .bind(target.content_key)
        .bind(target.size.map(|s| s as i64))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| CoreError::Storage(format!("dedup index upsert failed: {e}")))?;

        // Map logical key → content hash (propagate errors instead of silently dropping).
        // This must happen before the old hash's row is touched below, so that the
        // foreign key from artifact_dedup_refs no longer points at the old hash when
        // we try to delete it.
        sqlx::query(
            r#"
            INSERT INTO artifact_dedup_refs (logical_key, content_hash)
            VALUES ($1, $2)
            ON CONFLICT (logical_key) DO UPDATE SET content_hash = EXCLUDED.content_hash
            "#,
        )
        .bind(target.key)
        .bind(target.content_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| CoreError::Storage(format!("dedup refs insert failed: {e}")))?;

        // Decrement ref count for the previous hash if the key is being replaced.
        if let Some(old_hash) = &existing_hash {
            let old_count: i32 = sqlx::query_scalar(
                "UPDATE artifact_dedup_index SET ref_count = ref_count - 1 \
                 WHERE content_hash = $1 RETURNING ref_count",
            )
            .bind(old_hash)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| CoreError::Storage(e.to_string()))?;

            if old_count <= 0 {
                sqlx::query("DELETE FROM artifact_dedup_index WHERE content_hash = $1")
                    .bind(old_hash)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| CoreError::Storage(e.to_string()))?;
            }
        }

        // Write the physical blob while the transaction is still open.  Doing
        // this before commit ensures a backend failure causes a full rollback
        // rather than leaving orphaned dedup rows that point to a missing blob.
        // First reference, or a later one whose blob is not actually there. The
        // second case is the one that bites: `ref_count > 1` is a claim the
        // *index* makes about the backend, and when storage has been cleared
        // without resetting the database the claim is false. Trusting it makes
        // `store_streaming` return success having written nothing, so the
        // caller's `retrieve` — the proxy's promote step — finds the staged
        // artifact "vanished" and answers 502. Every subsequent fetch of those
        // bytes dedup-hits the same dangling row, so the artifact can never be
        // cached again: the failure is permanent and repairs itself only if the
        // *same logical key* is re-stored (`reuse_identical_blob`, which does
        // check). Found by tests/heavy/npm.sh, whose per-run storage directory
        // is thrown away while the dedup index in Postgres is not — which is
        // also what an operator does when they recreate a cache bucket.
        let blob_present = count > 1 && backend.exists(target.content_key).await.unwrap_or(false);
        if !blob_present {
            if let Err(e) = source.materialize(backend, target.content_key).await {
                let _ = tx.rollback().await;
                source.discard_staged(backend).await;
                return Err(e);
            }
        } else {
            // The blob already exists from another reference — drop the
            // redundant staged copy (no-op for the inline path).
            source.discard_staged(backend).await;
        }

        tx.commit()
            .await
            .map_err(|e| CoreError::Storage(e.to_string()))?;
        Ok(())
    }
}

/// The logical/physical identity of one dedup transaction: which logical key
/// is being stored, the content hash its bytes produced, the physical blob
/// key that hash maps to, and the byte size (when known). Built once by
/// `store`/`store_streaming` and threaded through [`StorageRouter::finalize_dedup`]
/// and [`StorageRouter::commit_new_hash`] by reference.
///
/// Deliberately excludes `backend_name`/`backend`/`source`: those describe
/// *where* the blob is written and *how* its bytes are obtained, a separate
/// concern from *what* is being deduplicated.
struct DedupTarget<'a> {
    key: &'a str,
    content_hash: &'a str,
    content_key: &'a str,
    size: Option<u64>,
}

/// How [`StorageRouter::finalize_dedup`] obtains the physical blob bytes when a
/// content hash is seen for the first time.
enum BlobSource {
    /// Bytes already in memory (the `store` path).
    Inline(Bytes, StorageMeta),
    /// Bytes already streamed to a staging key (the `store_streaming` path),
    /// promoted to the content key with a cheap backend move.
    Staged(String),
}

impl BlobSource {
    /// Place the blob at `content_key` (first reference only).
    async fn materialize(
        &self,
        backend: &Arc<dyn StorageBackend>,
        content_key: &str,
    ) -> Result<(), CoreError> {
        match self {
            BlobSource::Inline(data, meta) => {
                backend.store(content_key, data.clone(), meta.clone()).await
            }
            BlobSource::Staged(staging_key) => backend.move_key(staging_key, content_key).await,
        }
    }

    /// Best-effort removal of a staged blob that won't be promoted.
    async fn discard_staged(&self, backend: &Arc<dyn StorageBackend>) {
        if let BlobSource::Staged(staging_key) = self {
            if let Err(e) = backend.delete(staging_key).await {
                tracing::warn!(key = %staging_key, error = %e, "failed to delete staged blob");
            }
        }
    }
}

#[async_trait]
impl StorageBackend for StorageRouter {
    async fn store(&self, key: &str, data: Bytes, meta: StorageMeta) -> Result<(), CoreError> {
        let content_hash = hex::encode(sha2::Sha256::digest(&data));
        let content_key = format!("blob/{content_hash}");
        let size = meta.size;

        let backend_name = self.backend_name_for_key(key).to_owned();
        let backend = self.resolve_backend(&backend_name).clone();

        let target = DedupTarget {
            key,
            content_hash: &content_hash,
            content_key: &content_key,
            size,
        };
        self.finalize_dedup(
            &target,
            &backend_name,
            &backend,
            BlobSource::Inline(data, meta),
        )
        .await
    }

    /// Streaming counterpart to [`store`](Self::store). The bytes are streamed
    /// to a temporary staging key on the target backend (peak memory bounded to
    /// one chunk/part), and the content hash that decides dedup is computed
    /// during that write. The staged blob is then promoted to `blob/<hash>` with
    /// a cheap backend move on first reference, or discarded on a dedup hit.
    async fn store_streaming(
        &self,
        key: &str,
        stream: ByteStream,
        meta: StorageMeta,
    ) -> Result<StoreOutcome, CoreError> {
        let backend_name = self.backend_name_for_key(key).to_owned();
        let backend = self.resolve_backend(&backend_name).clone();

        let staging_key = format!("blob/staging/{}", uuid::Uuid::new_v4());
        let outcome = match backend.store_streaming(&staging_key, stream, meta).await {
            Ok(o) => o,
            Err(e) => {
                // The stream failed mid-write (e.g. size limit hit, or upstream
                // error). Drop any partially-written staging blob.
                if let Err(del) = backend.delete(&staging_key).await {
                    tracing::warn!(key = %staging_key, error = %del, "failed to delete partial staging blob");
                }
                return Err(e);
            }
        };
        let content_key = format!("blob/{}", outcome.content_hash);
        let size = Some(outcome.size);

        // `finalize_dedup` promotes or discards the staged blob on its own happy
        // paths, but its early DB-error returns do not. Keep the staging key here
        // and clean it up if finalize fails, so a transient DB error can never
        // leak an orphaned `blob/staging/<uuid>` (there is no staging GC sweep).
        let target = DedupTarget {
            key,
            content_hash: &outcome.content_hash,
            content_key: &content_key,
            size,
        };
        if let Err(e) = self
            .finalize_dedup(
                &target,
                &backend_name,
                &backend,
                BlobSource::Staged(staging_key.clone()),
            )
            .await
        {
            if let Err(del) = backend.delete(&staging_key).await {
                tracing::warn!(key = %staging_key, error = %del, "failed to delete staging blob after finalize_dedup error");
            }
            return Err(e);
        }

        Ok(outcome)
    }

    /// Intentionally unsupported: a logical key here is a row in `artifact_dedup_refs`
    /// pointing at a shared content blob, not a movable physical object, so a
    /// logical-key move has no stable meaning (see the `move_key` note on the
    /// [`StorageBackend`] trait). The router promotes staged blobs by calling
    /// `move_key` on the *inner* leaf backend, never on itself.
    async fn move_key(&self, _from: &str, _to: &str) -> Result<(), CoreError> {
        Err(CoreError::Storage(
            "move_key is not supported on the deduplicating storage router".into(),
        ))
    }

    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
        // Dedup path: resolve logical key → physical content key.
        if let Some((content_key, backend)) = self.dedup_content_key(key).await? {
            let artifact = backend.retrieve(&content_key).await?;
            if artifact.is_some() {
                return Ok(artifact);
            }
            // Physical blob missing (possible race during first write); fall through.
        }

        // Legacy path: artifact was stored before dedup was enabled.
        let backend = self.legacy_backend_for(key).await?;
        let artifact = backend.retrieve(key).await?;
        if let Some(ref a) = artifact {
            if let Some(size) = a.meta.size {
                self.lazy_update_size(key, size).await;
            }
        }
        Ok(artifact)
    }

    async fn exists(&self, key: &str) -> Result<bool, CoreError> {
        // Dedup path.
        if let Some((content_key, backend)) = self.dedup_content_key(key).await? {
            return backend.exists(&content_key).await;
        }

        // Legacy path.
        self.legacy_backend_for(key).await?.exists(key).await
    }

    async fn delete(&self, key: &str) -> Result<bool, CoreError> {
        // ── Dedup path ────────────────────────────────────────────────────────
        // Delegates the heavy DB work to `delete_dedup_entry` in tracking.rs.
        let existed = if let Some((should_delete_blob, content_key, blob_backend_name)) =
            self.delete_dedup_entry(key).await?
        {
            // Delete physical blob when no more references.
            if should_delete_blob {
                let backend = self.resolve_backend(&blob_backend_name);
                let _ = backend.delete(&content_key).await.inspect_err(
                    |e| tracing::warn!(error = %e, key = %content_key, "failed to delete physical blob"),
                );
            }
            true
        } else {
            // ── Legacy path ───────────────────────────────────────────────────
            let backend = self.legacy_backend_for(key).await?;
            backend.delete(key).await?
        };

        // Clean up the routing record regardless of path.
        let _ = sqlx::query("DELETE FROM artifact_storage WHERE storage_key = $1")
            .bind(key)
            .execute(&self.pool)
            .await
            .inspect_err(
                |e| tracing::warn!(error = %e, key, "failed to delete artifact_storage record"),
            );

        Ok(existed)
    }

    /// Returns count and total size of all logical artifact keys matching `prefix`.
    /// Queries the DB tables rather than the physical backend (which now stores
    /// content-addressed blobs under `blob/` prefixes).
    async fn stat_by_prefix(&self, prefix: &str) -> Result<(u64, u64), CoreError> {
        use sqlx::Row;
        let like = like_prefix_pattern(prefix);
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) AS cnt, COALESCE(SUM(size_bytes), 0) AS total
            FROM artifact_storage
            WHERE storage_key LIKE $1 ESCAPE '\'
            "#,
        )
        .bind(&like)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::Storage(e.to_string()))?;

        let count: i64 = row.try_get("cnt").unwrap_or(0);
        let total: i64 = row.try_get("total").unwrap_or(0);
        Ok((count as u64, total as u64))
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<usize, CoreError> {
        let keys = self.logical_keys_by_prefix(prefix).await?;
        let count = keys.len();
        for key in keys {
            if let Err(e) = self.delete(&key).await {
                tracing::warn!(error = %e, key, "delete_by_prefix: failed to delete artifact");
            }
        }
        Ok(count)
    }

    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        self.logical_keys_by_prefix(prefix).await
    }
}
