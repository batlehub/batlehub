//! Integration tests for `StorageRouter`.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   task test:pg-cache                              # starts Postgres via Podman automatically
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test storage_router

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use sqlx::PgPool;

use batlehub_adapters::storage::{FilesystemStorageBackend, StorageRouter};
use batlehub_core::ports::{StorageBackend, StorageMeta, StoredArtifact};
use sha2::{Digest, Sha256};

fn content_key(data: &[u8]) -> String {
    format!("blob/{}", hex::encode(Sha256::digest(data)))
}

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

// Returns a unique key safe to use across parallel tests AND across test runs.
// PID is included so keys from different process invocations never collide in
// the shared dedup tables.
fn ukey(registry: &str, name: &str) -> String {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("artifact:{registry}/p{pid}-t{id}-{name}")
}

// Returns payload bytes that are unique per process invocation.
// Embedding the PID guarantees a fresh content hash on every test run, so the
// dedup system always writes a new physical blob (ref_count == 1) rather than
// reusing a blob from a previous run that no longer exists on disk.
//
// **`label` must also be unique per test.** Two tests passing the same label
// produce the same bytes, hence the same content hash, hence one shared row in
// the dedup table — which records the backend by *name*. Every test here names
// its filesystem backend "default" while pointing it at its own directory, so
// the second test's `exists`/`retrieve` resolves the blob to whichever
// directory won the race to store it and looks for it in its own. That made
// `delete_by_prefix_treats_percent_as_literal_not_wildcard` fail only when the
// full suite ran in parallel and only sometimes.
fn upayload(label: &str) -> Bytes {
    Bytes::from(format!("{label}-pid{}", std::process::id()))
}

async fn make_fs(label: &str) -> Arc<FilesystemStorageBackend> {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("batlehub-router-{label}-{pid}-{id}"));
    Arc::new(FilesystemStorageBackend::new(dir).await.unwrap())
}

async fn pool(url: &str) -> PgPool {
    let p = PgPool::connect(url).await.expect("connect to postgres");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&p)
        .await
        .expect("run migrations");
    p
}

async fn collect(artifact: StoredArtifact) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut stream = artifact.stream;
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.unwrap());
    }
    buf
}

fn single_backend_router(fs: Arc<FilesystemStorageBackend>, pool: PgPool) -> StorageRouter {
    let mut backends: HashMap<String, Arc<dyn StorageBackend>> = HashMap::new();
    backends.insert("default".to_owned(), fs);
    StorageRouter::new(backends, "default".to_owned(), HashMap::new(), pool)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn store_and_retrieve_round_trip() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("rr").await, pool(&url).await);
    let key = ukey("npm", "test-pkg");

    let data = upayload("hello-router");
    router
        .store(&key, data.clone(), StorageMeta::default())
        .await
        .unwrap();
    let artifact = router
        .retrieve(&key)
        .await
        .unwrap()
        .expect("should be found");
    assert_eq!(collect(artifact).await, data.as_ref());
}

// Re-storing different bytes under the same logical key must re-point
// artifact_dedup_refs to the new content hash and drop the old
// artifact_dedup_index row (when its ref_count reaches zero) without
// tripping the artifact_dedup_refs_content_hash_fkey constraint.
#[tokio::test]
async fn store_overwrite_with_different_content_repoints_dedup_entry() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("overwrite").await, pool(&url).await);
    let key = ukey("npm", "test-pkg");

    let first = upayload("first-version");
    router
        .store(&key, first.clone(), StorageMeta::default())
        .await
        .unwrap();

    let second = upayload("second-version-with-different-bytes");
    router
        .store(&key, second.clone(), StorageMeta::default())
        .await
        .unwrap();

    let artifact = router
        .retrieve(&key)
        .await
        .unwrap()
        .expect("should be found");
    assert_eq!(collect(artifact).await, second.as_ref());
}

#[tokio::test]
async fn retrieve_missing_key_returns_none() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("miss").await, pool(&url).await);
    let key = ukey("npm", "no-such-pkg");
    assert!(router.retrieve(&key).await.unwrap().is_none());
}

#[tokio::test]
async fn exists_before_and_after_store() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("ex").await, pool(&url).await);
    let key = ukey("npm", "ex-pkg");

    assert!(!router.exists(&key).await.unwrap());
    router
        .store(&key, upayload("exists-data"), StorageMeta::default())
        .await
        .unwrap();
    assert!(router.exists(&key).await.unwrap());
}

