//! The `nodejs.org/dist` tree as a typed registry (RFC 0010, phases 2–3).
//!
//! What nvm does, and what this proxy therefore has to get right:
//!
//! - it resolves **every** install through `index.tab`, strips line 1 with
//!   `sed 1d`, and reports *"Version 'x' not found"* when the row is absent —
//!   so a blocked release must be gone from the table and the header must
//!   survive, or the newest release is silently eaten instead of the header;
//! - it verifies every download against `SHASUMS256.txt`, which a sibling
//!   `.asc`/`.sig` signs — so that file has to reach the client byte-exact;
//! - fnm and mise read `index.json` for the same decision — so the JSON has
//!   to say the same thing as the TSV.
//!
//! The fixture is `FixedRegistry`'s three-version table (see
//! `tests/common/mod.rs`); the age-gate tests below use the real
//! `NodeDistRegistryClient` against a mock tree, because `published_at` read
//! from `index.tab` is the client's job and a fixture that faked it would prove
//! nothing about it.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, TestRequest};
use batlehub_adapters::registry::NodeDistRegistryClient;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    ports::RegistryClient,
    rules::{BlockListRule, ReleaseAgeGateRule},
    services::{proxy::proxy_artifact_key, RegistryPolicy},
};

const REG: &str = "node";

async fn app() -> impl TestService {
    proxy_registry_app(REG, "nodedist").await
}

fn index_tab_url() -> String {
    format!("/proxy/{REG}/nodedist/index.tab")
}

fn file_url(version: &str, file: &str) -> String {
    format!("/proxy/{REG}/nodedist/{version}/{file}")
}

/// The versions named by an `index.tab` body, header excluded.
fn versions_in(tab: &str) -> Vec<String> {
    tab.lines()
        .skip(1)
        .filter_map(|l| l.split('\t').next())
        .map(str::to_owned)
        .collect()
}

// ── the listing is the enforcement point ─────────────────────────────────────

#[actix_web::test]
async fn index_tab_lists_every_release_with_its_header() {
    let app = app().await;
    let tab = get_text(&app, &index_tab_url()).await;
    assert!(tab.starts_with("version\tdate\tfiles\t"), "{tab:?}");
    assert_eq!(versions_in(&tab), ["v2.0.0-beta.1", "v1.1.0", "v1.0.0"]);
}

/// The case that matters: nvm strips line 1 unconditionally, so a filter that
/// consumed the header would make nvm lose the newest release instead.
#[actix_web::test]
async fn index_tab_hides_a_blocked_release_and_keeps_the_header() {
    let app = app().await;
    block_version(&app, REG, "node", "v1.1.0").await;

    let tab = get_text(&app, &index_tab_url()).await;
    assert!(tab.starts_with("version\tdate\tfiles\t"), "{tab:?}");
    assert_eq!(versions_in(&tab), ["v2.0.0-beta.1", "v1.0.0"]);
    assert!(
        tab.contains("v1.0.0\t2020-01-02\theaders,linux-x64,src\t6.13.0"),
        "the surviving rows are byte-identical: {tab:?}"
    );
}

/// `nvm install 22.11.0` is the documented spelling and the one an operator
/// copies into a block; the tree spells `v22.11.0`.
#[actix_web::test]
async fn a_block_recorded_without_the_v_prefix_matches_the_listing() {
    let app = app().await;
    block_version(&app, REG, "node", "1.1.0").await;
    let tab = get_text(&app, &index_tab_url()).await;
    assert!(!tab.contains("v1.1.0"), "{tab:?}");
}

/// Blocking the newest LTS release moves the alias nvm derives from the `lts`
/// column: the first surviving `Argon` row is now `v1.0.0`. Nothing computes
/// that — removal alone is enough, which is the one place this design gets
/// something for free (RFC 0010 §6.2).
#[actix_web::test]
async fn blocking_the_newest_lts_release_moves_the_alias_nvm_derives() {
    let app = app().await;
    block_version(&app, REG, "node", "v1.1.0").await;

    let tab = get_text(&app, &index_tab_url()).await;
    let first_argon = tab
        .lines()
        .skip(1)
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .find(|cols| cols.get(9) == Some(&"Argon"))
        .map(|cols| cols[0].to_owned());
    assert_eq!(first_argon.as_deref(), Some("v1.0.0"));
}

/// Two encodings of one document: a block reaches both, or fnm and mise get an
/// unfiltered answer to the question nvm was refused.
#[actix_web::test]
async fn index_json_hides_the_same_release() {
    let app = app().await;
    let url = format!("/proxy/{REG}/nodedist/index.json");

    let before = get_json(&app, &url).await;
    assert_eq!(before.as_array().map(Vec::len), Some(3));

    block_version(&app, REG, "node", "v1.1.0").await;

    let after = get_json(&app, &url).await;
    let versions: Vec<&str> = after
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["v2.0.0-beta.1", "v1.0.0"]);
    assert_eq!(after[1]["lts"], "Argon", "surviving entries are untouched");
}

