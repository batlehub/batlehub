//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use chrono::Utc;
use serde_json::Value;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    entities::{AccessEvent, PackageId, PackageStatus, Role},
    ports::{CacheStore, PackageRepository, RegistryClient, StorageBackend, UserTokenRepository},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy},
};
use batlehub_web::RegistryModeMap;

// ── Cargo sparse registry config ─────────────────────────────────────────────

/// Build a test app with a wired-up CargoIndexProxy so we can test the
/// `cargo_registry_config` handler's happy path.
async fn make_app_with_cargo_index(
    repo: Arc<InMemoryRepo>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "cargo".to_owned(),
        FixedRegistry::new("cargo") as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> = [(
        "cargo".to_owned(),
        Arc::new(rbac_policy(repo_dyn.clone()).0),
    )]
    .into();

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
    let access_config = access_config_for(&["cargo"]);
    let registry_map = registry_map_for(&[("cargo", "cargo")]);

    // Wire up a real CargoIndexProxy entry so cargo_registry_config can return a config
    let cargo_indexes = batlehub_web::CargoIndexMap::new(std::collections::HashMap::from([(
        "cargo".to_owned(),
        batlehub_web::CargoIndexProxy {
            http: reqwest::Client::new(),
            index_url: "https://index.crates.io".to_owned(),
        },
    )]));

    finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults::default(),
        test_auth_providers(),
    )
    .await
}

#[actix_web::test]
async fn cargo_registry_config_returns_dl_url() {
    let app = make_app_with_cargo_index(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/cargo/registry/config.json")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["dl"]
        .as_str()
        .unwrap()
        .contains("/proxy/cargo/{crate}/{version}/download"));
}

