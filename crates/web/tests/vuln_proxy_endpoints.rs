//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, init_service, read_body_json, TestRequest};
use actix_web::App;
use serde_json::Value;
use utoipa_actix_web::AppExt;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    ports::{CacheStore, RegistryClient, StorageBackend},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService},
};
use batlehub_web::{AuthMiddlewareFactory, RegistryModeMap, RepoSignerMap};

// ─────────────────────────────────────────────────────────────────────────────
// ── Vulnerability proxy endpoints ────────────────────────────────────────────
// ─────────────────────────────────────────────────────────────────────────────

// ── npm audit bulk ────────────────────────────────────────────────────────────

#[actix_web::test]
async fn audit_bulk_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/proxy/cargo/-/npm/v1/audit/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn audit_bulk_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/proxy/no-such/-/npm/v1/audit/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn audit_bulk_no_upstream_configured_returns_404() {
    // make_app uses UpstreamMap::default() (empty) → no upstream for "npm" → 404
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/audit/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn audit_bulk_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"{}";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let upstream_url = format!("http://127.0.0.1:{port}");
    let app = upstream_forwarding_app("npm", "npm", upstream_url, rbac_policy).await;

    let req = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/audit/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"packages": {}}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── NuGet vulnerability endpoints ─────────────────────────────────────────────

