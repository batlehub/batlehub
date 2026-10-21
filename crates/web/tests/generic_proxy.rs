//! Integration tests for the `generic` file-mirror registry
//! (`GET /proxy/{registry}/generic/{path}`).
//!
//! Unlike most files in this suite, the app here is wired with the **real**
//! [`PathProxyRegistryClient`] pointed at a `mockito` upstream rather than a
//! `FixedRegistry` stub. The `path_allow` allowlist lives inside that client, so
//! stubbing it out would leave the security behaviour this registry type exists
//! to provide untested end-to-end.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body, TestRequest};

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::registry::{PathProxyRegistryClient, UpstreamHttpOptions};
use batlehub_core::{
    ports::{CacheStore, PackageRepository, RegistryClient, StorageBackend, UserTokenRepository},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy},
};
use batlehub_web::RegistryModeMap;

use common::*;

/// Build an app with a single `generic` registry named `files`, backed by a real
/// path-proxy client pointed at `upstream_url` and restricted to `path_allow`.
async fn make_generic_app(
    upstream_url: &str,
    path_allow: &[&str],
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo = InMemoryRepo::new();
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let proxy_metrics = Arc::new(ProxyMetrics::new(&[]));

    let allow: Vec<String> = path_allow.iter().map(|p| (*p).to_owned()).collect();
    let client =
        PathProxyRegistryClient::new("generic", upstream_url, &UpstreamHttpOptions::default())
            .expect("building path-proxy client")
            .with_path_allow(&allow)
            .expect("compiling path_allow globs");

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "files".to_owned(),
        Arc::new(client) as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> = [(
        "files".to_owned(),
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
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage: storage.clone(),
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: proxy_metrics.clone(),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);

    finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config_for(&["files"]),
        registry_map_for(&[("files", "generic")]),
        make_local_svc(hot, storage),
        RegistryModeMap::default(),
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            proxy_metrics,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await
}

#[actix_web::test]
async fn generic_get_streams_allowed_path_from_upstream() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/v24.18.0/node-v24.18.0-linux-x64.tar.gz")
        .with_status(200)
        .with_body("tarball-bytes")
        .create_async()
        .await;

    let app = make_generic_app(&server.url(), &["v*/node-v*-linux-x64.tar.gz"]).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/files/generic/v24.18.0/node-v24.18.0-linux-x64.tar.gz")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await, "tarball-bytes");
}

#[actix_web::test]
async fn generic_get_outside_path_allow_returns_403() {
    // No mock registered for this path: reaching 403 rather than a 502/404 from
    // an unmatched upstream request proves the allowlist denied it locally.
    let server = mockito::Server::new_async().await;
    let app = make_generic_app(&server.url(), &["v*/node-v*-linux-x64.tar.gz"]).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/files/generic/some-other-bucket/secret.bin")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn generic_traversal_path_returns_400() {
    let server = mockito::Server::new_async().await;
    let app = make_generic_app(&server.url(), &["**"]).await;
    // `**` allows everything, so a rejection here can only come from the
    // handler's edge traversal check — not from the allowlist.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/files/generic/../../etc/passwd")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn generic_double_star_allows_any_path() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/any/deep/nested/file.bin")
        .with_status(200)
        .with_body("ok")
        .create_async()
        .await;

    let app = make_generic_app(&server.url(), &["**"]).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/files/generic/any/deep/nested/file.bin")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn generic_missing_upstream_path_returns_404() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", "/v1.0.0/missing.tar.gz")
        .with_status(404)
        .create_async()
        .await;

    let app = make_generic_app(&server.url(), &["**"]).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/files/generic/v1.0.0/missing.tar.gz")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn generic_get_on_non_generic_registry_is_404() {
    let server = mockito::Server::new_async().await;
    let app = make_generic_app(&server.url(), &["**"]).await;
    // `unknown` is not in the registry map at all → require_registry_type 404s.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/unknown/generic/some/file.bin")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}
