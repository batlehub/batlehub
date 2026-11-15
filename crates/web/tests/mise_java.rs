//! mise's JVM catalogue as a typed registry (RFC 0048 §6.3, §6.4, §14).
//!
//! What mise 2026.11.3's `core:java` does (`src/plugins/core/java.rs`), and
//! what this proxy therefore owes it:
//!
//! - it reads `jvm/{type}/{os}/{arch}.json` and installs the row whose key
//!   (`temurin-21.0.5+11.0.LTS`) it was asked for (`:375-395`, `:767`) — so a
//!   blocked build must leave that document;
//! - it downloads the row's own URL (`:207-234`), and with a lock it skips the
//!   document and downloads the locked URL directly (`:615-634`) — so the
//!   download route must refuse a blocked build by itself (T4), and must never
//!   fetch a URL no document named (T3).
//!
//! The registry is the real `MiseJavaRegistryClient` against a mock serving
//! the documents and the vendor hosts (`with_test_host`), because "a URL is a
//! lookup key, never a fetch target" and "every hop is checked" are the
//! client's job, and a fixture would prove neither.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::test::call_service;
use batlehub_adapters::registry::{MiseJavaOptions, MiseJavaRegistryClient};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    ports::RegistryClient,
    rules::{BlockListRule, ReleaseAgeGateRule, Rule},
    services::RegistryPolicy,
};
use sha2::{Digest, Sha256};

const REG: &str = "jdk";
const ARCHIVE: &[u8] = b"not really a JDK";
const TEMURIN: &str = "temurin-21.0.5+11.0.LTS";
/// A row URL as mise-java spells it — `%2B` and all.
const ADOPTIUM: &str = "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/OpenJDK21U-jdk_x64_linux_hotspot_21.0.5_11.tar.gz";
const ZULU: &str = "https://cdn.azul.com/zulu/bin/zulu21.38.21-ca-jdk21.0.5-linux_x64.tar.gz";

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn row(vendor: &str, version: &str, url: &str, checksum: &str) -> serde_json::Value {
    serde_json::json!({"checksum": checksum, "created_at": "2025-03-28T22:04:32.217430",
        "features": [], "file_type": "tar.gz", "image_type": "jdk",
        "java_version": "21.0.5", "jvm_impl": "hotspot", "url": url,
        "vendor": vendor, "version": version})
}

fn rows(adoptium_checksum: &str) -> Vec<serde_json::Value> {
    vec![
        row("temurin", "21.0.5+11.0.LTS", ADOPTIUM, adoptium_checksum),
        row(
            "zulu",
            "21.38.21",
            ZULU,
            "md5:0123456789abcdef0123456789abcdef",
        ),
        // Another GitHub owner, and a host nobody listed.
        row(
            "evil",
            "1",
            "https://github.com/someone/jdk/releases/download/1/x.tar.gz",
            "",
        ),
        row("meta", "1", "https://169.254.169.254/latest/meta-data", ""),
    ]
}

/// A mock serving the GA Linux x86_64 document, `404` for the others.
async fn upstream(rows: Vec<serde_json::Value>) -> mockito::ServerGuard {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/jvm/ga/linux/x86_64.json")
        .with_body(serde_json::Value::Array(rows).to_string())
        // mockito answers with the first matching mock still short of its
        // expected hits, else the last: a ceiling it never reaches keeps the
        // document ahead of the catch-all below on every request.
        .expect_at_most(usize::MAX)
        .create_async()
        .await;
    server
        .mock("GET", mockito::Matcher::Regex("^/jvm/".to_owned()))
        .with_status(404)
        .create_async()
        .await;
    server
}

fn client(server: &mockito::ServerGuard) -> MiseJavaRegistryClient {
    MiseJavaRegistryClient::new(
        server.url(),
        REG,
        MiseJavaOptions::from_config(None, None, None),
        &Default::default(),
    )
    .unwrap()
    .with_test_host("github.com", &server.url())
    .with_test_host("cdn.azul.com", &server.url())
    .with_test_host("release-assets.githubusercontent.com", &server.url())
}

async fn app_against(server: &mockito::ServerGuard) -> impl TestService {
    app_with(server, Vec::new()).await
}

async fn app_with(
    server: &mockito::ServerGuard,
    extra_rules: Vec<Box<dyn Rule>>,
) -> impl TestService {
    let parts = local_registry_app_parts(REG, "mise-java", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(client(server)) as Arc<dyn RegistryClient>,
        );
        let mut rules: Vec<Box<dyn Rule>> = vec![Box::new(BlockListRule::new(Arc::clone(
            &parts.proxy_svc.repo,
        )))];
        rules.extend(extra_rules);
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules,
            }),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

