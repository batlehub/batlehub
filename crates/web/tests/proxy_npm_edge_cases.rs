//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    ports::{CacheStore, RegistryClient, StorageBackend, UserTokenRepository},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService},
};
use batlehub_web::RegistryModeMap;

// ── proxy/npm.rs: wrong-registry-type and unknown-registry paths ──────────────

#[actix_web::test]
async fn get_packument_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    // "github" is registered but is type "github", not npm/cargo/openvsx
    let req = TestRequest::get()
        .uri("/proxy/github/some-package")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn get_packument_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/no-such-registry/some-package")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn get_version_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/some-package/1.0.0")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn get_version_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/no-such-registry/some-package/1.0.0")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn audit_quick_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/proxy/cargo/-/npm/v1/audit/quick")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn audit_quick_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/proxy/no-such/-/npm/v1/audit/quick")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn audit_quick_no_upstream_configured_returns_404() {
    // make_app uses UpstreamMap::default() (empty), so no upstream for "npm"
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/audit/quick")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn audit_quick_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // Bind a random local port and serve a single HTTP response.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"{}";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(body).await;
    });

    let upstream_url = format!("http://127.0.0.1:{port}");

    let repo_dyn: Arc<dyn batlehub_core::ports::PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "npm".to_owned(),
        FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<batlehub_core::services::RegistryPolicy>> =
        [("npm".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0))].into();
    let hot = new_hot_lock(HotConfig {
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access_config_for(&["npm"]);
    let registry_map = registry_map_for(&[("npm", "npm")]);
    let mut upstream_entries = std::collections::HashMap::new();
    upstream_entries.insert("npm".to_owned(), upstream_url);
    let upstream_map = batlehub_web::UpstreamMap::from(upstream_entries);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();
    let app = finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults {
            upstream_map,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await;

    let req = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/audit/quick")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"packages": {}}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

// ── proxy/npm.rs: download_tarball wrong registry type ───────────────────────

#[actix_web::test]
async fn download_tarball_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/cargo/some-package/1.0.0/tarball")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── front_office/packages: build_proxy_url coverage ──────────────────────────

#[actix_web::test]
async fn access_check_returns_proxy_url_for_github_tarball() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=github&name=rust-lang%2Frust&version=v1.80.0&artifact=tarball")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/github/rust-lang/rust/tarball/v1.80.0"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_github_zipball() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=github&name=rust-lang%2Frust&version=v1.80.0&artifact=zipball")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/github/rust-lang/rust/zipball/v1.80.0"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_github_raw_file() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=github&name=rust-lang%2Frust&version=v1.80.0&artifact=raw%2FCompiler_Options.md")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/github/rust-lang/rust/raw/v1.80.0/Compiler_Options.md"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_github_asset_by_name() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=github&name=rust-lang%2Frust&version=v1.80.0&artifact=rustc-1.80.0-x86_64.tar.gz")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url
        .contains("/proxy/github/rust-lang/rust/releases/assets/rustc-1.80.0-x86_64.tar.gz"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_npm_metadata() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=npm&name=lodash&version=4.17.21")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/npm/lodash/4.17.21"));
    assert!(!proxy_url.contains("/tarball"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_cargo_metadata() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=cargo&name=serde&version=1.0.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/cargo/serde/1.0.0"));
    assert!(!proxy_url.contains("/download"));
}

#[actix_web::test]
async fn access_check_returns_null_proxy_url_for_unknown_registry_type() {
    let app = make_app(InMemoryRepo::new()).await;
    // openvsx is a known registry but has no build_proxy_url branch -> returns None
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=openvsx&name=some.ext&version=1.0.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["proxy_url"].is_null());
}