#[actix_web::test]
async fn cargo_registry_config_returns_404_for_unknown_registry() {
    let app = make_app(InMemoryRepo::new()).await;
    // 'npm' is not a cargo registry
    let req = TestRequest::get()
        .uri("/proxy/npm/registry/config.json")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn cargo_registry_config_returns_404_when_no_index_configured() {
    // make_app uses empty cargo_indexes, so the cargo registry exists but has no index
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/cargo/registry/config.json")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn cargo_registry_index_returns_404_for_non_cargo_registry() {
    // npm is not a cargo registry — cargo_registry_index should return 404
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/registry/se/rd/serde")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn cargo_registry_index_returns_404_when_no_index_configured() {
    // cargo registry exists in the map but cargo_indexes is empty
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/cargo/registry/se/rd/serde")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── front_office packages: registry filter + proxy_url ───────────────────────

#[actix_web::test]
async fn packages_list_filters_out_inaccessible_registry() {
    let repo = InMemoryRepo::new();
    // Record a package in an inaccessible registry
    let pkg_npm = PackageId::new("npm", "lodash", "4.17.21");
    let pkg_github = PackageId::new("github", "rust-lang/rust", "v1.80.0");
    repo.record_access(AccessEvent::allowed_download(
        pkg_npm,
        Some("u".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();
    repo.record_access(AccessEvent::allowed_download(
        pkg_github,
        Some("u".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    let app = make_app(repo).await;

    // Filter by npm — should only return npm package
    let req = TestRequest::get()
        .uri("/api/v1/packages?registry=npm")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert!(items.iter().all(|i| i["registry"] == "npm"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_npm_tarball() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=npm&name=lodash&version=4.17.21&artifact=tarball")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["can_access"], true);
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/npm/lodash/4.17.21/tarball"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_cargo_download() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=cargo&name=serde&version=1.0.0&artifact=download")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/cargo/serde/1.0.0/download"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_github_releases() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=github&name=rust-lang%2Frust&version=releases")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/github/rust-lang/rust/releases"));
}

#[actix_web::test]
async fn access_check_returns_proxy_url_for_github_tag() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=github&name=rust-lang%2Frust&version=v1.80.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let proxy_url = body["proxy_url"].as_str().unwrap();
    assert!(proxy_url.contains("/proxy/github/rust-lang/rust/releases/tags/v1.80.0"));
}

#[actix_web::test]
async fn packages_list_returns_empty_for_inaccessible_registry_filter() {
    // When a user asks for packages from a registry they can't access, they get empty results
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("github", "rust-lang/rust", "v1.80.0");
    repo.record_access(AccessEvent::allowed_download(
        pkg,
        Some("u".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    // make_app gives anonymous access to github, so anon CAN see it normally.
    // But filtering for a completely unknown registry should return empty.
    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages?registry=pypi")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 0);
}

// ── Cargo download (source:read) ──────────────────────────────────────────────

#[actix_web::test]
async fn proxy_cargo_download_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/cargo/serde/1.0.0/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = actix_web::test::read_body(resp).await;
    assert!(std::str::from_utf8(&body).unwrap().contains("serde"));
}

// ── npm tarball (source:read) ─────────────────────────────────────────────────

#[actix_web::test]
async fn proxy_npm_tarball_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21/tarball")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = actix_web::test::read_body(resp).await;
    assert!(std::str::from_utf8(&body).unwrap().contains("lodash"));
}

// ── The name the saved file gets ──────────────────────────────────────────────
//
// `…/{version}/tarball`, `…/{version}/download` and `…/{version}/vsix` end in a
// verb, because that is the address the package manager is specified to fetch.
// A browser saving one has nothing else to go on and writes a file called
// `tarball` or `download`, with no extension and no coordinate — which is what
// the console's own download link produced. `Content-Disposition` is the only
// place the answer can come from; the package managers never read it.

fn disposition_of(resp: &actix_web::dev::ServiceResponse<actix_web::body::BoxBody>) -> String {
    resp.headers()
        .get("Content-Disposition")
        .expect("a nameless download route must name its file")
        .to_str()
        .unwrap()
        .to_owned()
}

#[actix_web::test]
async fn proxy_cargo_download_names_the_crate_file() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/cargo/serde/1.0.0/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        disposition_of(&resp),
        "attachment; filename=\"serde-1.0.0.crate\""
    );
}

#[actix_web::test]
async fn proxy_npm_tarball_names_the_tgz() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21/tarball")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        disposition_of(&resp),
        "attachment; filename=\"lodash-4.17.21.tgz\""
    );
}

// ── GitHub download routes ────────────────────────────────────────────────────

#[actix_web::test]
async fn proxy_github_zipball_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/zipball/v1.80.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_github_zipball_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/zipball/v1.80.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_github_asset_by_name_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/releases/download/v1.80.0/rustc-1.80.0-x86_64-unknown-linux-gnu.tar.gz")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    // source:read required — user has it
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_github_asset_by_name_accessible_anonymously() {
    // releases/download uses releases:read which anonymous users have
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/releases/download/v1.80.0/rust.tar.gz")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_github_raw_file_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/raw/main/README.md")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_github_raw_file_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/raw/main/README.md")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_github_asset_by_id_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/releases/assets/12345678?tag=v1.80.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

// ── /api/v1/admin/packages/detail ────────────────────────────────────────────

#[actix_web::test]
async fn package_detail_returns_403_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages/detail?registry=npm&name=lodash")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn package_detail_returns_200_for_admin_with_no_packages() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages/detail?registry=npm&name=lodash")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["registry"], "npm");
    assert_eq!(body["name"], "lodash");
    assert!(body["versions"].as_array().unwrap().is_empty());
    assert!(body["recent_events"].as_array().unwrap().is_empty());
}

#[actix_web::test]
async fn package_detail_shows_versions_and_events_after_access() {
    let repo = InMemoryRepo::new();

    // Record a download event so the package appears in summaries and events
    let pkg = PackageId::new("npm", "lodash", "4.17.21");
    repo.record_access(AccessEvent::allowed_download(
        pkg.clone(),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages/detail?registry=npm&name=lodash")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["registry"], "npm");
    let versions = body["versions"].as_array().unwrap();
    assert!(!versions.is_empty(), "should list the recorded version");
    assert_eq!(versions[0]["version"], "4.17.21");
    // socket.dev badge is enabled by default for a supported registry type (npm).
    assert_eq!(
        versions[0]["socket_badge_url"],
        "https://badge.socket.dev/npm/package/lodash/4.17.21"
    );
    // No vulnerability repo attached in the test harness → empty findings.
    assert!(versions[0]["vulnerabilities"]
        .as_array()
        .unwrap()
        .is_empty());
    let events = body["recent_events"].as_array().unwrap();
    assert!(!events.is_empty(), "should list the recent events");
    assert_eq!(events[0]["outcome"], "allowed");
}

#[actix_web::test]
async fn package_detail_shows_blocked_status() {
    let repo = InMemoryRepo::new();

    let pkg = PackageId::new("npm", "evil-pkg", "1.0.0");
    repo.record_access(AccessEvent::allowed_download(
        pkg.clone(),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();
    repo.set_status(
        &pkg,
        PackageStatus::Blocked {
            reason: "vuln".to_owned(),
            blocked_by: "admin".to_owned(),
            blocked_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages/detail?registry=npm&name=evil-pkg")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let versions = body["versions"].as_array().unwrap();
    assert!(!versions.is_empty());
    assert_eq!(versions[0]["status"]["status"], "blocked");
    assert_eq!(versions[0]["status"]["reason"], "vuln");
}
