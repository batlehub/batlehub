//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, init_service, read_body, read_body_json, TestRequest};
use serde_json::Value;
use utoipa_actix_web::AppExt;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    ports::{CacheStore, PackageRepository, StorageBackend, UserTokenRepository},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService},
};
use batlehub_web::{healthz, prometheus_metrics, AuthMiddlewareFactory};
use metrics_exporter_prometheus::PrometheusBuilder;

// ── /healthz ──────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn healthz_returns_ok_without_db() {
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
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
        registries: HashMap::new(),
        policies: HashMap::new(),
        ..Default::default()
    });
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache: Arc::new(InMemoryCacheStore::new()),
        repo: InMemoryRepo::new(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });

    let app = init_service(
        actix_web::App::new()
            .app_data(actix_web::web::Data::new(proxy_svc))
            .service(healthz),
    )
    .await;

    let req = TestRequest::get().uri("/healthz").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["db"], "unconfigured");
    assert_eq!(body["storage"], "ok");
}

#[actix_web::test]
async fn healthz_is_unauthenticated() {
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let proxy_svc = Arc::new(ProxyService {
        hot: new_hot_lock(HotConfig {
            // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
            // `instance_node` is §10 rule 5's own translation, so the fixture's admin
            // holds the control verbs and nobody else does. Without it every
            // `require_verb` on a control endpoint refuses, including the admin the
            // suite is asserting about — a fixture that does not build the model
            // tests a server nobody runs (§13.5).
            instance: Some(std::sync::Arc::new(
                batlehub_core::services::authz::translate::instance_node(None),
            )),
            registries: HashMap::new(),
            policies: HashMap::new(),
            ..Default::default()
        }),
        storage,
        cache: Arc::new(InMemoryCacheStore::new()),
        repo: InMemoryRepo::new(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });

    let app = init_service(
        actix_web::App::new()
            // RFC 0015 §4.2 — this app registers only the handlers under test, so the
            // hot lock the control-verb check reads has to be registered with them.
            .app_data(actix_web::web::Data::new(
                batlehub_core::services::hot_config::new_hot_lock(
                    batlehub_core::services::hot_config::HotConfig {
                        instance: Some(std::sync::Arc::new(
                            batlehub_core::services::authz::translate::instance_node(None),
                        )),
                        ..Default::default()
                    },
                ),
            ))
            .app_data(actix_web::web::Data::new(proxy_svc))
            .service(healthz)
            .wrap(AuthMiddlewareFactory::new(test_auth_providers())),
    )
    .await;

    // No Authorization header — must still return 200
    let req = TestRequest::get().uri("/healthz").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

// ── /metrics ──────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn metrics_returns_503_without_handle() {
    let app = init_service(actix_web::App::new().service(prometheus_metrics)).await;

    let req = TestRequest::get().uri("/metrics").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 503);
}

#[actix_web::test]
async fn metrics_returns_200_with_handle() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();

    let app = init_service(
        actix_web::App::new()
            // RFC 0015 §4.2 — this app registers only the handlers under test, so the
            // hot lock the control-verb check reads has to be registered with them.
            .app_data(actix_web::web::Data::new(
                batlehub_core::services::hot_config::new_hot_lock(
                    batlehub_core::services::hot_config::HotConfig {
                        instance: Some(std::sync::Arc::new(
                            batlehub_core::services::authz::translate::instance_node(None),
                        )),
                        ..Default::default()
                    },
                ),
            ))
            .app_data(actix_web::web::Data::new(handle))
            .service(prometheus_metrics),
    )
    .await;

    let req = TestRequest::get().uri("/metrics").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.starts_with("text/plain"),
        "unexpected content-type: {ct}"
    );
}

// ══ CLI download endpoint ══════════════════════════════════════════════════════

#[actix_web::test]
async fn cli_download_returns_404_when_not_configured() {
    // No CliBinaryPath in app_data → handler returns 404
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/cli/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn cli_download_serves_binary_when_configured() {
    use batlehub_web::CliBinaryPath;

    // Write a fake binary to a temp file.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"fake-cli-bytes").unwrap();
    let path = tmp.path().to_path_buf();

    // Build a minimal app, identical to make_app but with CliBinaryPath added.
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
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
        registries: HashMap::new(),
        policies: HashMap::new(),
        ..Default::default()
    });
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access_config_for(&[]);

    let (raw, _) = actix_web::App::new()
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            admin_svc,
            token_repo,
            access_config,
            registry_map_for(&[]),
            ConfigureAppDefaults::default(),
        ))
        .split_for_parts();

    let local_svc = make_local_svc(hot.clone(), InMemoryStorage::new());
    let app = init_service(
        raw.app_data(actix_web::web::Data::new(CliBinaryPath(path)))
            .app_data(actix_web::web::Data::new(local_svc))
            .app_data(actix_web::web::Data::new(
                batlehub_web::RegistryModeMap::default(),
            ))
            .wrap(AuthMiddlewareFactory::new(test_auth_providers())),
    )
    .await;

    let req = TestRequest::get()
        .uri("/api/v1/cli/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, "application/octet-stream");
    let body = read_body(resp).await;
    assert_eq!(&body[..], b"fake-cli-bytes");
}