#[tokio::test]
async fn delete_removes_artifact() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("del").await, pool(&url).await);
    let key = ukey("npm", "del-pkg");

    router
        .store(&key, upayload("bye"), StorageMeta::default())
        .await
        .unwrap();
    router.delete(&key).await.unwrap();
    assert!(!router.exists(&key).await.unwrap());
}

#[tokio::test]
async fn delete_missing_key_is_ok() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("ghost").await, pool(&url).await);
    let key = ukey("npm", "ghost");
    router.delete(&key).await.unwrap();
}

#[tokio::test]
async fn retrieve_falls_back_to_default_when_no_artifact_storage_record() {
    let Some(url) = db_url() else { return };
    let fs = make_fs("fallback").await;
    let key = ukey("npm", "direct-pkg");

    // Write directly to the FS backend — no artifact_storage record created.
    fs.store(&key, Bytes::from_static(b"direct"), StorageMeta::default())
        .await
        .unwrap();

    let router = single_backend_router(fs, pool(&url).await);
    // Router must fall back to resolve_backend_for_key and find the file.
    let artifact = router
        .retrieve(&key)
        .await
        .unwrap()
        .expect("fallback must succeed");
    assert_eq!(collect(artifact).await, b"direct");
}

#[tokio::test]
async fn routes_to_correct_named_backend() {
    let Some(url) = db_url() else { return };
    let fs_a = make_fs("named-a").await;
    let fs_b = make_fs("named-b").await;

    let npm_key = ukey("npm", "pkg");
    let cargo_key = ukey("cargo", "crate");

    let mut backends: HashMap<String, Arc<dyn StorageBackend>> = HashMap::new();
    backends.insert("backend-a".to_owned(), fs_a.clone());
    backends.insert("backend-b".to_owned(), fs_b.clone());

    let mut assignments = HashMap::new();
    assignments.insert("npm".to_owned(), "backend-a".to_owned());
    assignments.insert("cargo".to_owned(), "backend-b".to_owned());

    let router = StorageRouter::new(
        backends,
        "backend-a".to_owned(),
        assignments,
        pool(&url).await,
    );

    let npm_data = upayload("npm-data");
    let cargo_data = upayload("cargo-data");
    router
        .store(&npm_key, npm_data.clone(), StorageMeta::default())
        .await
        .unwrap();
    router
        .store(&cargo_key, cargo_data.clone(), StorageMeta::default())
        .await
        .unwrap();

    // Router stores blobs content-addressed as "blob/{sha256}", not under the logical key.
    let npm_blob = content_key(&npm_data);
    let cargo_blob = content_key(&cargo_data);

    assert!(
        fs_a.exists(&npm_blob).await.unwrap(),
        "npm artifact must land in backend-a"
    );
    assert!(
        !fs_b.exists(&npm_blob).await.unwrap(),
        "npm artifact must NOT be in backend-b"
    );
    assert!(
        fs_b.exists(&cargo_blob).await.unwrap(),
        "cargo artifact must land in backend-b"
    );
    assert!(
        !fs_a.exists(&cargo_blob).await.unwrap(),
        "cargo artifact must NOT be in backend-a"
    );
}

#[tokio::test]
async fn unknown_registry_uses_default_backend() {
    let Some(url) = db_url() else { return };
    let fs_default = make_fs("unk-default").await;
    let fs_other = make_fs("unk-other").await;

    let key = ukey("pypi", "some-package");

    let mut backends: HashMap<String, Arc<dyn StorageBackend>> = HashMap::new();
    backends.insert("default".to_owned(), fs_default.clone());
    backends.insert("other".to_owned(), fs_other.clone());

    // Only "npm" is assigned; "pypi" has no assignment → must fall back to default.
    let mut assignments = HashMap::new();
    assignments.insert("npm".to_owned(), "other".to_owned());

    let router = StorageRouter::new(
        backends,
        "default".to_owned(),
        assignments,
        pool(&url).await,
    );
    let pypi_data = upayload("pypi-data");
    router
        .store(&key, pypi_data.clone(), StorageMeta::default())
        .await
        .unwrap();

    // Router stores blobs content-addressed as "blob/{sha256}", not under the logical key.
    let blob = content_key(&pypi_data);
    assert!(
        fs_default.exists(&blob).await.unwrap(),
        "unassigned registry must use default backend"
    );
    assert!(!fs_other.exists(&blob).await.unwrap());
}