const DOC: &str = "/proxy/jdk/mise-java/jvm/ga/linux/x86_64.json";

fn dl(url: &str) -> String {
    format!(
        "/proxy/{REG}/mise-java/dl/{}",
        url.strip_prefix("https://").unwrap()
    )
}

/// The vendor path of a row URL, as the mock sees it.
fn vendor_path(url: &str) -> String {
    let rest = url.strip_prefix("https://").unwrap();
    format!("/{}", rest.split_once('/').unwrap().1)
}

fn urls_of(doc: &serde_json::Value) -> Vec<&str> {
    doc.as_array()
        .unwrap()
        .iter()
        .map(|r| r["url"].as_str().unwrap())
        .collect()
}

/// T1: rows off `vendor_hosts` — another GitHub owner, an internal address —
/// never reach the client; the rest are served with their URLs unchanged.
#[actix_web::test]
async fn an_off_list_host_row_is_never_served() {
    let server = upstream(rows(&format!("sha256:{}", digest(ARCHIVE)))).await;
    let app = app_against(&server).await;
    let doc = get_json(&app, DOC).await;
    assert_eq!(urls_of(&doc), [ADOPTIUM, ZULU]);
}

/// T1, the redirect half: a row on an allowed owner whose sibling owner is
/// not allowed is never fetchable through the download route either.
#[actix_web::test]
async fn a_row_on_another_github_owner_is_never_served() {
    let mut server = upstream(rows("")).await;
    let never = server
        .mock("GET", mockito::Matcher::Regex("^/someone/".to_owned()))
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;
    let resp = call_service(
        &app,
        admin_get(&dl(
            "https://github.com/someone/jdk/releases/download/1/x.tar.gz",
        )),
    )
    .await;
    assert_eq!(resp.status(), 404);
    never.assert_async().await;
}

/// T5 and §5.2: the archive streams from the row's vendor, is verified against
/// the row's `sha256:` checksum at cache write, and the second read is the
/// cache's. Also R6/obs. 12: this client never read a document first — the
/// lock-driven install — and a fresh client (a restart) resolves the same URL.
#[actix_web::test]
async fn a_sha256_prefixed_checksum_is_verified_on_cache_write() {
    let mut server = upstream(rows(&format!("sha256:{}", digest(ARCHIVE)))).await;
    let archive = server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .with_body(ARCHIVE)
        .expect(1)
        .create_async()
        .await;
    let app = app_against(&server).await;
    assert_eq!(get_bytes(&app, &dl(ADOPTIUM)).await, ARCHIVE);
    // `+` and `%2B` are one key.
    assert_eq!(
        get_bytes(&app, &dl(&ADOPTIUM.replace("%2B", "+"))).await,
        ARCHIVE
    );
    archive.assert_async().await;

    let resp = call_service(&app, admin_get(&dl(ADOPTIUM))).await;
    assert_eq!(
        resp.headers()
            .get("X-BatleHub-Version")
            .and_then(|v| v.to_str().ok()),
        Some(TEMURIN)
    );
}

#[actix_web::test]
async fn the_index_survives_a_restart() {
    let mut server = upstream(rows(&format!("sha256:{}", digest(ARCHIVE)))).await;
    server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .with_body(ARCHIVE)
        .create_async()
        .await;
    // Read the document on one instance, download on another: the second has
    // never seen a document and finds the row by itself.
    let first = app_against(&server).await;
    get_json(&first, DOC).await;
    let second = app_against(&server).await;
    let resp = call_service(&second, admin_get(&dl(ADOPTIUM))).await;
    let status = resp.status();
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
}

#[actix_web::test]
async fn an_archive_that_does_not_match_its_row_is_refused() {
    let mut server = upstream(rows(&format!("sha256:{}", "0".repeat(64)))).await;
    server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .with_body(ARCHIVE)
        .expect(2)
        .create_async()
        .await;
    let app = app_against(&server).await;
    for _ in 0..2 {
        let resp = call_service(&app, admin_get(&dl(ADOPTIUM))).await;
        assert_eq!(
            resp.status(),
            502,
            "a mismatching archive is never served or kept"
        );
    }
}

