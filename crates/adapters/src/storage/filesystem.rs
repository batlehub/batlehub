use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use batlehub_core::{
    error::CoreError,
    ports::{ByteStream, StorageBackend, StorageMeta, StoreOutcome, StoredArtifact},
};

use super::read_chunked;

/// Creates `path`'s parent directory (and any missing ancestors), if it has one.
/// Shared by `store`/`store_streaming`/`move_key`, which each need their
/// destination file's directory to exist before writing to it.
async fn ensure_parent_dir(path: &Path) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| CoreError::Storage(format!("create dirs for {}: {e}", path.display())))?;
    }
    Ok(())
}

/// Path of a uniquely-named staging file next to `path`.
///
/// Writes land here first and are renamed onto `path` only once complete, so a
/// `retrieve` racing a `store` sees either the previous artifact or the new one
/// and never a prefix of the bytes in flight. Being a sibling matters: `rename`
/// is only atomic within a filesystem, and a temp directory elsewhere may be a
/// different mount.
///
/// The suffix is deliberately not `.dat`, so an in-flight write stays invisible
/// to `walk_dat_files` — and therefore to `list_keys`/`stat_by_prefix`.
fn staging_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    path.with_file_name(name)
}

/// Removes an abandoned staging file. Only ever the staging file: a write that
/// fails part-way must leave whatever was already at the key untouched, since a
/// failed overwrite is no reason to lose a complete artifact.
///
/// Cleanup failures are logged rather than returned — the caller is already
/// propagating the more interesting error.
async fn discard_staging(tmp: &Path, context: &'static str) {
    if let Err(rm) = tokio::fs::remove_file(tmp).await {
        if rm.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %tmp.display(), error = %rm, context, "failed to remove staging file");
        }
    }
}

/// Publishes a completed staging file onto its final path, cleaning the staging
/// file up if the rename itself fails.
async fn publish_staging(tmp: &Path, path: &Path) -> Result<(), CoreError> {
    match tokio::fs::rename(tmp, path).await {
        Ok(()) => Ok(()),
        Err(e) => {
            discard_staging(tmp, "publish").await;
            Err(CoreError::Storage(format!(
                "publish {} -> {}: {e}",
                tmp.display(),
                path.display()
            )))
        }
    }
}

/// Recursively walks `dir` (depth-first via an explicit stack — async fns can't
/// recurse without boxing) and returns every `.dat` file found. A missing or
/// unreadable directory yields no entries, matching callers that tolerate a
/// prefix that hasn't been written to yet.
///
/// Shared by `stat_by_prefix`/`list_keys`/`delete_by_prefix`, which otherwise
/// each repeat this same traversal and differ only in what they do with a
/// matched file (count + sum size, reconstruct its logical key, or just count).
async fn walk_dat_files(dir: PathBuf) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&d).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let Ok(ftype) = entry.file_type().await else {
                continue;
            };
            if ftype.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(std::ffi::OsStr::to_str) == Some("dat") {
                files.push(path);
            }
        }
    }
    files
}

/// Stores cached artifacts on the local filesystem.
///
/// Each artifact is stored as a single `.dat` file. The key is sanitised:
/// `:` → `__`, `/` stays as the path separator, and `.dat` is appended.
/// The `.dat` suffix prevents a file at `…/v1.2.3.dat` from colliding with
/// the directory `…/v1.2.3/` needed when a sub-artifact (e.g. `.mod`) is
/// stored alongside the version info under the same version prefix.
pub struct FilesystemStorageBackend {
    root: PathBuf,
}