// Regression: re-storing identical bytes after the physical blob was deleted
// (e.g. `./storage` wiped without resetting the DB) must restore the blob and
// make `retrieve` return the content — not a "vanished immediately after caching"
// 502 error.
#[tokio::test]
async fn restore_after_physical_blob_deleted() {
    let Some(url) = db_url() else { return };
    let fs = make_fs("restore").await;
    let key = ukey("jetbrains", "idea");

    let data = upayload("idea-archive");
    let blob = content_key(&data);

    // First store: sets up dedup tables + physical blob.
    let router = single_backend_router(fs.clone(), pool(&url).await);
    router
        .store(&key, data.clone(), StorageMeta::default())
        .await
        .unwrap();
    assert!(router.exists(&key).await.unwrap());

    // Simulate clearing the storage directory without resetting the DB.
    fs.delete(&blob).await.unwrap();
    assert!(
        !router.exists(&key).await.unwrap(),
        "blob gone → exists false"
    );

    // Re-store the same bytes (same hash). Must restore the physical blob.
    router
        .store(&key, data.clone(), StorageMeta::default())
        .await
        .unwrap();

    // retrieve must now succeed — no "vanished immediately after caching" error.
    let artifact = router
        .retrieve(&key)
        .await
        .unwrap()
        .expect("blob must be retrievable after re-store");
    assert_eq!(collect(artifact).await, data.as_ref());
}

// Regression: a *different* logical key storing bytes whose hash is already in
// the dedup index, after the physical blob went missing. `reuse_identical_blob`
// covers the same-key case; this is the other one, and it was not covered — the
// dedup hit skipped materialisation on the strength of `ref_count > 1`, so
// `store_streaming` reported success having written nothing and the caller's
// `retrieve` came back empty. The proxy turns that into
// "staged artifact '…' vanished before promotion" → 502, permanently, because
// every later fetch hits the same dangling row.
//
// Found by tests/heavy/npm.sh: its storage directory is per-run and the dedup
// index in Postgres is not, which is the same shape as an operator recreating
// a cache bucket.
#[tokio::test]
async fn dedup_hit_restores_a_missing_blob_for_a_new_key() {
    let Some(url) = db_url() else { return };
    let fs = make_fs("dedup-restore").await;
    let router = single_backend_router(fs.clone(), pool(&url).await);

    let data = upayload("shared-bytes");
    let blob = content_key(&data);
    let first = ukey("npm", "first");
    let second = ukey("npm", "second");

    router
        .store(&first, data.clone(), StorageMeta::default())
        .await
        .unwrap();
    assert!(fs.exists(&blob).await.unwrap());

    // The bucket is recreated / the cache directory is wiped; the DB is not.
    fs.delete(&blob).await.unwrap();

    // A second key now stores the same bytes. This is a dedup *hit*: the index
    // says the blob exists. It does not.
    router
        .store(&second, data.clone(), StorageMeta::default())
        .await
        .unwrap();

    let artifact = router
        .retrieve(&second)
        .await
        .unwrap()
        .expect("a dedup hit must not report success for bytes it did not write");
    assert_eq!(collect(artifact).await, data.as_ref());

    // ...and the key that was there first is readable again too, because there
    // is only ever one physical blob behind both of them.
    let artifact = router
        .retrieve(&first)
        .await
        .unwrap()
        .expect("blob restored");
    assert_eq!(collect(artifact).await, data.as_ref());
}