#[actix_web::test]
async fn the_listings_answer_in_their_own_content_types() {
    let app = app().await;
    assert_content_type(&app, &index_tab_url(), "text/plain").await;
    assert_content_type(
        &app,
        &format!("/proxy/{REG}/nodedist/index.json"),
        "application/json",
    )
    .await;
}

// ── the files ────────────────────────────────────────────────────────────────

/// Hiding governs resolution, not diagnosis: a request for a blocked release by
/// exact name — from an `.nvmrc`, a cached LTS alias, or memory — gets the
/// operator's `403`, and upstream is never asked.
#[actix_web::test]
async fn a_direct_request_for_a_blocked_release_is_denied() {
    let app = app().await;
    block_version(&app, REG, "node", "v1.1.0").await;

    let resp = call_service(
        &app,
        admin_get(&file_url("v1.1.0", "node-v1.1.0-linux-x64.tar.xz")),
    )
    .await;
    assert_eq!(resp.status(), 403);

    let resp = call_service(&app, admin_get(&file_url("v1.1.0", "SHASUMS256.txt"))).await;
    assert_eq!(
        resp.status(),
        403,
        "the checksum file is part of the release and blocked with it"
    );
}

/// `SHASUMS256.txt` is what nvm verifies against and what a sibling
/// `.asc`/`.sig` signs; it reaches the client exactly as upstream sent it.
#[actix_web::test]
async fn shasums_are_served_byte_identical_and_as_text() {
    let app = app().await;
    let url = file_url("v1.1.0", "SHASUMS256.txt");

    let resp = call_service(&app, admin_get(&url)).await;
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ct.starts_with("text/plain"), "{ct}");
    let body = actix_web::test::read_body(resp).await;
    // `FixedRegistry` answers every artifact with its own coordinate, so the
    // bytes name the key this file was fetched and cached under.
    assert_eq!(body, "artifact:nodedist:node/node/v1.1.0/SHASUMS256.txt");
}

#[actix_web::test]
async fn a_tarball_is_served_as_bytes_under_its_release_coordinate() {
    let app = app().await;
    let url = file_url("v1.1.0", "node-v1.1.0-linux-x64.tar.xz");
    assert_content_type(&app, &url, "application/octet-stream").await;
    assert_eq!(
        get_text(&app, &url).await,
        "artifact:nodedist:node/node/v1.1.0/node-v1.1.0-linux-x64.tar.xz"
    );
}

/// The second build agent to ask for a release does not leave the site: the
/// first request writes the artifact under its coordinate, and it is there for
/// the next.
#[actix_web::test]
async fn a_release_file_is_cached_under_a_stable_per_release_key() {
    let parts = local_registry_app_parts(REG, "nodedist", RegistryMode::Proxy, None);
    let storage = Arc::clone(&parts.proxy_svc.storage);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let pkg = batlehub_core::entities::PackageId::new(REG, "node", "v1.1.0")
        .with_artifact("node-v1.1.0-linux-x64.tar.xz");
    let key = proxy_artifact_key(&pkg);
    assert!(!storage.exists(&key).await.unwrap());

    get_bytes(&app, &file_url("v1.1.0", "node-v1.1.0-linux-x64.tar.xz")).await;
    assert!(
        storage.exists(&key).await.unwrap(),
        "the first read must cache {key}"
    );
    get_bytes(&app, &file_url("v1.1.0", "node-v1.1.0-linux-x64.tar.xz")).await;
}

// ── edge validation ──────────────────────────────────────────────────────────