/// RFC 0048 T10 at the HTTP layer: a mismatch found after the body began to
/// stream aborts the response. The client sees a cut transfer, never a short
/// `200` that ends cleanly and reads as a complete archive.
#[actix_web::test]
async fn a_mismatch_found_mid_stream_aborts_the_response() {
    let big = vec![b'j'; 9 * 1024 * 1024];
    let mut server = upstream(rows(&format!("sha256:{}", "0".repeat(64)))).await;
    server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .with_body(&big)
        .create_async()
        .await;
    let app = app_against(&server).await;
    let resp = call_service(&app, admin_get(&dl(ADOPTIUM))).await;
    assert_eq!(
        resp.status(),
        200,
        "the first segment streams before the end"
    );
    let body = actix_web::body::to_bytes(resp.into_body()).await;
    assert!(
        body.is_err(),
        "a reset body must be an error, not {} clean bytes",
        body.map(|b| b.len()).unwrap_or(0)
    );
}

/// T5, accepted residual (§15 Q10): an `md5:` row stays in the document and
/// its archive is cached unverified — served, and served again from cache.
#[actix_web::test]
async fn a_md5_row_is_kept_cached_unverified_and_counted() {
    let mut server = upstream(rows("")).await;
    let archive = server
        .mock("GET", vendor_path(ZULU).as_str())
        .with_body(ARCHIVE)
        .expect(1)
        .create_async()
        .await;
    let app = app_against(&server).await;
    assert!(urls_of(&get_json(&app, DOC).await).contains(&ZULU));
    assert_eq!(get_bytes(&app, &dl(ZULU)).await, ARCHIVE);
    assert_eq!(get_bytes(&app, &dl(ZULU)).await, ARCHIVE);
    archive.assert_async().await;
}

/// T3 / R6: a URL no document names is `404`, and nothing is fetched for it.
#[actix_web::test]
async fn a_download_not_in_the_document_is_404_without_upstream() {
    let mut server = upstream(rows("")).await;
    let never = server
        .mock(
            "GET",
            mockito::Matcher::Regex("^/(latest|zulu|adoptium)/".to_owned()),
        )
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;
    for url in [
        "https://169.254.169.254/latest/meta-data",
        "https://cdn.azul.com/zulu/bin/not-a-row.tar.gz",
        "https://github.com/adoptium/temurin21-binaries/releases/download/x/../../y.tar.gz",
        "https://cdn.azul.com/zulu/bin/a%252e%252e/b",
    ] {
        let resp = call_service(&app, admin_get(&dl(url))).await;
        assert_eq!(resp.status(), 404, "{url}");
    }
    never.assert_async().await;
}

/// T4 and R5: a blocked build leaves the document, and its download is `403`
/// before anything is fetched.
#[actix_web::test]
async fn a_blocked_row_download_is_403() {
    let mut server = upstream(rows("")).await;
    let never = server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;
    block_version(&app, REG, "java", TEMURIN).await;
    assert_eq!(urls_of(&get_json(&app, DOC).await), [ZULU]);
    let resp = call_service(&app, admin_get(&dl(ADOPTIUM))).await;
    assert_eq!(resp.status(), 403);
    never.assert_async().await;
}

/// §15 Q1, the phase-2 acceptance test: a lock pins the GitHub URL of a
/// blocked Temurin, mise skips the document and asks for the URL directly —
/// and the rules `client setup` writes send it here, before the `github`
/// rules, where the block holds.
#[actix_web::test]
async fn a_locked_blocked_temurin_via_the_rules_is_403() {
    use batlehub_core::entities::RegistryKind;
    use batlehub_core::services::mise_java::DEFAULT_VENDOR_HOSTS;
    use batlehub_core::services::mise_wire::{select, wired_prefix, Mode, WireRegistry};

    let vendor_hosts: Vec<String> = DEFAULT_VENDOR_HOSTS.iter().map(|s| s.to_string()).collect();
    let reg = |name, kind, vendor_hosts| WireRegistry {
        name,
        kind,
        upstream: Some("https://api.github.com"),
        upstreams: &[],
        path_routing: true,
        vendor_hosts,
    };
    let selection = select(
        &[
            reg("jdk", RegistryKind::MiseJava, &vendor_hosts),
            reg("github", RegistryKind::Github, &[]),
        ],
        Mode::Classic,
    );
    // mise's `url_replacements` is first-match: the first rule whose literal
    // host and prefix the locked URL sits under decides where it goes.
    let (host, path) = ADOPTIUM
        .strip_prefix("https://")
        .unwrap()
        .split_once('/')
        .unwrap();
    let first = selection
        .rules
        .iter()
        .find(|r| {
            let (h, p) = wired_prefix(&r.pattern).unwrap();
            h == host && path.starts_with(&p)
        })
        .unwrap();
    assert_eq!(first.registry, "jdk", "{selection:?}");
    let (_, prefix) = wired_prefix(&first.pattern).unwrap();
    let rewritten = first
        .replacement
        .replace("{proxy}", &format!("/proxy/{REG}/mise-java"))
        .replace("$1", path.strip_prefix(&prefix).unwrap());
    assert_eq!(rewritten, dl(ADOPTIUM));

    let mut server = upstream(rows("")).await;
    let never = server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;
    block_version(&app, REG, "java", TEMURIN).await;
    let resp = call_service(&app, admin_get(&rewritten)).await;
    assert_eq!(resp.status(), 403);
    never.assert_async().await;
}