// A literal `%` in a prefix must be treated as a literal character, not a SQL
// wildcard. Without escaping, `LIKE 'base-50%%'` (prefix's own `%` plus the
// trailing wildcard the router appends) degrades to "starts with base-50",
// matching keys that never contained the literal `%` at all.
#[tokio::test]
async fn stat_by_prefix_treats_percent_as_literal_not_wildcard() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("pct-stat").await, pool(&url).await);
    let base = ukey("npm", "orig");

    // Contains a literal '%' right after "-50" — must match.
    let matching_key = format!("{base}-50%-real");
    // Starts with "-50" but the next character is '1', not '%' — must NOT
    // match a correctly-escaped `LIKE '{base}-50%%'` (prefix `%` escaped to
    // literal, trailing `%` as the only wildcard).
    let decoy_key = format!("{base}-501-other");

    router
        .store(
            &matching_key,
            upayload("stat-match"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    router
        .store(&decoy_key, upayload("stat-decoy"), StorageMeta::default())
        .await
        .unwrap();

    let prefix = format!("{base}-50%");
    let (count, _total) = router.stat_by_prefix(&prefix).await.unwrap();
    assert_eq!(
        count, 1,
        "only the key containing the literal '%' should match"
    );
}

#[tokio::test]
async fn delete_by_prefix_treats_percent_as_literal_not_wildcard() {
    let Some(url) = db_url() else { return };
    let router = single_backend_router(make_fs("pct-del").await, pool(&url).await);
    let base = ukey("npm", "orig");

    let matching_key = format!("{base}-50%-real");
    let decoy_key = format!("{base}-501-other");

    router
        .store(&matching_key, upayload("del-match"), StorageMeta::default())
        .await
        .unwrap();
    router
        .store(&decoy_key, upayload("del-decoy"), StorageMeta::default())
        .await
        .unwrap();

    let prefix = format!("{base}-50%");
    let deleted = router.delete_by_prefix(&prefix).await.unwrap();
    assert_eq!(deleted, 1, "only the literal '%' key should be deleted");
    assert!(
        !router.exists(&matching_key).await.unwrap(),
        "matching key must be gone"
    );
    assert!(
        router.exists(&decoy_key).await.unwrap(),
        "decoy key must survive an escaped prefix delete"
    );
}

fn byte_stream(data: Bytes) -> batlehub_core::ports::ByteStream {
    Box::pin(futures::stream::iter([Ok(data)]))
}

async fn tracked_rows(pool: &PgPool, key: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM artifact_dedup_refs WHERE logical_key = $1) \
              + (SELECT COUNT(*) FROM artifact_storage WHERE storage_key = $1)",
    )
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The proxy's verify-before-serve path: bytes are staged, checked, then
/// promoted. A staging key never enters the dedup tables, and promotion moves
/// the staged blob into place rather than writing the artifact a second time.
#[tokio::test]
async fn staged_bytes_stay_out_of_dedup_and_promote_by_rename() {
    let Some(url) = db_url() else { return };
    let fs = make_fs("staging").await;
    let pool = pool(&url).await;
    let router = single_backend_router(fs.clone(), pool.clone());

    let data = upayload("staged-bytes");
    let key = ukey("npm", "staged");
    let staged = batlehub_core::ports::staging_key_for(&key);

    let outcome = router
        .store_streaming(&staged, byte_stream(data.clone()), StorageMeta::default())
        .await
        .unwrap();
    assert_eq!(
        tracked_rows(&pool, &staged).await,
        0,
        "staging is not tracked"
    );
    assert!(
        fs.exists(&staged).await.unwrap(),
        "staged on the leaf as-is"
    );
    assert!(router.exists(&staged).await.unwrap());
    assert!(
        !router.exists(&key).await.unwrap(),
        "not servable before promotion"
    );

    router.promote(&staged, &key, &outcome).await.unwrap();
    assert!(
        !fs.exists(&staged).await.unwrap(),
        "renamed away, not left behind"
    );
    assert!(fs.exists(&content_key(&data)).await.unwrap());
    assert_eq!(
        tracked_rows(&pool, &key).await,
        2,
        "one ref, one storage row"
    );
    let artifact = router.retrieve(&key).await.unwrap().expect("promoted");
    assert_eq!(collect(artifact).await, data.as_ref());

    // Same bytes staged for a second key: a dedup hit, the staged copy dropped.
    let second = ukey("npm", "staged-again");
    let staged = batlehub_core::ports::staging_key_for(&second);
    let outcome = router
        .store_streaming(&staged, byte_stream(data.clone()), StorageMeta::default())
        .await
        .unwrap();
    router.promote(&staged, &second, &outcome).await.unwrap();
    assert!(!fs.exists(&staged).await.unwrap());
    let artifact = router.retrieve(&second).await.unwrap().expect("promoted");
    assert_eq!(collect(artifact).await, data.as_ref());

    // A refused artifact is evicted by deleting its staging key: nothing tracked.
    let refused = batlehub_core::ports::staging_key_for(&ukey("npm", "refused"));
    router
        .store_streaming(
            &refused,
            byte_stream(upayload("refused-bytes")),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    assert!(router.delete(&refused).await.unwrap());
    assert!(!fs.exists(&refused).await.unwrap());
}