impl FilesystemStorageBackend {
    pub async fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root).await?;
        Ok(Self { root })
    }

    fn key_to_path(&self, key: &str) -> Result<PathBuf, CoreError> {
        crate::storage::ensure_safe_key(key)?;
        let rel = key.replace(':', "__");
        Ok(self.root.join(format!("{rel}.dat")))
    }

    /// A staging key (`staging:<uuid>/<artifact key>`) nests its file under a
    /// per-upload `staging__<uuid>/…` tree; once the file is gone the tree is
    /// empty and would otherwise be left behind, one per verified download.
    async fn prune_staging_dir(&self, key: &str) {
        let Some((id, _)) = key
            .strip_prefix(batlehub_core::ports::STAGING_PREFIX)
            .and_then(|rest| rest.split_once('/'))
        else {
            return;
        };
        let dir = self.root.join(format!("staging__{id}"));
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %e, "failed to prune staging directory");
            }
        }
    }

    /// Resolve `prefix` (a logical key prefix, not a full key) to the
    /// directory it maps to on disk. Shared by `stat_by_prefix`/`list_keys`/
    /// `delete_by_prefix`, which otherwise each repeat this same conversion.
    fn prefix_to_dir(&self, prefix: &str) -> Result<PathBuf, CoreError> {
        crate::storage::ensure_safe_key(prefix)?;
        let fs_rel = prefix.replace(':', "__");
        Ok(self.root.join(fs_rel.trim_end_matches('/')))
    }
}

#[async_trait]
impl StorageBackend for FilesystemStorageBackend {
    async fn store(&self, key: &str, data: Bytes, _meta: StorageMeta) -> Result<(), CoreError> {
        let path = self.key_to_path(key)?;
        ensure_parent_dir(&path).await?;
        let tmp = staging_path_for(&path);
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(|e| CoreError::Storage(format!("create file {}: {e}", tmp.display())))?;

        // `tokio::fs::File` buffers writes and completes them on a background
        // task, so `write_all` returning does not mean the bytes reached the OS —
        // and dropping the handle does not flush them. Flush before the rename,
        // or the file published at the key can still be short.
        let write_result = async {
            file.write_all(&data)
                .await
                .map_err(|e| CoreError::Storage(format!("write file {}: {e}", tmp.display())))?;
            file.flush()
                .await
                .map_err(|e| CoreError::Storage(format!("flush file {}: {e}", tmp.display())))
        }
        .await;
        // Close before renaming: on Windows a rename can fail while a handle is
        // still open, and the flush above has already done the work that matters.
        drop(file);

        if let Err(e) = write_result {
            discard_staging(&tmp, "store").await;
            return Err(e);
        }
        publish_staging(&tmp, &path).await?;

        tracing::debug!(key = %key, bytes = data.len(), "stored artifact on filesystem");
        Ok(())
    }

