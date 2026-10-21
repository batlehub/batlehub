//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, TestRequest};

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    ports::{CacheStore, PackageRepository, RegistryClient, StorageBackend, UserTokenRepository},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy},
};
use batlehub_web::RegistryModeMap;

// ── RubyGems proxy-mode coverage ─────────────────────────────────────────────

async fn make_rubygems_proxy_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "gems".to_owned(),
        FixedRegistry::new("rubygems") as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> =
        [("gems".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0))].into();

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

    let access_config = access_config_for(&["gems"]);
    let registry_map = registry_map_for(&[("gems", "rubygems")]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();
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
async fn gem_download_proxy_mode_returns_200() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/gems/rails-7.1.0.gem")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn gem_download_invalid_filename_returns_400() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/gems/not-a-gem-file")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    // Missing .gem suffix → bad request
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn gem_download_wrong_registry_type_returns_404() {
    // "npm" in make_app is npm type, not rubygems → 404
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/gems/lodash-1.0.0.gem")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn gem_info_proxy_mode_returns_200() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/api/v1/gems/rails.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn gem_versions_proxy_mode_returns_200() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/api/v1/versions/rails.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn gem_specs_full_proxy_mode_returns_200() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/specs.4.8.gz")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn gem_specs_latest_proxy_mode_returns_200() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/latest_specs.4.8.gz")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn gem_specs_prerelease_proxy_mode_returns_200() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/prerelease_specs.4.8.gz")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn gem_gemspec_proxy_mode_returns_200() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::get()
        .uri("/proxy/gems/quick/Marshal.4.8/rails-7.1.0.gemspec.rz")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn gem_publish_in_proxy_mode_returns_404() {
    // publish is local/hybrid only — proxy mode → 404
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::post()
        .uri("/proxy/gems/api/v1/gems")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_payload(vec![0u8; 10])
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn gem_yank_in_proxy_mode_returns_404() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::delete()
        .uri("/proxy/gems/api/v1/gems/yank?gem_name=rails&version=7.0.0")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn gem_unyank_in_proxy_mode_returns_404() {
    let app = make_rubygems_proxy_app().await;
    let req = TestRequest::put()
        .uri("/proxy/gems/api/v1/gems/unyank?gem_name=rails&version=7.0.0")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}