/// Both segments reach a storage key, so both are refused at the edge with a
/// clean `400` — the regression every registry kind carries.
#[actix_web::test]
async fn nodedist_file_traversal_returns_400() {
    let app = app().await;
    for url in [
        format!("/proxy/{REG}/nodedist/../SHASUMS256.txt"),
        format!("/proxy/{REG}/nodedist/v1.1.0/.."),
        format!("/proxy/{REG}/nodedist/%2e%2e/SHASUMS256.txt"),
        format!("/proxy/{REG}/nodedist/v1.1.0/%2e%2e"),
        format!("/proxy/{REG}/nodedist/v1.1.0/..%2Fetc%2Fpasswd"),
    ] {
        let resp = call_service(&app, admin_get(&url)).await;
        assert!(
            matches!(resp.status().as_u16(), 400 | 404),
            "{url} answered {}",
            resp.status()
        );
        assert_ne!(resp.status(), 200, "{url} must never be served");
    }
    // The unambiguous case: a version segment that is a traversal after
    // decoding reaches the handler and is refused there, not deeper.
    let resp = call_service(
        &app,
        admin_get(&format!("/proxy/{REG}/nodedist/v1.1.0/..%2Fx")),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn the_routes_refuse_a_registry_of_another_kind() {
    let app = proxy_registry_app("npm-reg", "npm").await;
    let resp = call_service(&app, admin_get("/proxy/npm-reg/nodedist/index.tab")).await;
    assert_eq!(resp.status(), 404);
}

// ── the age gate, against the real client ────────────────────────────────────

/// An app whose `node` registry is the real `NodeDistRegistryClient` pointed
/// at `base`, behind an age gate with `deny_missing_timestamp` as given.
///
/// `min_age = 0`: a release with *any* date passes, so the only thing the gate
/// decides here is what it does with a release that has none.
async fn app_against(base: String, deny_missing: bool) -> impl TestService {
    let parts = local_registry_app_parts(REG, "nodedist", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(NodeDistRegistryClient::new(base, &Default::default()).unwrap())
                as Arc<dyn RegistryClient>,
        );
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![
                    Box::new(BlockListRule::new(Arc::clone(&parts.proxy_svc.repo))),
                    Box::new(
                        ReleaseAgeGateRule::new(Duration::from_secs(0), vec![])
                            .with_deny_missing_timestamp(deny_missing),
                    ),
                ],
            }),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

const MOCK_INDEX_TAB: &str =
    "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n\
    v22.11.0\t2024-10-29\theaders,linux-x64,src\t10.9.0\t12.4\t1.49.1\t1.3\t3.0.15\t127\tJod\t-\n";

/// A listed release carries the date `index.tab` gives it, and passes.
#[actix_web::test]
async fn a_listed_release_carries_the_date_from_index_tab_and_passes_the_gate() {
    let mut server = mockito::Server::new_async().await;
    let _index = server
        .mock("GET", "/index.tab")
        .with_body(MOCK_INDEX_TAB)
        .create_async()
        .await;
    let _file = server
        .mock("GET", "/v22.11.0/SHASUMS256.txt")
        .with_body("abc  node-v22.11.0-linux-x64.tar.xz\n")
        .create_async()
        .await;

    let app = app_against(server.url(), true).await;
    let resp = call_service(&app, admin_get(&file_url("v22.11.0", "SHASUMS256.txt"))).await;
    assert_eq!(resp.status(), 200);
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(body, "abc  node-v22.11.0-linux-x64.tar.xz\n");
}

/// A release the index no longer lists reaches the gate with no date, and the
/// operator's `deny_missing_timestamp` decides: `true` refuses it.
#[actix_web::test]
async fn a_delisted_release_is_refused_when_missing_timestamps_deny() {
    let mut server = mockito::Server::new_async().await;
    let _index = server
        .mock("GET", "/index.tab")
        .with_body(MOCK_INDEX_TAB)
        .create_async()
        .await;
    let _head = server
        .mock("HEAD", "/v0.10.48/SHASUMS256.txt")
        .with_status(200)
        .create_async()
        .await;
    let file = server
        .mock("GET", "/v0.10.48/SHASUMS256.txt")
        .with_body("old\n")
        .expect(0)
        .create_async()
        .await;

    let app = app_against(server.url(), true).await;
    let resp = call_service(&app, admin_get(&file_url("v0.10.48", "SHASUMS256.txt"))).await;
    assert_eq!(resp.status(), 403);
    file.assert_async().await;
}

/// …and `false` serves it, which is the other legitimate posture.
#[actix_web::test]
async fn a_delisted_release_is_served_when_missing_timestamps_are_exempt() {
    let mut server = mockito::Server::new_async().await;
    let _index = server
        .mock("GET", "/index.tab")
        .with_body(MOCK_INDEX_TAB)
        .create_async()
        .await;
    let _head = server
        .mock("HEAD", "/v0.10.48/SHASUMS256.txt")
        .with_status(200)
        .create_async()
        .await;
    let _file = server
        .mock("GET", "/v0.10.48/SHASUMS256.txt")
        .with_body("old\n")
        .create_async()
        .await;

    let app = app_against(server.url(), false).await;
    let resp = call_service(&app, admin_get(&file_url("v0.10.48", "SHASUMS256.txt"))).await;
    assert_eq!(resp.status(), 200);
}

/// A release that exists in neither the index nor the tree is a `404`, not a
/// gate decision.
#[actix_web::test]
async fn a_release_the_tree_does_not_have_is_not_found() {
    let mut server = mockito::Server::new_async().await;
    let _index = server
        .mock("GET", "/index.tab")
        .with_body(MOCK_INDEX_TAB)
        .create_async()
        .await;
    let _head = server
        .mock("HEAD", "/v99.0.0/SHASUMS256.txt")
        .with_status(404)
        .create_async()
        .await;

    let app = app_against(server.url(), false).await;
    let resp = call_service(&app, admin_get(&file_url("v99.0.0", "SHASUMS256.txt"))).await;
    assert_eq!(resp.status(), 404);
}

/// The proxied listing is the upstream's own bytes, filtered — so a real
/// `index.tab` round-trips through the real client unchanged when nothing is
/// blocked.
#[actix_web::test]
async fn the_real_client_serves_index_tab_as_upstream_sent_it() {
    let mut server = mockito::Server::new_async().await;
    let _index = server
        .mock("GET", "/index.tab")
        .with_body(MOCK_INDEX_TAB)
        .create_async()
        .await;
    let app = app_against(server.url(), false).await;
    let req = TestRequest::get()
        .uri(&index_tab_url())
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(body, MOCK_INDEX_TAB);
}