#[actix_web::test]
async fn nuget_vuln_index_wrong_registry_type_returns_404() {
    // "npm" exists but is not a nuget registry.
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/nuget/v3/vulnerabilities/index.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn nuget_vuln_index_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/no-such/nuget/v3/vulnerabilities/index.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn nuget_vuln_page_wrong_registry_type_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/nuget/v3/vulnerabilities/page/0.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn nuget_vuln_page_invalid_id_returns_400() {
    // Validation runs before the upstream call; a space in the page ID is rejected.
    let app = make_local_nuget_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-nuget/nuget/v3/vulnerabilities/page/bad%20page")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

/// Builds a minimal app with one "nuget" registry and an upstream map pointing to `upstream_url`.
async fn build_nuget_vuln_test_app(upstream_url: String) -> impl TestService {
    upstream_forwarding_app("nuget", "nuget", upstream_url, rbac_policy).await
}

#[actix_web::test]
async fn nuget_vuln_index_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"[]";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let app = build_nuget_vuln_test_app(format!("http://127.0.0.1:{port}")).await;
    let req = TestRequest::get()
        .uri("/proxy/nuget/nuget/v3/vulnerabilities/index.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn nuget_vuln_page_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"[]";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let app = build_nuget_vuln_test_app(format!("http://127.0.0.1:{port}")).await;
    let req = TestRequest::get()
        .uri("/proxy/nuget/nuget/v3/vulnerabilities/page/0.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── Composer security advisories ─────────────────────────────────────────────

#[actix_web::test]
async fn composer_security_advisories_wrong_registry_type_returns_404() {
    // "npm" exists but is not a composer registry.
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/api/security-advisories/")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn composer_security_advisories_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/no-such/api/security-advisories/")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn composer_security_advisories_no_upstream_returns_404() {
    // make_local_composer_app uses UpstreamMap::default() (empty) → handler returns 404.
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    let req = TestRequest::get()
        .uri("/proxy/local-composer/api/security-advisories/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn composer_security_advisories_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"{\"advisories\":{}}";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let upstream_url = format!("http://127.0.0.1:{port}");
    let repo_dyn: Arc<dyn batlehub_core::ports::PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "packagist".to_owned(),
        FixedRegistry::new("composer") as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<batlehub_core::services::RegistryPolicy>> = [(
        "packagist".to_owned(),
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
    });
    let mode_map = RegistryModeMap::default();
    mode_map.insert("packagist".to_owned(), RegistryMode::Proxy);
    let mut upstream_entries = std::collections::HashMap::new();
    upstream_entries.insert("packagist".to_owned(), upstream_url);
    let upstream_map = batlehub_web::UpstreamMap::from(upstream_entries);
    let app = finish_test_app(
        proxy_svc,
        Arc::new(AdminService::new(repo_dyn)),
        Arc::new(NullTokenRepository),
        access_config_for(&["packagist"]),
        registry_map_for(&[("packagist", "composer")]),
        local_svc,
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            upstream_map,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await;

    let req = TestRequest::get()
        .uri("/proxy/packagist/api/security-advisories/?packages[]=vendor/pkg")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── goproxy vulnerability database ────────────────────────────────────────────

#[actix_web::test]
async fn goproxy_vuln_index_wrong_registry_type_returns_404() {
    // "npm" exists but is not a goproxy registry.
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/v1/index.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn goproxy_vuln_index_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/no-such/v1/index.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn goproxy_vuln_entry_invalid_id_returns_400() {
    // Validation rejects IDs with spaces before the upstream is contacted.
    // "go" is a goproxy registry in make_app, so require_registry_type passes.
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/go/v1/ID/bad%20id.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

/// Builds a minimal goproxy app with a custom `VulnDbMap` (not the shared default).
async fn build_goproxy_vuln_test_app(vuln_db_map: batlehub_web::VulnDbMap) -> impl TestService {
    let (proxy_svc, repo_dyn, local_svc) = one_registry_proxy("go", "goproxy", rbac_policy);

    let (app, _) = App::new()
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
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            Arc::new(AdminService::new(repo_dyn)),
            Arc::new(NullTokenRepository),
            access_config_for(&["go"]),
            registry_map_for(&[("go", "goproxy")]),
            ConfigureAppDefaults::default(),
        ))
        .split_for_parts();

    let app = app
        .app_data(actix_web::web::Data::new(
            batlehub_web::CargoIndexMap::default(),
        ))
        .app_data(actix_web::web::Data::new(local_svc))
        .app_data(actix_web::web::Data::new(RegistryModeMap::default()))
        .app_data(actix_web::web::Data::new(RepoSignerMap::default()))
        .app_data(actix_web::web::Data::new(vuln_db_map));

    init_service(app.wrap(AuthMiddlewareFactory::new(test_auth_providers()))).await
}

#[actix_web::test]
async fn goproxy_vuln_index_disabled_returns_404() {
    // An absent map entry disables the vuln DB proxy for the registry, matching
    // how `build_vuln_db_map` (server/src/hot_config.rs) signals "disabled" by
    // omitting the key entirely rather than inserting an empty string.
    let vuln_db = batlehub_web::VulnDbMap::new(std::collections::HashMap::new());
    let app = build_goproxy_vuln_test_app(vuln_db).await;
    let req = TestRequest::get()
        .uri("/proxy/go/v1/index.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn goproxy_vuln_index_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"[]";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let vuln_db = batlehub_web::VulnDbMap::new(
        [("go".to_owned(), format!("http://127.0.0.1:{port}"))]
            .into_iter()
            .collect(),
    );
    let app = build_goproxy_vuln_test_app(vuln_db).await;
    let req = TestRequest::get()
        .uri("/proxy/go/v1/index.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn goproxy_vuln_query_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"{\"vulns\":[]}";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let vuln_db = batlehub_web::VulnDbMap::new(
        [("go".to_owned(), format!("http://127.0.0.1:{port}"))]
            .into_iter()
            .collect(),
    );
    let app = build_goproxy_vuln_test_app(vuln_db).await;
    let req = TestRequest::post()
        .uri("/proxy/go/v1/query")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"queries": []}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn goproxy_vuln_entry_forwards_to_upstream_and_returns_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let body = b"{\"id\":\"GO-2023-1234\"}";
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let vuln_db = batlehub_web::VulnDbMap::new(
        [("go".to_owned(), format!("http://127.0.0.1:{port}"))]
            .into_iter()
            .collect(),
    );
    let app = build_goproxy_vuln_test_app(vuln_db).await;
    let req = TestRequest::get()
        .uri("/proxy/go/v1/ID/GO-2023-1234.json")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["id"], "GO-2023-1234");
}

#[actix_web::test]
async fn audit_bulk_relays_upstream_response_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await;
        // Upstream returns a non-trivial audit report.
        let body =
            br#"{"advisories":{"lodash":{"findings":[{"version":"4.17.15"}],"severity":"high"}}}"#;
        let _ = stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = stream.write_all(body).await;
    });

    let upstream_url = format!("http://127.0.0.1:{port}");
    let app = upstream_forwarding_app("npm", "npm", upstream_url, rbac_policy).await;

    let req = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/audit/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"packages": {"lodash": ["4.17.15"]}}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    // Verify the upstream body is relayed verbatim.
    assert_eq!(body["advisories"]["lodash"]["severity"], "high");
}

// ── npm audit on the paths npm actually sends (RFC 0009 §7.1) ─────────────────
//
// The suite above exercises `/-/npm/v1/audit/{bulk,quick}`, which npm has never
// sent — that is the defect RFC 0009 opens with, and those tests passing while
// `npm audit` 404s is exactly why they could not find it. These use npm's own
// paths, and the third pins the caching that keeps an audit answerable when the
// advisory database is unreachable.

/// A single-shot upstream: answers one request, then the socket is gone.
///
/// That is the point — the second request has no upstream to reach, so what
/// answers it can only be the cache.
async fn one_shot_upstream(body: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = stream.write_all(body.as_bytes()).await;
        }
        // Listener dropped here: nothing answers on this port again.
    });
    format!("http://127.0.0.1:{port}")
}

async fn audit_app(upstream_url: String, serve_stale: bool) -> impl TestService {
    upstream_forwarding_app("npm", "npm", upstream_url, move |repo| {
        let (mut policy, perms) = rbac_policy(repo);
        policy.serve_stale_metadata = serve_stale;
        // A short TTL so the second request is a stale read rather than a fresh hit.
        policy.metadata_ttl = Some(std::time::Duration::from_millis(1));
        (policy, perms)
    })
    .await
}

#[actix_web::test]
async fn npm_audit_bulk_is_served_on_the_path_npm_sends() {
    let app = audit_app(one_shot_upstream("{}").await, false).await;
    let req = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/security/advisories/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"express": ["4.18.2"]}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn npm_audit_quick_is_served_on_the_path_npm_sends() {
    let app = audit_app(one_shot_upstream("{}").await, false).await;
    let req = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/security/audits/quick")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "app", "requires": {}}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

/// RFC 0009 §4.2, the reason the helper exists: an audit answered once must
/// still be answerable when the advisory database has gone away. This used to
/// be a bare `reqwest` POST with no cache at all, so the second call was a 502.
#[actix_web::test]
async fn an_audit_survives_the_advisory_database_going_away() {
    let app = audit_app(one_shot_upstream(r#"{"advisories":{}}"#).await, true).await;
    let body = serde_json::json!({"express": ["4.18.2"]});

    let first = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/security/advisories/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(&body)
        .to_request();
    assert_eq!(call_service(&app, first).await.status(), 200);

    // The one-shot upstream is gone now. Same question, no upstream.
    let second = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/security/advisories/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(&body)
        .to_request();
    let resp = call_service(&app, second).await;
    assert_eq!(
        resp.status(),
        200,
        "an unreachable advisory database must not fail the pipeline asking about it"
    );
    let cache_header = resp
        .headers()
        .get("X-BatleHub-Cache")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(
        cache_header == "hit" || cache_header == "stale",
        "the answer must be marked as coming from cache, got {cache_header:?}"
    );
}

/// A *different* dependency set is a different question and must not be
/// answered from the first one's entry.
#[actix_web::test]
async fn a_different_dependency_set_is_a_different_cache_entry() {
    let app = audit_app(one_shot_upstream(r#"{"advisories":{}}"#).await, false).await;

    let first = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/security/advisories/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"express": ["4.18.2"]}))
        .to_request();
    assert_eq!(call_service(&app, first).await.status(), 200);

    // Upstream is gone and `serve_stale` is off, so a cache miss must surface
    // as an upstream failure rather than as the other project's advisories.
    let second = TestRequest::post()
        .uri("/proxy/npm/-/npm/v1/security/advisories/bulk")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"lodash": ["4.17.21"]}))
        .to_request();
    assert_eq!(
        call_service(&app, second).await.status(),
        502,
        "one project's audit answer must not be served to another's question"
    );
}

