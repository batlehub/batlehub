//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, TestRequest};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures::stream;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    entities::{PackageId, PackageMetadata, Role},
    error::CoreError,
    ports::{
        CacheStore, FetchedArtifact, PackageRepository, RegistryClient, StorageBackend,
        UserTokenRepository,
    },
    rules::{BlockListRule, RbacRule},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy},
};
use batlehub_web::RegistryModeMap;

// ── OpenVSX proxy handler ─────────────────────────────────────────────────────

#[actix_web::test]
async fn proxy_openvsx_vsix_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/openvsx/ms-python.python/2023.20.0/vsix")
        .to_request();
    let resp = call_service(&app, req).await;
    // download_vsix uses source:read — anonymous only has releases:read
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_openvsx_vsix_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/openvsx/ms-python.python/2023.20.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_openvsx_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    // "npm" exists but is type "npm", not "openvsx" — require_openvsx rejects it
    let req = TestRequest::get()
        .uri("/proxy/npm/ms-python.python/2023.20.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn proxy_openvsx_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/unknown-reg/ms-python.python/2023.20.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── VS Code Marketplace proxy handler ─────────────────────────────────────────

#[actix_web::test]
async fn proxy_vscode_marketplace_vsix_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/vscode/ms-python.python/2024.2.1/vsix")
        .to_request();
    let resp = call_service(&app, req).await;
    // download_vsix uses source:read — anonymous only has releases:read
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_vscode_marketplace_vsix_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/vscode/ms-python.python/2024.2.1/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_vscode_marketplace_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    // "npm" exists but is type "npm", not "vscode-marketplace" — require_openvsx rejects it
    let req = TestRequest::get()
        .uri("/proxy/npm/ms-python.python/2024.2.1/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── GoProxy handler ───────────────────────────────────────────────────────────

#[actix_web::test]
async fn proxy_goproxy_latest_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/golang.org/x/text/@latest")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_goproxy_list_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/golang.org/x/text/@v/list")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_goproxy_info_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/golang.org/x/text/@v/v0.3.7.info")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_goproxy_mod_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/golang.org/x/text/@v/v0.3.7.mod")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_goproxy_zip_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/golang.org/x/text/@v/v0.3.7.zip")
        .to_request();
    let resp = call_service(&app, req).await;
    // zip uses source:read — anonymous only has releases:read
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_goproxy_zip_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/golang.org/x/text/@v/v0.3.7.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_goproxy_unknown_file_extension_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/golang.org/x/text/@v/v0.3.7.tar")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn proxy_goproxy_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    // "npm" exists but is type "npm", not "goproxy" — require_goproxy rejects it
    let req = TestRequest::get()
        .uri("/proxy/npm/golang.org/x/text/@latest")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── Upstream-KO / stale-serving integration tests ────────────────────────────
//
// These tests verify the end-to-end HTTP behaviour when the upstream registry
// is unavailable and the proxy falls back to stale metadata from its cache.

struct UnavailableRegistry;

#[async_trait]
impl RegistryClient for UnavailableRegistry {
    fn registry_type(&self) -> &str {
        "npm"
    }

    async fn resolve_metadata(&self, _pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Err(CoreError::Registry("upstream down".into()))
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let data = Bytes::from(format!("artifact:npm:{}", pkg.cache_key()));
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: None,
        })
    }
}

async fn make_unavailable_npm_app(
    repo: Arc<InMemoryRepo>,
    cache: Arc<InMemoryCacheStore>,
    serve_stale: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache_dyn: Arc<dyn CacheStore> = cache;

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "npm".to_owned(),
        Arc::new(UnavailableRegistry) as Arc<dyn RegistryClient>,
    )]
    .into();

    let perms = HashMap::from([
        (Role::Anonymous, vec!["releases:read".to_owned()]),
        (
            Role::User,
            vec!["releases:read".to_owned(), "source:read".to_owned()],
        ),
        (Role::Admin, vec!["*".to_owned()]),
    ]);
    let policies: HashMap<String, Arc<RegistryPolicy>> = [(
        "npm".to_owned(),
        Arc::new(RegistryPolicy {
            metadata_ttl: Some(Duration::from_secs(300)),
            firewall_only: false,
            serve_stale_metadata: serve_stale,
            artifact_ttl: None,
            rules: vec![
                Box::new(RbacRule::from_patterns(perms).expect("fixture rbac patterns are valid")),
                Box::new(BlockListRule::new(repo_dyn.clone())),
            ],
        }),
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
        cache: cache_dyn,
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

fn stale_npm_meta(name: &str, version: &str) -> PackageMetadata {
    PackageMetadata {
        id: PackageId::new("npm", name, version),
        published_at: Some(Utc::now() - chrono::Duration::days(30)),
        download_url: None,
        checksum: None,
        is_signed: None,
        extra: serde_json::json!({}),
        cache_control: None,
    }
}

#[actix_web::test]
async fn upstream_down_with_stale_metadata_returns_200() {
    let cache = Arc::new(InMemoryCacheStore::new());
    let pkg = PackageId::new("npm", "lodash", "4.17.21");
    let cache_key = format!("meta:{}", pkg.cache_key());
    cache
        .seed_expired(&cache_key, stale_npm_meta("lodash", "4.17.21"))
        .await;

    let app = make_unavailable_npm_app(InMemoryRepo::new(), cache, true).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        200,
        "stale metadata should be served when upstream is down"
    );
}

#[actix_web::test]
async fn upstream_down_no_stale_returns_502() {
    let cache = Arc::new(InMemoryCacheStore::new()); // empty — no stale entry
    let app = make_unavailable_npm_app(InMemoryRepo::new(), cache, true).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        502,
        "no stale + upstream down must return 502"
    );
}

#[actix_web::test]
async fn upstream_down_serve_stale_disabled_returns_502() {
    // Stale entry exists in cache but serve_stale_metadata = false
    let cache = Arc::new(InMemoryCacheStore::new());
    let pkg = PackageId::new("npm", "lodash", "4.17.21");
    let cache_key = format!("meta:{}", pkg.cache_key());
    cache
        .seed_expired(&cache_key, stale_npm_meta("lodash", "4.17.21"))
        .await;

    let app = make_unavailable_npm_app(InMemoryRepo::new(), cache, false).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        502,
        "serve_stale=false must not use the stale entry"
    );
}
