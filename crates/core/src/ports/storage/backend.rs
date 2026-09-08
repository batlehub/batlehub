use std::pin::Pin;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{Stream, StreamExt};
use serde::Deserialize;

use crate::error::CoreError;

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, CoreError>> + Send + 'static>>;

/// Config for the S3-compatible storage backend adapter. Lives in `core`
/// (rather than `crates/config`, which re-exports it) so `adapters` doesn't
/// need a dependency on the `config` crate.
#[derive(Debug, Deserialize)]
pub struct S3StorageConfig {
    pub bucket: String,
    pub region: String,
    pub prefix: Option<String>,
    pub endpoint_url: Option<String>,
    /// Use path-style URLs (required for RustFS, MinIO, and other S3-compatible stores).
    pub force_path_style: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct StorageMeta {
    pub content_type: Option<String>,
    pub size: Option<u64>,
    pub checksum: Option<String>,
}

pub struct StoredArtifact {
    pub stream: ByteStream,
    pub meta: StorageMeta,
}

/// Drain a [`ByteStream`] into a contiguous buffer.
///
/// The one place the whole-artifact buffer is unavoidable (the `store`/`move_key`
/// defaults that have no streaming backing, callers that genuinely need every
/// byte resident). Lives in the ports layer so both the trait defaults below and
/// higher layers (e.g. the proxy service) share a single implementation.
pub async fn collect_byte_stream(mut stream: ByteStream) -> Result<Bytes, CoreError> {
    let mut buf = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
    }
    Ok(buf.freeze())
}

/// What a streaming store wrote: the bare SHA-256 hex of the persisted bytes and
/// their total length. The digest is the same value [`crate::services::integrity::sha256_hex`]
/// would compute over the full artifact, so it doubles as both the dedup content
/// hash and the re-serve verification checksum (callers avoid a second pass).
#[derive(Debug, Clone)]
pub struct StoreOutcome {
    pub content_hash: String,
    pub size: u64,
}

/// Stores and retrieves cached artifact bytes (the actual file content).
///
/// Keys are opaque strings (typically a `PackageId::cache_key()` prefixed with `"artifact:"`).
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// Store artifact bytes. Overwrites any existing entry at `key`.
    async fn store(&self, key: &str, data: Bytes, meta: StorageMeta) -> Result<(), CoreError>;

    /// Store artifact bytes from a stream, returning the SHA-256 + size of what
    /// was written. Streaming backends override this to keep peak memory bounded
    /// to a single chunk regardless of artifact size.
    ///
    /// The default implementation collects the whole stream into memory and
    /// delegates to [`store`](Self::store), so backends that do not override it
    /// behave exactly as before (no streaming win, but correct). Callers that
    /// want the memory guarantee must go through a backend that overrides this.
    async fn store_streaming(
        &self,
        key: &str,
        stream: ByteStream,
        mut meta: StorageMeta,
    ) -> Result<StoreOutcome, CoreError> {
        let data = collect_byte_stream(stream).await?;
        let content_hash = crate::services::integrity::sha256_hex(&data);
        let size = data.len() as u64;
        if meta.size.is_none() {
            meta.size = Some(size);
        }
        self.store(key, data, meta).await?;
        Ok(StoreOutcome { content_hash, size })
    }

    /// Move an artifact from one physical key to another, overwriting any entry
    /// at `to`.
    ///
    /// Used by content-addressed staging: a blob is streamed to a staging key,
    /// then promoted to its final `blob/<hash>` key once the hash is known.
    /// The default copies via [`retrieve`](Self::retrieve) + [`store`](Self::store)
    /// + [`delete`](Self::delete); streaming backends override it with a cheap
    /// rename / server-side copy.
    ///
    /// This operates on **physical** keys. A backend that maps logical keys onto
    /// shared content (the deduplicating router) cannot give a logical key-move
    /// stable semantics and may return an error instead — callers that need to
    /// re-home a logical key should not rely on it.
    async fn move_key(&self, from: &str, to: &str) -> Result<(), CoreError> {
        let Some(artifact) = self.retrieve(from).await? else {
            return Err(CoreError::Storage(format!(
                "move_key source '{from}' does not exist"
            )));
        };
        let data = collect_byte_stream(artifact.stream).await?;
        self.store(to, data, artifact.meta).await?;
        self.delete(from).await?;
        Ok(())
    }

    /// Retrieve an artifact as a streaming response. Returns `None` if not cached.
    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError>;

    /// Check existence without reading data.
    async fn exists(&self, key: &str) -> Result<bool, CoreError>;

    /// Remove a cached artifact.
    ///
    /// Returns `true` if the key existed and was deleted, `false` if it was not present.
    /// Backends that cannot determine existence atomically (e.g. S3) return `true` on any
    /// successful delete call.
    async fn delete(&self, key: &str) -> Result<bool, CoreError>;

    // ── The prefix trio ───────────────────────────────────────────────────────
    //
    // **A leaf backend's implementation of the three methods below is never
    // reached by the running server.** `initialize_storage` always wraps the
    // configured backends in a `StorageRouter`, including the single-backend
    // case, and the router overrides all three to answer from its database
    // tables rather than delegating to the backend underneath it. It has to:
    // once dedup is in play the physical objects live at `blob/<sha256>`, so no
    // scan of the backend by a logical prefix such as `artifact:npm/` could
    // find them.
    //
    // A leaf implementation is therefore exercised only by its own tests. That
    // is not a licence to leave it approximate: it is part of the contract, the
    // trait is public API of the adapters crate, and the shadowing is a wiring
    // decision that a future caller holding an `Arc<dyn StorageBackend>` for a
    // leaf is under no obligation to preserve. Implement it correctly and test
    // it, and in particular never report having deleted what is still there.

    /// Remove all artifacts whose keys start with `prefix` and return the count
    /// deleted. Read the note above before implementing this: in the wired
    /// server the router's override is what runs.
    async fn delete_by_prefix(&self, prefix: &str) -> Result<usize, CoreError>;

    /// Count artifacts and sum their sizes for keys starting with `prefix`.
    /// Returns `(count, total_bytes)`. Shadowed by the router, as above.
    async fn stat_by_prefix(&self, prefix: &str) -> Result<(u64, u64), CoreError>;

    /// List all keys starting with `prefix`. Returns the logical keys (not
    /// backend-internal paths). Used by eviction, coherence, and deduplication.
    /// Shadowed by the router, as above.
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError>;
}
