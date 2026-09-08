#![cfg(feature = "storage-s3")]

//! Integration tests for `S3StorageBackend`.
//!
//! Requires a running S3-compatible service. Set `S3_TEST_ENDPOINT` to opt in:
//!
//!   task test:s3                                  # starts MinIO automatically
//!   S3_TEST_ENDPOINT=http://127.0.0.1:19000 \
//!     AWS_ACCESS_KEY_ID=minioadmin \
//!     AWS_SECRET_ACCESS_KEY=minioadmin \
//!     cargo test -p batlehub-adapters --features storage-s3 --test s3_storage

use aws_config::BehaviorVersion;
use aws_sdk_s3::{config::Builder as S3ConfigBuilder, Client};
use batlehub_adapters::storage::S3StorageBackend;
use batlehub_core::ports::{
    ByteStream, S3StorageConfig, StorageBackend, StorageMeta, StoredArtifact,
};
use bytes::Bytes;
use futures::{stream, StreamExt};

const BUCKET: &str = "test-artifacts";
const REGION: &str = "us-east-1";

/// Returns the S3 test endpoint, or `None` to skip the test.
/// Credentials are taken from `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`.
fn s3_endpoint() -> Option<String> {
    std::env::var("S3_TEST_ENDPOINT").ok()
}

async fn s3_client(endpoint: &str) -> Client {
    let sdk_config = aws_config::defaults(BehaviorVersion::latest())
        .region(aws_config::Region::new(REGION))
        .endpoint_url(endpoint)
        .load()
        .await;

    let s3_cfg = S3ConfigBuilder::from(&sdk_config)
        .force_path_style(true)
        .build();

    let client = Client::from_conf(s3_cfg);

    // Idempotent — no-op if the bucket was created by a previous test run.
    let _ = client.create_bucket().bucket(BUCKET).send().await;

    client
}

async fn make_backend(endpoint: &str) -> S3StorageBackend {
    S3StorageBackend::from_client(s3_client(endpoint).await, BUCKET.to_owned(), String::new())
}

/// A backend scoped to a configured object prefix, plus the raw client behind
/// it — so a test can assert against the *full* object key S3 actually sees,
/// not only against the prefix-stripped view the backend presents.
async fn make_prefixed_backend(endpoint: &str, prefix: &str) -> (S3StorageBackend, Client) {
    let client = s3_client(endpoint).await;
    (
        S3StorageBackend::from_client(client.clone(), BUCKET.to_owned(), prefix.to_owned()),
        client,
    )
}

async fn collect(artifact: StoredArtifact) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stream = artifact.stream;
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.unwrap());
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn store_and_retrieve_round_trip() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    let data = Bytes::from_static(b"hello, s3");
    backend
        .store(
            "key-rt",
            data.clone(),
            StorageMeta {
                content_type: Some("text/plain".to_owned()),
                size: Some(data.len() as u64),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let artifact = backend
        .retrieve("key-rt")
        .await
        .unwrap()
        .expect("should exist");
    assert_eq!(collect(artifact).await, b"hello, s3");
}

#[tokio::test]
async fn retrieve_missing_key_returns_none() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;
    assert!(backend.retrieve("no-such-key").await.unwrap().is_none());
}