/// The age gate reads `created_at`: a build younger than the gate leaves the
/// document and its download is refused.
#[actix_web::test]
async fn a_build_younger_than_the_gate_is_hidden_and_refused() {
    let mut fresh = rows("");
    fresh[0]["created_at"] = chrono::Utc::now().to_rfc3339().into();
    let server = upstream(fresh).await;
    let gate: Box<dyn Rule> = Box::new(ReleaseAgeGateRule::new(
        Duration::from_secs(7 * 86400),
        Vec::new(),
    ));
    let app = app_with(&server, vec![gate]).await;
    assert_eq!(urls_of(&get_json(&app, DOC).await), [ZULU]);
    let resp = call_service(&app, admin_get(&dl(ADOPTIUM))).await;
    assert_eq!(resp.status(), 403);
}

/// T1 (§15 Q2): every hop is https and lands on `vendor_hosts` or
/// `redirect_hosts`; anything else is `502`, and nothing is cached — the next
/// request asks the vendor again.
#[actix_web::test]
async fn a_redirect_to_http_or_loopback_is_502_and_uncached() {
    for location in [
        "http://release-assets.githubusercontent.com/asset/1",
        "https://127.0.0.1/asset/1",
        "https://localhost/asset/1",
    ] {
        let mut server = upstream(rows("")).await;
        let hop = server
            .mock("GET", vendor_path(ADOPTIUM).as_str())
            .with_status(302)
            .with_header("location", location)
            .expect(2)
            .create_async()
            .await;
        let app = app_against(&server).await;
        for _ in 0..2 {
            let resp = call_service(&app, admin_get(&dl(ADOPTIUM))).await;
            assert_eq!(resp.status(), 502, "{location}");
        }
        hop.assert_async().await;
    }
}

#[actix_web::test]
async fn a_redirect_off_redirect_hosts_is_502() {
    let mut server = upstream(rows("")).await;
    server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .with_status(302)
        .with_header("location", "https://objects.example.net/asset/1")
        .create_async()
        .await;
    let app = app_against(&server).await;
    let resp = call_service(&app, admin_get(&dl(ADOPTIUM))).await;
    assert_eq!(resp.status(), 502);

    // The allowed redirect, for contrast: github.com → release-assets.
    let mut server = upstream(rows("")).await;
    server
        .mock("GET", vendor_path(ADOPTIUM).as_str())
        .with_status(302)
        .with_header(
            "location",
            "https://release-assets.githubusercontent.com/asset/1",
        )
        .create_async()
        .await;
    server
        .mock("GET", "/asset/1")
        .with_body(ARCHIVE)
        .create_async()
        .await;
    let app = app_against(&server).await;
    assert_eq!(get_bytes(&app, &dl(ADOPTIUM)).await, ARCHIVE);
}

/// T7: the client's token stops at this instance; the vendor sees none.
#[actix_web::test]
async fn no_client_credential_reaches_the_vendor() {
    let mut server = upstream(rows("")).await;
    let vendor = server
        .mock("GET", vendor_path(ZULU).as_str())
        .match_header("authorization", mockito::Matcher::Missing)
        .with_body(ARCHIVE)
        .expect(1)
        .create_async()
        .await;
    let app = app_against(&server).await;
    assert_eq!(get_bytes(&app, &dl(ZULU)).await, ARCHIVE);
    vendor.assert_async().await;
}

/// The platform segments reach an upstream URL: anything off the fixed lists,
/// or a release type the registry does not serve, is `404` at the edge.
#[actix_web::test]
async fn an_unknown_platform_or_release_type_is_404() {
    let server = upstream(rows("")).await;
    let app = app_against(&server).await;
    for uri in [
        "/proxy/jdk/mise-java/jvm/nightly/linux/x86_64.json",
        "/proxy/jdk/mise-java/jvm/ga/plan9/x86_64.json",
        "/proxy/jdk/mise-java/jvm/ga/linux/%2e%2e.json",
        "/proxy/jdk/mise-java/jvm/ea/linux/x86_64.json",
    ] {
        let resp = call_service(&app, admin_get(uri)).await;
        assert_eq!(resp.status(), 404, "{uri}");
    }
}
