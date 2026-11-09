//! conda registry integration tests: the local and hybrid arms of the channel
//! documents and the download route, the sharded-index refusals, and the
//! publish guards (see `tests/common/mod.rs` for the shared app factories).
//!
//! The proxy-mode filtering of `repodata.json` lives in
//! `blocked_versions_hidden_conda.rs`, the index cache in `conda_index_cache.rs`.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, read_body, TestRequest};
use batlehub_adapters::registry::{CondaRegistryClient, UpstreamHttpOptions};
use batlehub_config::schema::RegistryMode;
use serde_json::Value;

/// A 64-hex shard name, the only length `conda_shard` accepts.
const SHARD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Minimal conda `.tar.bz2`: a bzip2-compressed tar holding `info/index.json`.
fn make_conda_tar_bz2(name: &str, version: &str) -> Vec<u8> {
    use bzip2::write::BzEncoder;
    use std::io::Write as _;

    let index = serde_json::to_vec(&serde_json::json!({
        "name": name,
        "version": version,
        "build": "0",
        "build_number": 0,
        "depends": [],
        "subdir": "linux-64",
    }))
    .unwrap();
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(index.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "info/index.json", index.as_slice())
            .unwrap();
        builder.finish().unwrap();
    }
    let mut enc = BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    enc.write_all(&tar_bytes).unwrap();
    enc.finish().unwrap()
}

async fn app(mode: RegistryMode) -> impl TestService {
    registry_app("local-conda", "conda", mode).await
}

/// `app(mode)` with `localpkg 1.0.0` published; returns the published bytes too.
async fn app_with_local_pkg(mode: RegistryMode) -> (impl TestService, Vec<u8>) {
    let app = app(mode).await;
    let pkg = make_conda_tar_bz2("localpkg", "1.0.0");
    let resp = call_service(&app, publish_req(pkg.clone())).await;
    assert_eq!(resp.status(), 200, "publishing the fixture package");
    (app, pkg)
}