    /// Stream the bytes to disk, hashing as we go. Peak memory is one chunk: the
    /// SHA-256 is computed incrementally rather than over a buffered copy.
    async fn store_streaming(
        &self,
        key: &str,
        mut stream: ByteStream,
        _meta: StorageMeta,
    ) -> Result<StoreOutcome, CoreError> {
        let path = self.key_to_path(key)?;
        ensure_parent_dir(&path).await?;
        let tmp = staging_path_for(&path);
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(|e| CoreError::Storage(format!("create file {}: {e}", tmp.display())))?;

        // Write the stream to the staging file, hashing incrementally. On any
        // mid-stream failure (e.g. an upstream error or a size-limit abort
        // surfaced through the stream) the staging file is discarded and the key
        // is never touched, so a later `retrieve` can neither serve a truncated
        // artifact nor find a previously-cached one missing.
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let write_result: Result<(), CoreError> = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                hasher.update(&chunk);
                size += chunk.len() as u64;
                file.write_all(&chunk).await.map_err(|e| {
                    CoreError::Storage(format!("write file {}: {e}", tmp.display()))
                })?;
            }
            file.flush()
                .await
                .map_err(|e| CoreError::Storage(format!("flush file {}: {e}", tmp.display())))
        }
        .await;
        drop(file);

        if let Err(e) = write_result {
            discard_staging(&tmp, "store_streaming").await;
            return Err(e);
        }
        publish_staging(&tmp, &path).await?;

        tracing::debug!(key = %key, bytes = size, "streamed artifact to filesystem");
        Ok(StoreOutcome {
            content_hash: hex::encode(hasher.finalize()),
            size,
        })
    }

    /// Atomic rename within the same root — no bytes move through memory.
    async fn move_key(&self, from: &str, to: &str) -> Result<(), CoreError> {
        let from_path = self.key_to_path(from)?;
        let to_path = self.key_to_path(to)?;
        ensure_parent_dir(&to_path).await?;
        tokio::fs::rename(&from_path, &to_path).await.map_err(|e| {
            CoreError::Storage(format!(
                "rename {} -> {}: {e}",
                from_path.display(),
                to_path.display()
            ))
        })?;
        self.prune_staging_dir(from).await;
        Ok(())
    }

    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
        let path = self.key_to_path(key)?;
        let file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(CoreError::Storage(format!(
                    "open file {}: {e}",
                    path.display()
                )))
            }
        };
        let size = file.metadata().await.ok().map(|m| m.len());

        // Stream the file off disk in fixed-size chunks; peak memory is one chunk.
        // A zero-length file still yields exactly one (empty) chunk so consumers
        // that expect at least one item behave as they did before streaming.
        let stream: ByteStream = if size == Some(0) {
            Box::pin(stream::once(async { Ok(Bytes::new()) }))
        } else {
            read_chunked(file, path.display().to_string())
        };

        Ok(Some(StoredArtifact {
            stream,
            meta: StorageMeta {
                size,
                ..Default::default()
            },
        }))
    }

    async fn exists(&self, key: &str) -> Result<bool, CoreError> {
        Ok(self.key_to_path(key)?.exists())
    }

    async fn delete(&self, key: &str) -> Result<bool, CoreError> {
        let path = self.key_to_path(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                self.prune_staging_dir(key).await;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(CoreError::Storage(format!(
                "delete file {}: {e}",
                path.display()
            ))),
        }
    }

    async fn stat_by_prefix(&self, prefix: &str) -> Result<(u64, u64), CoreError> {
        let dir = self.prefix_to_dir(prefix)?;

        let mut count = 0u64;
        let mut total_bytes = 0u64;
        for path in walk_dat_files(dir).await {
            count += 1;
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                total_bytes += meta.len();
            }
        }
        Ok((count, total_bytes))
    }

    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        let dir = self.prefix_to_dir(prefix)?;

        let mut keys = Vec::new();
        for path in walk_dat_files(dir).await {
            // Reconstruct the logical key from the filesystem path.
            let Ok(rel) = path.strip_prefix(&self.root) else {
                continue;
            };
            let key = rel
                .to_string_lossy()
                .trim_end_matches(".dat")
                .replace("__", ":")
                .replace(std::path::MAIN_SEPARATOR, "/");
            keys.push(key);
        }
        Ok(keys)
    }

    async fn delete_by_prefix(&self, prefix: &str) -> Result<usize, CoreError> {
        let dir = self.prefix_to_dir(prefix)?;

        tracing::info!(dir = %dir.display(), prefix = %prefix, "delete_by_prefix: scanning directory");

        // Count .dat files before removing so we can return a meaningful number.
        let count = walk_dat_files(dir.clone()).await.len();

        tracing::info!(dir = %dir.display(), count, "delete_by_prefix: removing directory");

        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => Ok(count),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => {
                tracing::error!(dir = %dir.display(), error = %e, "delete_by_prefix: remove_dir_all failed");
                Err(CoreError::Storage(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    };

    use bytes::Bytes;
    use futures::StreamExt;

    use super::*;

    static DIR_ID: AtomicU64 = AtomicU64::new(0);

    async fn make_backend() -> FilesystemStorageBackend {
        let id = DIR_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("batlehub-test-fs-{pid}-{id}"));
        FilesystemStorageBackend::new(dir).await.unwrap()
    }

    async fn collect(artifact: StoredArtifact) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut stream = artifact.stream;
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk.unwrap());
        }
        buf
    }

    #[tokio::test]
    async fn store_and_retrieve_round_trip() {
        let b = make_backend().await;
        let data = Bytes::from_static(b"hello, fs");
        b.store(
            "artifact:npm/test-pkg",
            data.clone(),
            StorageMeta::default(),
        )
        .await
        .unwrap();
        let artifact = b
            .retrieve("artifact:npm/test-pkg")
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(collect(artifact).await, b"hello, fs");
    }

    #[tokio::test]
    async fn retrieve_missing_key_returns_none() {
        let b = make_backend().await;
        assert!(b.retrieve("artifact:npm/missing").await.unwrap().is_none());
    }

    fn chunked_stream(chunks: &[&'static [u8]]) -> ByteStream {
        let items: Vec<Result<Bytes, CoreError>> =
            chunks.iter().map(|c| Ok(Bytes::from_static(c))).collect();
        Box::pin(stream::iter(items))
    }

    #[tokio::test]
    async fn store_streaming_round_trips_and_hashes() {
        let b = make_backend().await;
        // "hello" split across chunks; SHA-256 of b"hello".
        let outcome = b
            .store_streaming(
                "artifact:npm/streamed",
                chunked_stream(&[b"he", b"", b"llo"]),
                StorageMeta::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.content_hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(outcome.size, 5);

        let artifact = b
            .retrieve("artifact:npm/streamed")
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(collect(artifact).await, b"hello");
    }

    #[tokio::test]
    async fn retrieve_streams_large_payload_in_chunks() {
        let b = make_backend().await;
        // Bigger than READ_CHUNK so retrieve yields multiple chunks.
        let big = vec![0xABu8; crate::storage::READ_CHUNK * 2 + 123];
        b.store(
            "artifact:npm/big",
            Bytes::from(big.clone()),
            StorageMeta::default(),
        )
        .await
        .unwrap();
        let artifact = b.retrieve("artifact:npm/big").await.unwrap().unwrap();
        let mut stream = artifact.stream;
        let mut chunks = 0;
        let mut total = Vec::new();
        while let Some(c) = stream.next().await {
            let c = c.unwrap();
            chunks += 1;
            total.extend_from_slice(&c);
        }
        assert_eq!(total, big);
        assert!(
            chunks >= 2,
            "expected a chunked read, got {chunks} chunk(s)"
        );
    }

    #[tokio::test]
    async fn a_staging_key_leaves_no_directory_behind() {
        let b = make_backend().await;
        let empty = |b: &FilesystemStorageBackend| {
            std::fs::read_dir(&b.root).unwrap().all(|e| {
                !e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("staging__")
            })
        };
        let promoted = batlehub_core::ports::staging_key_for("artifact:npm/a/1.0.0");
        b.store(&promoted, Bytes::from_static(b"x"), StorageMeta::default())
            .await
            .unwrap();
        b.move_key(&promoted, "blob/abc").await.unwrap();
        assert!(empty(&b), "move_key left the staging tree");
        let dropped = batlehub_core::ports::staging_key_for("artifact:npm/a/1.0.0");
        b.store(&dropped, Bytes::from_static(b"x"), StorageMeta::default())
            .await
            .unwrap();
        assert!(b.delete(&dropped).await.unwrap());
        assert!(empty(&b), "delete left the staging tree");
    }

    #[tokio::test]
    async fn move_key_renames_blob() {
        let b = make_backend().await;
        b.store(
            "blob/staging/abc",
            Bytes::from_static(b"promote me"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
        b.move_key("blob/staging/abc", "blob/deadbeef")
            .await
            .unwrap();
        assert!(!b.exists("blob/staging/abc").await.unwrap());
        let artifact = b.retrieve("blob/deadbeef").await.unwrap().unwrap();
        assert_eq!(collect(artifact).await, b"promote me");
    }

    #[tokio::test]
    async fn retrieve_empty_file_yields_one_empty_chunk() {
        let b = make_backend().await;
        b.store("artifact:npm/empty", Bytes::new(), StorageMeta::default())
            .await
            .unwrap();
        let artifact = b.retrieve("artifact:npm/empty").await.unwrap().unwrap();
        let mut stream = artifact.stream;
        let mut chunks = 0;
        let mut total = Vec::new();
        while let Some(c) = stream.next().await {
            chunks += 1;
            total.extend_from_slice(&c.unwrap());
        }
        assert!(total.is_empty());
        assert_eq!(chunks, 1, "empty file should still yield exactly one chunk");
    }

    #[tokio::test]
    async fn store_streaming_cleans_up_partial_file_on_error() {
        let b = make_backend().await;
        // A stream that yields some bytes, then errors mid-way.
        let items: Vec<Result<Bytes, CoreError>> = vec![
            Ok(Bytes::from_static(b"partial")),
            Err(CoreError::Registry("boom".into())),
        ];
        let stream: ByteStream = Box::pin(stream::iter(items));
        let res = b
            .store_streaming("artifact:npm/aborted", stream, StorageMeta::default())
            .await;
        assert!(res.is_err());
        // No truncated file must be left behind at the key.
        assert!(!b.exists("artifact:npm/aborted").await.unwrap());
        assert!(b.retrieve("artifact:npm/aborted").await.unwrap().is_none());
    }

    /// A `retrieve` racing a `store` on the same key must observe a complete
    /// artifact — the one being replaced or the one replacing it, never a prefix
    /// of the bytes in flight. Writing straight to the key fails this: `create`
    /// truncates first, so a reader that arrives mid-write gets a short file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_retrieve_never_observes_a_partial_artifact() {
        const KEY: &str = "artifact:npm/racing";
        const ROUNDS: usize = 40;

        let b = Arc::new(make_backend().await);
        // Several read chunks' worth, so a torn read is wide enough to catch.
        let payload = Bytes::from(vec![0xCDu8; crate::storage::READ_CHUNK * 4]);

        // Seed the key: the race under test is the overwrite, and this way every
        // read has something to observe rather than a legitimate `None`.
        b.store(KEY, payload.clone(), StorageMeta::default())
            .await
            .unwrap();

        let writing = Arc::new(AtomicBool::new(true));
        let writer = {
            let (b, payload, writing) = (Arc::clone(&b), payload.clone(), Arc::clone(&writing));
            tokio::spawn(async move {
                for _ in 0..ROUNDS {
                    b.store(KEY, payload.clone(), StorageMeta::default())
                        .await
                        .unwrap();
                }
                writing.store(false, Ordering::Release);
            })
        };

        let reader = {
            let (b, expected, writing) = (Arc::clone(&b), payload.clone(), Arc::clone(&writing));
            tokio::spawn(async move {
                let mut reads = 0usize;
                while writing.load(Ordering::Acquire) {
                    let artifact = b
                        .retrieve(KEY)
                        .await
                        .unwrap()
                        .expect("a seeded key must never vanish under an overwrite");
                    let got = collect(artifact).await;
                    assert_eq!(
                        got.len(),
                        expected.len(),
                        "retrieve observed a partial artifact"
                    );
                    assert_eq!(got, expected.as_ref(), "retrieve observed corrupt bytes");
                    reads += 1;
                }
                reads
            })
        };

        writer.await.unwrap();
        let reads = reader.await.unwrap();
        assert!(reads > 0, "the reader never ran, so nothing was exercised");
    }

    #[tokio::test]
    async fn exists_before_and_after_store() {
        let b = make_backend().await;
        assert!(!b.exists("artifact:npm/ex-pkg").await.unwrap());
        b.store(
            "artifact:npm/ex-pkg",
            Bytes::from_static(b"data"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
        assert!(b.exists("artifact:npm/ex-pkg").await.unwrap());
    }

    #[tokio::test]
    async fn delete_removes_file() {
        let b = make_backend().await;
        b.store(
            "artifact:npm/del-pkg",
            Bytes::from_static(b"bye"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
        b.delete("artifact:npm/del-pkg").await.unwrap();
        assert!(!b.exists("artifact:npm/del-pkg").await.unwrap());
    }

    #[tokio::test]
    async fn delete_missing_key_is_ok() {
        let b = make_backend().await;
        b.delete("artifact:npm/ghost").await.unwrap();
    }

    #[tokio::test]
    async fn colon_in_key_is_stored_and_retrieved() {
        let b = make_backend().await;
        b.store(
            "artifact:npm/colon-test",
            Bytes::from_static(b"x"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
        assert!(b.exists("artifact:npm/colon-test").await.unwrap());
    }

    #[tokio::test]
    async fn stat_by_prefix_counts_and_sums_sizes() {
        let b = make_backend().await;
        for i in 0..3u8 {
            b.store(
                &format!("artifact:npm/stat-pkg-{i}"),
                Bytes::from(vec![i; 100]),
                StorageMeta {
                    size: Some(100),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        // Prefix must end with '/' to resolve to the registry directory.
        let (count, bytes) = b.stat_by_prefix("artifact:npm/").await.unwrap();
        assert_eq!(count, 3);
        assert_eq!(bytes, 300);
    }

    #[tokio::test]
    async fn stat_by_prefix_returns_zero_for_nonexistent_prefix() {
        let b = make_backend().await;
        let (count, bytes) = b.stat_by_prefix("artifact:nonexistent/").await.unwrap();
        assert_eq!(count, 0);
        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn delete_by_prefix_removes_all_matching_files() {
        let b = make_backend().await;
        for i in 0..3u8 {
            b.store(
                &format!("artifact:npm/del-pkg-{i}"),
                Bytes::from(vec![0u8; 10]),
                StorageMeta::default(),
            )
            .await
            .unwrap();
        }
        b.store(
            "artifact:cargo/keep",
            Bytes::from_static(b"keep"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

        let deleted = b.delete_by_prefix("artifact:npm/").await.unwrap();
        assert!(deleted >= 3, "at least 3 npm artifacts should be deleted");

        let (remaining, _) = b.stat_by_prefix("artifact:npm/").await.unwrap();
        assert_eq!(remaining, 0, "no npm artifacts should remain");

        assert!(
            b.exists("artifact:cargo/keep").await.unwrap(),
            "cargo artifact must survive"
        );
    }

    #[tokio::test]
    async fn delete_by_prefix_existing_empty_dir_returns_zero() {
        // A directory that exists (so `remove_dir_all` succeeds) but contains
        // no `.dat` files must report 0 deletions, not the previous `count.max(1)`
        // false positive.
        let b = make_backend().await;
        let dir = b.root.join("artifact__npm").join("empty-pkg");
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let deleted = b.delete_by_prefix("artifact:npm/empty-pkg").await.unwrap();
        assert_eq!(deleted, 0);
    }

    #[tokio::test]
    async fn delete_by_prefix_nonexistent_returns_zero() {
        let b = make_backend().await;
        let deleted = b.delete_by_prefix("artifact:nonexistent/").await.unwrap();
        assert_eq!(deleted, 0);
    }

    #[tokio::test]
    async fn rejects_path_traversal_keys() {
        let b = make_backend().await;
        // A key whose `..` segments would escape the storage root.
        let evil = "local:npm/../../../../tmp/batlehub-traversal-probe/1.0";

        let store_err = b
            .store(evil, Bytes::from_static(b"x"), StorageMeta::default())
            .await;
        assert!(matches!(store_err, Err(CoreError::InvalidInput(_))));

        assert!(matches!(
            b.retrieve(evil).await,
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            b.delete(evil).await,
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            b.exists(evil).await,
            Err(CoreError::InvalidInput(_))
        ));
        assert!(matches!(
            b.delete_by_prefix("local:npm/../../../../tmp").await,
            Err(CoreError::InvalidInput(_))
        ));

        // Nothing was written outside the root.
        assert!(
            !std::path::Path::new("/tmp/batlehub-traversal-probe").exists(),
            "traversal must not create files outside the storage root"
        );
    }
}