// ── The Go checksum database (RFC 0009 §7.4) ─────────────────────────────────
//
// The `sumdb/` half of GOPROXY, absent until this phase — so `go mod download`
// through BatleHub still opened a direct connection to sum.golang.org for every
// module it had not seen. The proxy had moved the egress, not removed it, and an
// air-gapped estate failed closed on a lookup it could not make.
//
// Caching is the point rather than an optimisation: a sumdb lookup answerable
// only while the log is reachable buys nothing. It is sound because the log is
// signed — the signature travels with the bytes, so a cached record is exactly
// as trustworthy as a live one, and nothing here parses or rewrites it.

async fn sumdb_app(sumdb_url: Option<&str>) -> impl TestService {
    let (proxy_svc, repo_dyn, local_svc) = one_registry_proxy("go", "goproxy", |repo| {
        let (mut policy, perms) = rbac_policy(repo);
        policy.serve_stale_metadata = true;
        policy.metadata_ttl = Some(std::time::Duration::from_millis(1));
        (policy, perms)
    });

    let sumdb_map = match sumdb_url {
        Some(u) => {
            batlehub_web::SumDbMap::new([("go".to_owned(), u.to_owned())].into_iter().collect())
        }
        None => batlehub_web::SumDbMap::default(),
    };

    finish_test_app_with_extra(
        proxy_svc,
        Arc::new(AdminService::new(repo_dyn)),
        Arc::new(NullTokenRepository),
        access_config_for(&["go"]),
        registry_map_for(&[("go", "goproxy")]),
        local_svc,
        RegistryModeMap::default(),
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        sumdb_map,
        test_auth_providers(),
    )
    .await
}

