//! The compressed channel index is cached as **bytes in the storage backend**,
//! not as base64 inside a metadata-cache entry.
//!
//! `repodata.json.zst` for `conda-forge/linux-64` is 57 MiB. Held in the
//! metadata cache the way every other entry is held — a `serde_json::Value`
//! built around a base64 string — that is a 77 MiB string, a JSON document
//! around it and a serialisation of the whole thing on every write, and a
//! decode back into memory on every read. micromamba asks per subdir and per
//! encoding, so a two-subdir install paid that several times over: it was the
//! ~3.5 GB that was left after the parse itself had been taken off this path.
//!
//! Bytes now go where bytes go. The cache entry is a *pointer* — the storage
//! key plus the composition flag — and a hit streams out of storage rather than
//! being decoded first. What the tests below pin is that shape, because it is
//! invisible from the outside: both spellings serve identical bytes, and only
//! one of them is affordable.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body};
use batlehub_config::schema::RegistryMode;
use std::sync::Arc;

const REG: &str = "local-conda";
const SUBDIR: &str = "linux-64";
const URI: &str = "/proxy/local-conda/linux-64/repodata.json.zst";

/// Everything stored for this subdir, whatever the fingerprint.
const PREFIX: &str = "index/local-conda/linux-64/";

/// `GET uri` as an admin, returning the `X-BatleHub-Cache` value and the body.
async fn get_cached<S: TestService>(app: &S, uri: &str) -> (String, Vec<u8>) {
    let resp = call_service(app, admin_get(uri)).await;
    assert_eq!(resp.status(), 200, "{uri} should be served");
    let cache = resp
        .headers()
        .get("X-BatleHub-Cache")
        .expect("the index route reports its cache outcome")
        .to_str()
        .unwrap()
        .to_owned();
    (cache, read_body(resp).await.to_vec())
}

/// A miss writes the bytes to storage and the *pointer* to the cache.
#[actix_web::test]
async fn the_compressed_index_is_cached_in_storage_not_base64_in_the_cache_entry() {
    let parts = local_registry_app_parts(REG, "conda", RegistryMode::Proxy, None);
    let svc = Arc::clone(&parts.proxy_svc);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let (outcome, body) = get_cached(&app, URI).await;
    assert_eq!(outcome, "miss", "the first request cannot be a hit");
    assert!(!body.is_empty(), "the channel index is not empty");

    // The bytes are in the storage backend, under this subdir's prefix, and
    // they are the bytes the client got.
    let keys = svc.storage.list_keys(PREFIX).await.unwrap();
    assert_eq!(
        keys.len(),
        1,
        "one stored index for one subdir and one encoding, got {keys:?}"
    );
    let key = &keys[0];
    assert!(
        key.ends_with("/repodata.json.zst"),
        "the stored key names the document and its encoding: {key}"
    );
    let stored = svc
        .storage
        .retrieve(key)
        .await
        .unwrap()
        .expect("the key the cache entry points at holds the index");
    let stored = batlehub_core::ports::collect_byte_stream(stored.stream)
        .await
        .unwrap();
    assert_eq!(
        stored.as_ref(),
        body.as_slice(),
        "the stored bytes are the served bytes"
    );

    // And the cache entry is a pointer, not the payload: it names the storage
    // key and carries no encoded body of its own.
    let fingerprint = svc
        .blocked_snapshot_fingerprint(REG, batlehub_core::entities::RegistryKind::Conda)
        .await;
    let entry = svc
        .cache
        .get(&format!("repodata-zst:{REG}:{SUBDIR}:{fingerprint}"))
        .await
        .unwrap()
        .expect("the compressed index is cached in proxy mode");
    let extra = &entry.metadata.extra;
    assert_eq!(
        extra.get("storage_key").and_then(|v| v.as_str()),
        Some(key.as_str()),
        "the entry points at the stored bytes: {extra}"
    );
    assert!(
        extra.get("body_b64").is_none(),
        "the body does not live in the cache entry: {extra}"
    );
    // The size is the assertion that survives a rename of that field: a pointer
    // is a few hundred bytes whatever it is called, and a base64 body is not.
    let encoded = serde_json::to_vec(&entry.metadata).unwrap();
    assert!(
        encoded.len() < 512,
        "the cache entry is a pointer, not a copy of a {}-byte document ({} bytes)",
        body.len(),
        encoded.len()
    );
}

/// A second request is served *from storage*, byte for byte.
#[actix_web::test]
async fn a_warm_compressed_index_is_streamed_back_out_of_storage() {
    let parts = local_registry_app_parts(REG, "conda", RegistryMode::Proxy, None);
    let svc = Arc::clone(&parts.proxy_svc);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let (first, cold) = get_cached(&app, URI).await;
    assert_eq!(first, "miss");
    let (second, warm) = get_cached(&app, URI).await;
    assert_eq!(second, "hit", "the second request comes out of the cache");
    assert_eq!(warm, cold, "a hit serves the bytes that were stored");

    // A swept key is a miss, not an error: the entry says the bytes were
    // stored, storage says whether they still are.
    svc.storage.delete_by_prefix(PREFIX).await.unwrap();
    let (third, refetched) = get_cached(&app, URI).await;
    assert_eq!(third, "miss", "a dangling pointer re-fetches");
    assert_eq!(refetched, cold);
}

/// Blocking sweeps the copy filtered under the old list rather than racing it.
///
/// The fingerprint is part of the storage key, so the new answer lands beside
/// the old one unless the old one is removed — and a stale copy of a channel
/// index still naming a blocked package is exactly what RFC 0006 §4.2 forbids.
#[actix_web::test]
async fn blocking_sweeps_the_index_stored_under_the_previous_blocked_set() {
    let parts = local_registry_app_parts(REG, "conda", RegistryMode::Proxy, None);
    let svc = Arc::clone(&parts.proxy_svc);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let (_, before) = get_cached(&app, URI).await;
    let stored_before = svc.storage.list_keys(PREFIX).await.unwrap();
    assert_eq!(stored_before.len(), 1);

    block_version(&app, REG, "numpy", "1.1.0").await;
    // The read above warmed the 30-second blocked-set snapshot with an empty
    // set, and a block does not reach a warm one — that window is RFC 0006's
    // and is pinned elsewhere. Expiring it here is what the clock would do,
    // and it is the only way to have both fingerprints inside one app.
    svc.cache
        .invalidate(&format!("blocks:{REG}"))
        .await
        .unwrap();

    let (_, after) = get_cached(&app, URI).await;
    assert_ne!(
        after, before,
        "the filtered channel is not the unfiltered one"
    );
    let stored_after = svc.storage.list_keys(PREFIX).await.unwrap();
    assert_eq!(
        stored_after.len(),
        1,
        "the copy filtered under the old list is swept, not left beside the new one: {stored_after:?}"
    );
    assert_ne!(
        stored_after[0], stored_before[0],
        "a different blocked set is a different key"
    );
}
