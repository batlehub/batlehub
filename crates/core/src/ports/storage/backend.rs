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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct S3StorageConfig {
    pub bucket: String,
    pub region: String,
    pub prefix: Option<String>,
    pub endpoint_url: Option<String>,
    /// Use path-style URLs (required for RustFS and other S3-compatible stores).
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

/// Prefix of a key holding bytes that are not yet verified and must not be
/// served: the proxy streams an artifact it may still refuse under one, and
/// [`StorageBackend::promote`]s it to the real key once the digest matches.
pub const STAGING_PREFIX: &str = "staging:";

/// A fresh staging key for `artifact_key`. The destination is carried in the
/// key so a router places the staged bytes on the backend the artifact will
/// live on, where promoting them is a rename rather than a copy.
pub fn staging_key_for(artifact_key: &str) -> String {
    format!("{STAGING_PREFIX}{}/{artifact_key}", uuid::Uuid::new_v4())
}

/// The artifact key a staging key was made for, or `None` for any other key.
pub fn staged_destination(key: &str) -> Option<&str> {
    key.strip_prefix(STAGING_PREFIX)?
        .split_once('/')
        .map(|(_, dest)| dest)
}

/// RFC 0048-bis §15 Q1: the size of every segment of a cold fill but its last.
/// 8 MiB is the multipart `PART_SIZE` and clears S3's 5 MiB floor for any part
/// but the last, so segments can be assembled with `UploadPartCopy`.
pub const FILL_SEGMENT_BYTES: usize = 8 * 1024 * 1024;

/// RFC 0048-bis: segment `n` of the fill staged at `staged`.
/// `staging:<fill>/<dest>` → `staging:<fill>.<n>/<dest>`; still a staging
/// key, so `staged_destination` and the router route it unchanged. `None`
/// when `staged` is not a staging key, or is already a segment key (its fill
/// part carries a `.`), so a segment never names another fill's segment.
pub fn segment_key(staged: &str, n: u32) -> Option<String> {
    let (fill, dest) = staged.strip_prefix(STAGING_PREFIX)?.split_once('/')?;
    if fill.is_empty() || fill.contains('.') || dest.is_empty() {
        return None;
    }
    Some(format!("{STAGING_PREFIX}{fill}.{n}/{dest}"))
}

/// The segment key for `staged`, or the `InvalidInput` a backend answers for
/// a key that is not a fill's staging key.
pub fn require_segment_key(staged: &str, n: u32) -> Result<String, CoreError> {
    segment_key(staged, n)
        .ok_or_else(|| CoreError::InvalidInput(format!("{staged:?} is not a fill's staging key")))
}

/// A fill has at least one segment — an empty artifact is one empty segment —
/// so assembling none is a caller's mistake every backend refuses alike.
pub fn require_segments(staged: &str, count: u32) -> Result<(), CoreError> {
    if count == 0 {
        return Err(CoreError::InvalidInput(format!(
            "assemble {staged}: a fill has at least one segment"
        )));
    }
    Ok(())
}

/// Drop the first `offset` bytes of `stream`.
pub(crate) fn skip_bytes(stream: ByteStream, offset: u64) -> ByteStream {
    let mut remaining = offset;
    Box::pin(stream.filter_map(move |chunk| {
        let out = match chunk {
            Ok(b) if remaining > 0 => {
                // `n` is at most `b.len()`, so the cast cannot truncate.
                let n = remaining.min(b.len() as u64);
                remaining -= n;
                let rest = b.slice(n as usize..);
                (!rest.is_empty()).then_some(Ok(rest))
            }
            other => Some(other),
        };
        futures::future::ready(out)
    }))
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

    /// Make the bytes staged at `staged` — written by
    /// [`store_streaming`](Self::store_streaming), which returned `outcome` —
    /// the content of `key`, and remove `staged`.
    ///
    /// The default re-streams them and deletes the staging key, which is
    /// correct for any backend. The deduplicating router overrides it: the
    /// staged bytes are already hashed, so they become `blob/<hash>` with a
    /// rename on first reference and are dropped on a dedup hit — no second
    /// write of the artifact, and no dedup rows for a key that lived for
    /// milliseconds.
    async fn promote(
        &self,
        staged: &str,
        key: &str,
        outcome: &StoreOutcome,
    ) -> Result<(), CoreError> {
        let _ = outcome;
        let artifact = self.retrieve(staged).await?.ok_or_else(|| {
            CoreError::Registry(format!(
                "staged artifact '{staged}' vanished before promotion"
            ))
        })?;
        if let Err(e) = self
            .store_streaming(key, artifact.stream, StorageMeta::default())
            .await
        {
            if let Err(cleanup) = self.delete(staged).await {
                tracing::warn!(key = %staged, error = %cleanup, "failed to delete staging artifact after promotion failure");
            }
            return Err(e);
        }
        if let Err(e) = self.delete(staged).await {
            tracing::warn!(key = %staged, error = %e, "failed to delete staging artifact after promotion");
        }
        Ok(())
    }

    // ── Fill segments (RFC 0048-bis §6.1) ─────────────────────────────────────
    //
    // A cold fill writes its bytes as `FILL_SEGMENT_BYTES` segments under its
    // staging key `staged` (`staging:<fill>/<artifact key>`), so any replica
    // can read the ones already committed, and assembles them into the
    // artifact once the digest is verified. Every segment but the last is
    // exactly `FILL_SEGMENT_BYTES` long, and segments are put in order.

    /// Write segment `n` of the fill staged at `staged`. The default stores it
    /// as its own object at [`segment_key`].
    async fn put_segment(&self, staged: &str, n: u32, data: Bytes) -> Result<(), CoreError> {
        let key = require_segment_key(staged, n)?;
        let meta = StorageMeta {
            size: Some(data.len() as u64),
            ..Default::default()
        };
        self.store(&key, data, meta).await
    }

    /// [`put_segment`](Self::put_segment) from the chunks a fill received, in
    /// order. A backend that can write them one after the other overrides it,
    /// so the segment is never joined into a second buffer beside the chunks
    /// its readers still hold. The default joins them.
    async fn put_segment_parts(
        &self,
        staged: &str,
        n: u32,
        parts: Vec<Bytes>,
    ) -> Result<(), CoreError> {
        let data = match parts.as_slice() {
            [one] => one.clone(),
            parts => {
                let mut buf = bytes::BytesMut::with_capacity(parts.iter().map(Bytes::len).sum());
                parts.iter().for_each(|b| buf.extend_from_slice(b));
                buf.freeze()
            }
        };
        self.put_segment(staged, n, data).await
    }

    /// Read segment `n` of the fill staged at `staged`; `None` if it is not
    /// there (never written, or already deleted).
    async fn get_segment(&self, staged: &str, n: u32) -> Result<Option<ByteStream>, CoreError> {
        let key = require_segment_key(staged, n)?;
        Ok(self.retrieve(&key).await?.map(|a| a.stream))
    }

    /// Write segments `0..count` of `staged`, concatenated, as the object at
    /// `dest`; `count` is at least 1. Whether the segments stay is the
    /// backend's: the filesystem renames its one file onto `dest`. The default re-streams them through
    /// [`store_streaming`](Self::store_streaming).
    async fn assemble_segments(
        &self,
        staged: &str,
        count: u32,
        dest: &str,
    ) -> Result<(), CoreError> {
        require_segments(staged, count)?;
        let mut segments = Vec::with_capacity(count as usize);
        for n in 0..count {
            let segment = self.get_segment(staged, n).await?.ok_or_else(|| {
                CoreError::Storage(format!("segment {n} of {staged} vanished before assembly"))
            })?;
            segments.push(segment);
        }
        let chained: ByteStream = Box::pin(futures::stream::iter(segments).flatten());
        self.store_streaming(dest, chained, StorageMeta::default())
            .await
            .map(|_| ())
    }

    /// Delete segments `0..upto` of `staged`, the ones that may exist. A
    /// segment already gone is not an error.
    async fn delete_segments(&self, staged: &str, upto: u32) -> Result<(), CoreError> {
        let mut first_err = None;
        for n in 0..upto {
            let key = require_segment_key(staged, n)?;
            if let Err(e) = self.delete(&key).await {
                tracing::warn!(key = %key, error = %e, "failed to delete fill segment");
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// The segment counterpart of [`promote`](Self::promote): make segments
    /// `0..count` of `staged` — whose concatenation hashed to `outcome` — the
    /// content of `key`, then delete them. On failure nothing is promoted and
    /// the segments are deleted too. The deduplicating router overrides it so
    /// a dedup hit assembles nothing.
    async fn promote_segments(
        &self,
        staged: &str,
        count: u32,
        key: &str,
        outcome: &StoreOutcome,
    ) -> Result<(), CoreError> {
        let _ = outcome;
        let assembled = self.assemble_segments(staged, count, key).await;
        if let Err(e) = self.delete_segments(staged, count).await {
            tracing::warn!(key = %staged, error = %e, "failed to delete fill segments after promotion");
        }
        assembled
    }

    /// [`retrieve`](Self::retrieve) from byte `offset` on: the stream starts
    /// there and `meta.size`, when known, is the length still to come. An
    /// offset at or past the end yields an empty stream. The default reads
    /// and discards the first `offset` bytes.
    async fn retrieve_from(
        &self,
        key: &str,
        offset: u64,
    ) -> Result<Option<StoredArtifact>, CoreError> {
        let Some(mut artifact) = self.retrieve(key).await? else {
            return Ok(None);
        };
        if offset > 0 {
            artifact.meta.size = artifact.meta.size.map(|s| s.saturating_sub(offset));
            artifact.stream = skip_bytes(artifact.stream, offset);
        }
        Ok(Some(artifact))
    }

    /// Delete staged bytes (`staging:` keys, the router's `blob/staging/`)
    /// last written more than `older_than` ago — what a crashed fill or upload
    /// left behind — and return how many objects went (a filesystem staging
    /// tree counts as one). The default deletes nothing.
    async fn delete_stale_staging(
        &self,
        older_than: std::time::Duration,
    ) -> Result<usize, CoreError> {
        let _ = older_than;
        Ok(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_segment_key_routes_as_its_fills_destination() {
        let staged = staging_key_for("artifact:npm/@scope/pkg/1.0.0");
        let seg = segment_key(&staged, 7).unwrap();
        assert!(seg.starts_with(STAGING_PREFIX));
        assert!(seg.contains(".7/"));
        assert_eq!(staged_destination(&seg), staged_destination(&staged));
        assert_ne!(segment_key(&staged, 0), segment_key(&staged, 1));
    }

    #[test]
    fn only_a_fills_staging_key_has_segments() {
        assert_eq!(segment_key("artifact:npm/pkg/1.0", 0), None);
        assert_eq!(segment_key("staging:/artifact:npm/pkg/1.0", 0), None);
        assert_eq!(segment_key("staging:abc", 0), None);
        let seg = segment_key("staging:abc/artifact:npm/pkg/1.0", 3).unwrap();
        assert_eq!(seg, "staging:abc.3/artifact:npm/pkg/1.0");
        // A segment key is not a fill's staging key: no segment of a segment.
        assert_eq!(segment_key(&seg, 0), None);
        assert!(require_segment_key("blob/abc", 0).is_err());
    }

    #[tokio::test]
    async fn skip_bytes_drops_exactly_the_offset_across_chunks() {
        let chunks: Vec<Result<Bytes, CoreError>> = ["abc", "de", "fghij"]
            .iter()
            .map(|c| Ok(Bytes::from_static(c.as_bytes())))
            .collect();
        for (offset, want) in [
            (0, "abcdefghij"),
            (2, "cdefghij"),
            (5, "fghij"),
            (6, "ghij"),
            (10, ""),
            (99, ""),
        ] {
            let stream: ByteStream = Box::pin(futures::stream::iter(
                chunks
                    .iter()
                    .map(|c| Ok(c.as_ref().unwrap().clone()))
                    .collect::<Vec<_>>(),
            ));
            let got = collect_byte_stream(skip_bytes(stream, offset))
                .await
                .unwrap();
            assert_eq!(got, want.as_bytes(), "offset {offset}");
        }
    }
}
