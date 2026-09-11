#[cfg(feature = "storage-fs")]
pub mod filesystem;

#[cfg(feature = "storage-fs")]
pub use filesystem::FilesystemStorageBackend;

#[cfg(feature = "storage-s3")]
pub mod s3;

#[cfg(feature = "storage-s3")]
pub use s3::S3StorageBackend;

pub mod router;
pub use router::StorageRouter;

/// Chunk size for streaming an object body back to a caller. Keeps peak memory
/// bounded to one chunk regardless of artifact size.
#[cfg(any(feature = "storage-fs", feature = "storage-s3"))]
pub(crate) const READ_CHUNK: usize = 64 * 1024;

/// Adapt an [`AsyncRead`](tokio::io::AsyncRead) into a chunked [`ByteStream`],
/// reading `READ_CHUNK` bytes at a time. Shared by the filesystem and S3
/// `retrieve` paths so the reader-chunking loop lives in one place.
#[cfg(any(feature = "storage-fs", feature = "storage-s3"))]
pub(crate) fn read_chunked<R>(reader: R, label: String) -> batlehub_core::ports::ByteStream
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use batlehub_core::error::CoreError;
    use bytes::Bytes;
    use tokio::io::AsyncReadExt;

    Box::pin(futures::stream::try_unfold(reader, move |mut reader| {
        let label = label.clone();
        async move {
            let mut buf = vec![0u8; READ_CHUNK];
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|e| CoreError::Storage(format!("read {label}: {e}")))?;
            if n == 0 {
                Ok(None)
            } else {
                buf.truncate(n);
                Ok(Some((Bytes::from(buf), reader)))
            }
        }
    }))
}

/// Rejects storage keys that could escape the storage root via path traversal.
///
/// Storage keys are built from untrusted package coordinates
/// (`{registry}/{name}/{version}[/…]`). Names legitimately contain `/`
/// (npm scopes like `@scope/name`, GitHub `owner/repo`), so `/` is allowed — but
/// a `..` path segment, an absolute (leading-`/`) key, a backslash, or a NUL byte
/// are not. This is the single chokepoint every storage backend funnels through,
/// so it protects all registry adapters regardless of their own input validation.
#[cfg(any(feature = "storage-fs", feature = "storage-s3"))]
pub(crate) fn ensure_safe_key(key: &str) -> Result<(), batlehub_core::error::CoreError> {
    use batlehub_core::error::CoreError;
    if key.is_empty() {
        return Err(CoreError::InvalidInput("empty storage key".into()));
    }
    if key.contains('\0') || key.contains('\\') {
        return Err(CoreError::InvalidInput(format!(
            "storage key {key:?} contains an illegal character"
        )));
    }
    if key.starts_with('/') {
        return Err(CoreError::InvalidInput(format!(
            "storage key {key:?} must not be absolute"
        )));
    }
    // Checks every percent-decoding of the key, not just its literal bytes: a
    // `%2e%2e` segment is a dot segment to anything that decodes the key later
    // (a URL parser, a path rebuilt from a decoded form), and `%252e%252e`
    // survives one round of decoding as `%2e%2e`. `validate_path_safe` rejects
    // these at the edge; this chokepoint is the last line of defence for a key
    // an adapter built without going through it.
    if batlehub_core::services::has_traversal_after_decoding(key) {
        return Err(CoreError::InvalidInput(format!(
            "storage key {key:?} contains a path-traversal segment"
        )));
    }
    Ok(())
}

#[cfg(all(test, any(feature = "storage-fs", feature = "storage-s3")))]
mod ensure_safe_key_tests {
    use super::ensure_safe_key;

    #[test]
    fn accepts_legitimate_keys() {
        for key in [
            "artifact:npm/@scope/name/1.2.3",
            "local:maven/com.example:lib/1.0.0/lib.jar",
            "cargo/tokio/1.38.0",
        ] {
            assert!(ensure_safe_key(key).is_ok(), "should accept {key}");
        }
    }

    #[test]
    fn rejects_traversal_and_absolute_keys() {
        for key in [
            "",
            "local:maven/../../../../etc/passwd/1.0",
            "/etc/passwd",
            "..",
            "a/../b",
            "a/..",
            "a\\..\\b",
            "with\0nul",
            // Aliases of a neighbour on the filesystem backend, which joins
            // the key verbatim: same file as `local:npm/a/b/1.0`.
            "local:npm/a//b/1.0",
            "local:npm/a/./b/1.0",
        ] {
            assert!(ensure_safe_key(key).is_err(), "should reject {key:?}");
        }
    }
}