#[tokio::test]
async fn exists_before_and_after_store() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    assert!(!backend.exists("key-ex").await.unwrap());

    backend
        .store(
            "key-ex",
            Bytes::from_static(b"data"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    assert!(backend.exists("key-ex").await.unwrap());
}

#[tokio::test]
async fn delete_removes_artifact() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    backend
        .store(
            "key-del",
            Bytes::from_static(b"bye"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    backend.delete("key-del").await.unwrap();

    assert!(!backend.exists("key-del").await.unwrap());
}

#[tokio::test]
async fn delete_missing_key_is_ok() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;
    // NoSuchKey on delete must not propagate as an error.
    backend.delete("ghost-key").await.unwrap();
}

#[tokio::test]
async fn stat_by_prefix_counts_and_sums_sizes() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    for i in 0..3u8 {
        backend
            .store(
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

    let (count, bytes) = backend
        .stat_by_prefix("artifact:npm/stat-pkg-")
        .await
        .unwrap();
    assert_eq!(count, 3);
    assert_eq!(bytes, 300);
}

#[tokio::test]
async fn delete_by_prefix_removes_only_matching_keys() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    for i in 0..3u8 {
        backend
            .store(
                &format!("artifact:npm/del-pkg-{i}"),
                Bytes::from(vec![0u8; 10]),
                StorageMeta::default(),
            )
            .await
            .unwrap();
    }
    // A key under a different prefix that must survive.
    backend
        .store(
            "artifact:cargo/del-pkg-0",
            Bytes::from_static(b"keep"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let deleted = backend
        .delete_by_prefix("artifact:npm/del-pkg-")
        .await
        .unwrap();
    assert_eq!(deleted, 3);

    let (remaining, _) = backend
        .stat_by_prefix("artifact:npm/del-pkg-")
        .await
        .unwrap();
    assert_eq!(remaining, 0);

    assert!(backend.exists("artifact:cargo/del-pkg-0").await.unwrap());
}

/// A configured backend prefix must not change *what* `delete_by_prefix`
/// deletes. The keys `list_objects_v2` hands back already carry the prefix;
/// addressing the delete by the prefix-stripped logical key instead asks S3 to
/// remove objects that do not exist, which it reports as a success — a purge
/// that claims to have cleared the registry while every artifact is still in
/// the bucket. The unprefixed test above cannot see that, because with an empty
/// prefix the stripped key and the real key are the same string.
#[tokio::test]
async fn delete_by_prefix_removes_objects_under_a_configured_prefix() {
    let Some(ep) = s3_endpoint() else { return };
    let (backend, client) = make_prefixed_backend(&ep, "tenant-del/").await;

    for i in 0..3u8 {
        backend
            .store(
                &format!("artifact:npm/pfx-del-{i}"),
                Bytes::from(vec![0u8; 10]),
                StorageMeta::default(),
            )
            .await
            .unwrap();
    }
    // Same tenant, outside the deleted prefix: must survive.
    backend
        .store(
            "artifact:cargo/pfx-del-0",
            Bytes::from_static(b"keep"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let deleted = backend
        .delete_by_prefix("artifact:npm/pfx-del-")
        .await
        .unwrap();
    assert_eq!(deleted, 3);

    let (remaining, _) = backend
        .stat_by_prefix("artifact:npm/pfx-del-")
        .await
        .unwrap();
    assert_eq!(
        remaining, 0,
        "delete_by_prefix reported a purge it did not perform"
    );

    // Stated in S3's own terms: the object is gone at its full key, prefix
    // included, so the count above cannot be satisfied by a delete that missed.
    let head = client
        .head_object()
        .bucket(BUCKET)
        .key("tenant-del/artifact:npm/pfx-del-0")
        .send()
        .await;
    assert!(
        head.is_err(),
        "object should no longer exist at its full, prefixed key"
    );

    assert!(backend.exists("artifact:cargo/pfx-del-0").await.unwrap());
}

fn byte_stream(chunks: Vec<Vec<u8>>) -> ByteStream {
    Box::pin(stream::iter(chunks).map(|c| Ok(Bytes::from(c))))
}

#[tokio::test]
async fn store_streaming_small_payload_uses_single_put() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    let outcome = backend
        .store_streaming(
            "key-stream-small",
            byte_stream(vec![b"hello".to_vec(), b", streamed".to_vec()]),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    assert_eq!(outcome.size, 15);
    let artifact = backend
        .retrieve("key-stream-small")
        .await
        .unwrap()
        .expect("should exist");
    assert_eq!(collect(artifact).await, b"hello, streamed");
}

#[tokio::test]
async fn store_streaming_large_payload_uses_multipart() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    // Two chunks of 9 MiB each: crosses the 8 MiB part size twice, exercising
    // the multipart create/upload-part/complete path with more than one part.
    let chunk = vec![7u8; 9 * 1024 * 1024];
    let total_len = chunk.len() * 2;
    let outcome = backend
        .store_streaming(
            "key-stream-large",
            byte_stream(vec![chunk.clone(), chunk]),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    assert_eq!(outcome.size, total_len as u64);
    let artifact = backend
        .retrieve("key-stream-large")
        .await
        .unwrap()
        .expect("should exist");
    assert_eq!(collect(artifact).await.len(), total_len);
}

#[tokio::test]
async fn move_key_copies_then_deletes_source() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    backend
        .store(
            "key-move-src",
            Bytes::from_static(b"movable"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    backend
        .move_key("key-move-src", "key-move-dst")
        .await
        .unwrap();

    assert!(!backend.exists("key-move-src").await.unwrap());
    let artifact = backend
        .retrieve("key-move-dst")
        .await
        .unwrap()
        .expect("moved artifact should exist at destination");
    assert_eq!(collect(artifact).await, b"movable");
}

#[tokio::test]
async fn list_keys_returns_logical_keys_under_prefix() {
    let Some(ep) = s3_endpoint() else { return };
    let backend = make_backend(&ep).await;

    for i in 0..3u8 {
        backend
            .store(
                &format!("artifact:npm/list-pkg-{i}"),
                Bytes::from_static(b"x"),
                StorageMeta::default(),
            )
            .await
            .unwrap();
    }

    let mut keys = backend.list_keys("artifact:npm/list-pkg-").await.unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "artifact:npm/list-pkg-0".to_owned(),
            "artifact:npm/list-pkg-1".to_owned(),
            "artifact:npm/list-pkg-2".to_owned(),
        ]
    );
}

#[tokio::test]
async fn new_creates_backend_and_round_trips_through_configured_prefix() {
    let Some(ep) = s3_endpoint() else { return };
    // Ensure the bucket exists (idempotent) before `new()`'s head_bucket check.
    make_backend(&ep).await;

    let cfg = S3StorageConfig {
        bucket: BUCKET.to_owned(),
        region: REGION.to_owned(),
        prefix: Some("tenant-a/".to_owned()),
        endpoint_url: Some(ep),
        force_path_style: Some(true),
    };
    let backend = S3StorageBackend::new(&cfg).await.unwrap();

    backend
        .store(
            "key-prefixed",
            Bytes::from_static(b"scoped"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let artifact = backend
        .retrieve("key-prefixed")
        .await
        .unwrap()
        .expect("should exist under configured prefix");
    assert_eq!(collect(artifact).await, b"scoped");

    let keys = backend.list_keys("key-prefixed").await.unwrap();
    assert_eq!(keys, vec!["key-prefixed".to_owned()]);
}

#[tokio::test]
async fn new_fails_for_unreachable_bucket() {
    let Some(ep) = s3_endpoint() else { return };
    let cfg = S3StorageConfig {
        bucket: "bucket-that-does-not-exist".to_owned(),
        region: REGION.to_owned(),
        prefix: None,
        endpoint_url: Some(ep),
        force_path_style: Some(true),
    };
    let result = S3StorageBackend::new(&cfg).await;
    assert!(
        result.is_err(),
        "head_bucket check should fail for a missing bucket"
    );
}