/// A registry with no sumdb configured must 404 rather than proxy a lookup:
/// a private-modules-only registry has no checksum database, and asking the
/// public log about a private module path leaks it.
#[actix_web::test]
async fn sumdb_disabled_returns_404() {
    let app = sumdb_app(None).await;
    let req = TestRequest::get()
        .uri("/proxy/go/sumdb/sum.golang.org/supported")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

/// `go` routes checksum lookups through a proxy only after a 200 on
/// `supported`, and nothing upstream answers that probe (even
/// proxy.golang.org is a 404 on it — tests/heavy/go.sh), so it is answered
/// here: with a log configured, the registry carries it.
#[actix_web::test]
async fn sumdb_supported_is_answered_by_the_registry() {
    let app = sumdb_app(Some("http://127.0.0.1:1")).await;
    let req = TestRequest::get()
        .uri("/proxy/go/sumdb/sum.golang.org/supported")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn sumdb_rejects_a_traversing_path() {
    let app = sumdb_app(Some("http://127.0.0.1:1")).await;
    let req = TestRequest::get()
        .uri("/proxy/go/sumdb/sum.golang.org/../../etc/passwd")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

/// The air-gapped case, end to end: one upstream answer, then the log is gone,
/// and the second build still resolves.
#[actix_web::test]
async fn a_sumdb_lookup_survives_the_log_going_away() {
    let upstream = one_shot_upstream("go.sum database tree\n").await;
    let app = sumdb_app(Some(&upstream)).await;
    let uri = "/proxy/go/sumdb/sum.golang.org/lookup/github.com/pkg/errors@v0.9.1";

    let first = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, first).await.status(), 200);

    let second = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, second).await;
    assert_eq!(
        resp.status(),
        200,
        "an unreachable checksum log must not stop a build that already has the answer"
    );
    let cache = resp
        .headers()
        .get("X-BatleHub-Cache")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(
        cache == "hit" || cache == "stale",
        "the answer must be marked as served from cache, got {cache:?}"
    );
}