fn publish_req(payload: Vec<u8>) -> actix_http::Request {
    TestRequest::post()
        .uri("/proxy/local-conda/linux-64/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(payload)
        .to_request()
}

async fn status<S: TestService>(app: &S, uri: &str) -> u16 {
    call_service(app, admin_get(uri)).await.status().as_u16()
}

fn package_names(doc: &Value) -> Vec<String> {
    let mut names: Vec<String> = doc["packages"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

// ── Local mode ───────────────────────────────────────────────────────────────

#[actix_web::test]
async fn local_repodata_lists_only_the_published_package() {
    let (app, _) = app_with_local_pkg(RegistryMode::Local).await;
    let doc = get_json(&app, "/proxy/local-conda/linux-64/repodata.json").await;
    assert_eq!(package_names(&doc), ["localpkg-1.0.0-0.tar.bz2"]);
}

/// A `HEAD` on a local channel has no upstream to probe, so it falls through
/// to the body path rather than 404ing.
#[actix_web::test]
async fn local_repodata_answers_a_head_probe() {
    let (app, _) = app_with_local_pkg(RegistryMode::Local).await;
    let req = TestRequest::default()
        .method(actix_web::http::Method::HEAD)
        .uri("/proxy/local-conda/linux-64/repodata.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn local_bz2_repodata_decompresses_to_the_published_package() {
    use std::io::Read as _;
    let (app, _) = app_with_local_pkg(RegistryMode::Local).await;
    let compressed = get_bytes(&app, "/proxy/local-conda/linux-64/repodata.json.bz2").await;
    let mut raw = Vec::new();
    bzip2::read::BzDecoder::new(compressed.as_slice())
        .read_to_end(&mut raw)
        .unwrap();
    let doc: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(package_names(&doc), ["localpkg-1.0.0-0.tar.bz2"]);
}

#[actix_web::test]
async fn local_current_repodata_is_not_served() {
    let app = app(RegistryMode::Local).await;
    assert_eq!(
        status(&app, "/proxy/local-conda/linux-64/current_repodata.json").await,
        404
    );
}

#[actix_web::test]
async fn local_download_returns_the_published_bytes() {
    let (app, pkg) = app_with_local_pkg(RegistryMode::Local).await;
    let body = get_bytes(&app, "/proxy/local-conda/linux-64/localpkg-1.0.0-0.tar.bz2").await;
    assert_eq!(body, pkg);
}

#[actix_web::test]
async fn local_download_of_an_unpublished_file_is_404() {
    let app = app(RegistryMode::Local).await;
    assert_eq!(
        status(&app, "/proxy/local-conda/linux-64/ghost-1.0.0-0.tar.bz2").await,
        404
    );
}

#[actix_web::test]
async fn local_registry_serves_no_shard_index() {
    let app = app(RegistryMode::Local).await;
    assert_eq!(
        status(
            &app,
            "/proxy/local-conda/linux-64/repodata_shards.msgpack.zst"
        )
        .await,
        404
    );
}

#[actix_web::test]
async fn local_registry_answers_a_shard_index_head_with_404() {
    let app = app(RegistryMode::Local).await;
    let req = TestRequest::default()
        .method(actix_web::http::Method::HEAD)
        .uri("/proxy/local-conda/linux-64/repodata_shards.msgpack.zst")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn local_registry_serves_no_shards() {
    let app = app(RegistryMode::Local).await;
    assert_eq!(
        status(
            &app,
            &format!("/proxy/local-conda/linux-64/{SHARD}.msgpack.zst")
        )
        .await,
        404
    );
}

// ── Hybrid mode ──────────────────────────────────────────────────────────────

#[actix_web::test]
async fn hybrid_repodata_merges_the_local_package_into_upstream() {
    let (app, _) = app_with_local_pkg(RegistryMode::Hybrid).await;
    let doc = get_json(&app, "/proxy/local-conda/linux-64/repodata.json").await;
    assert_eq!(
        package_names(&doc),
        [
            "localpkg-1.0.0-0.tar.bz2",
            "numpy-1.0.0-py311_0.tar.bz2",
            "numpy-1.1.0-py311_0.tar.bz2",
            "scipy-1.1.0-py311_0.tar.bz2",
        ]
    );
}

#[actix_web::test]
async fn hybrid_zst_repodata_carries_both_halves() {
    let (app, _) = app_with_local_pkg(RegistryMode::Hybrid).await;
    let compressed = get_bytes(&app, "/proxy/local-conda/linux-64/repodata.json.zst").await;
    let doc: Value =
        serde_json::from_slice(&zstd::decode_all(compressed.as_slice()).unwrap()).unwrap();
    let names = package_names(&doc);
    assert!(
        names.contains(&"localpkg-1.0.0-0.tar.bz2".to_owned())
            && names.contains(&"scipy-1.1.0-py311_0.tar.bz2".to_owned()),
        "{names:?}"
    );
}

#[actix_web::test]
async fn hybrid_current_repodata_is_the_upstream_document() {
    let app = app(RegistryMode::Hybrid).await;
    let doc = get_json(&app, "/proxy/local-conda/linux-64/current_repodata.json").await;
    assert_eq!(doc["info"]["subdir"], "linux-64");
}

#[actix_web::test]
async fn hybrid_download_prefers_the_local_package() {
    let (app, pkg) = app_with_local_pkg(RegistryMode::Hybrid).await;
    let body = get_bytes(&app, "/proxy/local-conda/linux-64/localpkg-1.0.0-0.tar.bz2").await;
    assert_eq!(body, pkg);
}

#[actix_web::test]
async fn hybrid_download_of_an_unpublished_file_falls_through_to_upstream() {
    let app = app(RegistryMode::Hybrid).await;
    let body = get_bytes(
        &app,
        "/proxy/local-conda/linux-64/numpy-1.1.0-py311_0.tar.bz2",
    )
    .await;
    let body = String::from_utf8(body).unwrap();
    assert!(body.starts_with("artifact:conda:"), "{body}");
}

// ── Proxy mode: download parsing and shards ──────────────────────────────────

#[actix_web::test]
async fn proxy_download_of_an_unparseable_filename_is_400() {
    let app = app(RegistryMode::Proxy).await;
    assert_eq!(
        status(&app, "/proxy/local-conda/linux-64/nodashes.conda").await,
        400
    );
}

#[actix_web::test]
async fn proxy_shard_is_streamed_from_upstream() {
    let app = app(RegistryMode::Proxy).await;
    let resp = call_service(
        &app,
        admin_get(&format!("/proxy/local-conda/linux-64/{SHARD}.msgpack.zst")),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(
        body.contains(SHARD),
        "the shard must come from upstream: {body}"
    );
}

#[actix_web::test]
async fn a_shard_name_that_is_not_a_sha256_is_404() {
    let app = app(RegistryMode::Proxy).await;
    assert_eq!(
        status(&app, "/proxy/local-conda/linux-64/abcdef.msgpack.zst").await,
        404
    );
}

/// Shards are unfiltered upstream bytes, so a registry with anything blocked
/// must not serve them — the client falls back to the filtered monolith.
#[actix_web::test]
async fn a_registry_with_a_block_serves_no_shards() {
    let app = app(RegistryMode::Proxy).await;
    block_version(&app, "local-conda", "numpy", "1.1.0").await;
    assert_eq!(
        status(
            &app,
            &format!("/proxy/local-conda/linux-64/{SHARD}.msgpack.zst")
        )
        .await,
        404
    );
}

/// An upstream that offers no streamed shard index answers 404, which is what
/// sends micromamba to `repodata.json`.
#[actix_web::test]
async fn proxy_shard_index_absent_upstream_is_404() {
    let app = app(RegistryMode::Proxy).await;
    assert_eq!(
        status(
            &app,
            "/proxy/local-conda/linux-64/repodata_shards.msgpack.zst"
        )
        .await,
        404
    );
}

// ── Publish guards ───────────────────────────────────────────────────────────

#[actix_web::test]
async fn publish_to_a_proxy_registry_is_404() {
    let app = app(RegistryMode::Proxy).await;
    let resp = call_service(&app, publish_req(make_conda_tar_bz2("p", "1.0.0"))).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn publish_of_a_non_conda_payload_is_422() {
    let app = app(RegistryMode::Local).await;
    let resp = call_service(&app, publish_req(b"not a conda package".to_vec())).await;
    assert_eq!(resp.status(), 422);
}

#[actix_web::test]
async fn republishing_the_same_build_is_409() {
    let (app, pkg) = app_with_local_pkg(RegistryMode::Local).await;
    let resp = call_service(&app, publish_req(pkg)).await;
    assert_eq!(resp.status(), 409);
}

#[actix_web::test]
async fn conda_routes_refuse_a_registry_of_another_kind() {
    let app = registry_app("local-npm", "npm", RegistryMode::Local).await;
    assert_eq!(
        status(&app, "/proxy/local-npm/linux-64/repodata.json").await,
        404
    );
}

// ── Byte path against a real channel (proxy mode) ────────────────────────────
//
// `FixedRegistry` has no byte path, so every streamed arm of the handler — the
// plain-index cache, the filter-as-it-decodes, the `HEAD` probe, the shard
// index — was unreachable from the rows above. These put the real
// `CondaRegistryClient` in front of a mockito channel.

/// The channel's `linux-64/repodata.json`.
fn channel_repodata() -> Value {
    serde_json::json!({
        "info": { "subdir": "linux-64" },
        "packages": {
            "numpy-1.0.0-py311_0.tar.bz2": { "name": "numpy", "version": "1.0.0" },
            "numpy-1.1.0-py311_0.tar.bz2": { "name": "numpy", "version": "1.1.0" },
            "scipy-1.1.0-py311_0.tar.bz2": { "name": "scipy", "version": "1.1.0" }
        },
        "repodata_version": 1
    })
}

/// `local-conda` in `mode`, its upstream the real conda client aimed at `upstream`.
async fn channel_app(mode: RegistryMode, upstream: &str) -> impl TestService {
    let parts = local_registry_app_parts("local-conda", "conda", mode, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            "local-conda".to_owned(),
            Arc::new(CondaRegistryClient::new(upstream, &UpstreamHttpOptions::default()).unwrap()),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

/// A channel that publishes `path` with `body` (and nothing else matched).
async fn serve(server: &mut mockito::ServerGuard, path: &str, body: Vec<u8>) -> mockito::Mock {
    server
        .mock("GET", path)
        .with_status(200)
        .with_body(body)
        .create_async()
        .await
}

async fn absent(server: &mut mockito::ServerGuard, method: &str, path: &str) {
    server
        .mock(method, path)
        .with_status(404)
        .create_async()
        .await;
}

fn header(resp: &actix_web::dev::ServiceResponse, name: &str) -> String {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn bz2_decode(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read as _;
    let mut out = Vec::new();
    bzip2::read::BzDecoder::new(bytes)
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// Nothing blocked: the plain index is relayed byte for byte, stored, and the
/// next request inside the TTL never leaves the process.
#[actix_web::test]
async fn proxy_plain_index_is_cached_and_the_second_read_is_a_hit() {
    let mut server = mockito::Server::new_async().await;
    let raw = serde_json::to_vec(&channel_repodata()).unwrap();
    let upstream = serve(&mut server, "/linux-64/repodata.json", raw.clone())
        .await
        .expect(1);
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;
    let uri = "/proxy/local-conda/linux-64/repodata.json";

    let first = call_service(&app, admin_get(uri)).await;
    assert_eq!(header(&first, "X-BatleHub-Cache"), "miss");
    assert_eq!(read_body(first).await.to_vec(), raw);

    let second = call_service(&app, admin_get(uri)).await;
    assert_eq!(header(&second, "X-BatleHub-Cache"), "hit");
    assert_eq!(read_body(second).await.to_vec(), raw);
    upstream.assert_async().await;
}

/// A `HEAD` is answered with the upstream's own `HEAD`, validators relayed.
#[actix_web::test]
async fn proxy_index_head_relays_the_upstream_validators() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("HEAD", "/linux-64/repodata.json")
        .with_status(200)
        .with_header("etag", "\"abc123\"")
        .with_header("last-modified", "Wed, 01 Jan 2025 00:00:00 GMT")
        .create_async()
        .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;

    let req = TestRequest::default()
        .method(actix_web::http::Method::HEAD)
        .uri("/proxy/local-conda/linux-64/repodata.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "etag"), "\"abc123\"");
}

#[actix_web::test]
async fn proxy_zst_index_head_is_answered_by_the_probe() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("HEAD", "/linux-64/repodata.json.zst")
        .with_status(200)
        .with_header("etag", "\"zst-tag\"")
        .create_async()
        .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;

    let req = TestRequest::default()
        .method(actix_web::http::Method::HEAD)
        .uri("/proxy/local-conda/linux-64/repodata.json.zst")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(header(&resp, "etag"), "\"zst-tag\"");
}

/// With a block in force the plain index is filtered as it decodes — here from
/// the channel's `.zst`, which the filter asks for first.
#[actix_web::test]
async fn proxy_plain_index_with_a_block_is_filtered_from_the_zst() {
    let mut server = mockito::Server::new_async().await;
    let raw = serde_json::to_vec(&channel_repodata()).unwrap();
    serve(
        &mut server,
        "/linux-64/repodata.json.zst",
        zstd::encode_all(raw.as_slice(), 3).unwrap(),
    )
    .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;
    block_version(&app, "local-conda", "numpy", "1.1.0").await;

    let resp = call_service(&app, admin_get("/proxy/local-conda/linux-64/repodata.json")).await;
    assert_eq!(header(&resp, "X-BatleHub-Cache"), "filtered");
    let doc: Value = serde_json::from_slice(&read_body(resp).await).unwrap();
    assert_eq!(
        package_names(&doc),
        ["numpy-1.0.0-py311_0.tar.bz2", "scipy-1.1.0-py311_0.tar.bz2"]
    );
}

/// A channel without a `.zst` is filtered from its plain document, and the
/// compressed route re-encodes the result in the encoding the client asked for.
#[actix_web::test]
async fn proxy_bz2_index_with_a_block_is_filtered_from_the_plain_document() {
    let mut server = mockito::Server::new_async().await;
    absent(&mut server, "GET", "/linux-64/repodata.json.zst").await;
    serve(
        &mut server,
        "/linux-64/repodata.json",
        serde_json::to_vec(&channel_repodata()).unwrap(),
    )
    .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;
    block_version(&app, "local-conda", "numpy", "1.1.0").await;

    let compressed = get_bytes(&app, "/proxy/local-conda/linux-64/repodata.json.bz2").await;
    let doc: Value = serde_json::from_slice(&bz2_decode(&compressed)).unwrap();
    assert!(
        !package_names(&doc).contains(&"numpy-1.1.0-py311_0.tar.bz2".to_owned()),
        "{doc}"
    );
}

#[actix_web::test]
async fn proxy_zst_index_with_a_block_is_filtered_and_recompressed() {
    let mut server = mockito::Server::new_async().await;
    let raw = serde_json::to_vec(&channel_repodata()).unwrap();
    serve(
        &mut server,
        "/linux-64/repodata.json.zst",
        zstd::encode_all(raw.as_slice(), 3).unwrap(),
    )
    .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;
    block_version(&app, "local-conda", "scipy", "1.1.0").await;

    let compressed = get_bytes(&app, "/proxy/local-conda/linux-64/repodata.json.zst").await;
    let doc: Value =
        serde_json::from_slice(&zstd::decode_all(compressed.as_slice()).unwrap()).unwrap();
    assert_eq!(
        package_names(&doc),
        ["numpy-1.0.0-py311_0.tar.bz2", "numpy-1.1.0-py311_0.tar.bz2"]
    );
}

/// A channel whose index is not JSON cannot be filtered, and saying so is a
/// `502` — not a channel served with the blocked package still in it.
#[actix_web::test]
async fn proxy_index_that_cannot_be_filtered_is_a_bad_gateway() {
    let mut server = mockito::Server::new_async().await;
    absent(&mut server, "GET", "/linux-64/repodata.json.zst").await;
    serve(
        &mut server,
        "/linux-64/repodata.json",
        b"{not json".to_vec(),
    )
    .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;
    block_version(&app, "local-conda", "numpy", "1.1.0").await;

    assert_eq!(
        status(&app, "/proxy/local-conda/linux-64/repodata.json.bz2").await,
        502
    );
}

/// A shard index whose base URLs are relative routes the client back here, so
/// it is relayed as the channel published it.
#[actix_web::test]
async fn proxy_relative_shard_index_is_relayed() {
    let mut server = mockito::Server::new_async().await;
    let index = zstd::encode_all(&b"\x83\xa4infoshards_base_url\xa0base_url\xa0"[..], 3).unwrap();
    serve(
        &mut server,
        "/linux-64/repodata_shards.msgpack.zst",
        index.clone(),
    )
    .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;

    let body = get_bytes(
        &app,
        "/proxy/local-conda/linux-64/repodata_shards.msgpack.zst",
    )
    .await;
    assert_eq!(body, index);
}

/// An absolute `shards_base_url` would send clients past this proxy; the index
/// is refused so they fall back to the filtered monolith.
#[actix_web::test]
async fn proxy_shard_index_naming_another_host_is_refused() {
    let mut server = mockito::Server::new_async().await;
    serve(
        &mut server,
        "/linux-64/repodata_shards.msgpack.zst",
        zstd::encode_all(&b"shards_base_url https://cdn.elsewhere.example/"[..], 3).unwrap(),
    )
    .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;

    assert_eq!(
        status(
            &app,
            "/proxy/local-conda/linux-64/repodata_shards.msgpack.zst"
        )
        .await,
        404
    );
}

/// Shards are unfiltered, so a registry with a block serves no shard index.
#[actix_web::test]
async fn proxy_shard_index_is_withheld_while_anything_is_blocked() {
    let mut server = mockito::Server::new_async().await;
    let index = zstd::encode_all(&b"base_url\xa0"[..], 3).unwrap();
    serve(&mut server, "/linux-64/repodata_shards.msgpack.zst", index).await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;
    block_version(&app, "local-conda", "numpy", "1.1.0").await;

    assert_eq!(
        status(
            &app,
            "/proxy/local-conda/linux-64/repodata_shards.msgpack.zst"
        )
        .await,
        404
    );
}

#[actix_web::test]
async fn proxy_shard_index_head_is_answered_by_the_probe() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("HEAD", "/linux-64/repodata_shards.msgpack.zst")
        .with_status(200)
        .with_header("content-length", "568")
        .create_async()
        .await;
    let app = channel_app(RegistryMode::Proxy, &server.url()).await;

    let req = TestRequest::default()
        .method(actix_web::http::Method::HEAD)
        .uri("/proxy/local-conda/linux-64/repodata_shards.msgpack.zst")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(header(&resp, "content-length"), "568");
}

/// Hybrid merge when the upstream has no `packages.conda` at all: the local
/// half's table is carried over whole rather than dropped.
#[actix_web::test]
async fn hybrid_merge_adds_a_table_the_upstream_lacks() {
    let mut server = mockito::Server::new_async().await;
    serve(
        &mut server,
        "/linux-64/repodata.json",
        serde_json::to_vec(&serde_json::json!({ "info": {}, "packages.conda": {} })).unwrap(),
    )
    .await;
    let app = channel_app(RegistryMode::Hybrid, &server.url()).await;
    assert_eq!(
        call_service(&app, publish_req(make_conda_tar_bz2("localpkg", "1.0.0")))
            .await
            .status(),
        200
    );

    let doc = get_json(&app, "/proxy/local-conda/linux-64/repodata.json").await;
    assert_eq!(package_names(&doc), ["localpkg-1.0.0-0.tar.bz2"]);
}

#[actix_web::test]
async fn proxy_download_with_an_empty_package_name_is_400() {
    let app = app(RegistryMode::Proxy).await;
    assert_eq!(
        status(&app, "/proxy/local-conda/linux-64/-1.0-0.conda").await,
        400
    );
}
