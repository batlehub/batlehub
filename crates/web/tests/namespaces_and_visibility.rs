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
use batlehub_adapters::local_registry::InMemoryLocalRegistry;
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{TeamNamespace, Visibility};
use batlehub_core::ports::TeamNamespacePort;
use batlehub_core::{
    ports::{
        CacheStore, LocalRegistryBackend, PackageRepository, RegistryClient, StorageBackend,
        UserTokenRepository,
    },
    services::{
        new_hot_lock, AdminService, HotConfig, LocalRegistryService, ProxyMetrics, ProxyService,
        RegistryPolicy,
    },
};
use batlehub_web::RegistryModeMap;

// ── App factories for team namespace + visibility ─────────────────────────────

/// Build a minimal admin-only test app with a `TeamNamespacePort` registered.
/// No proxy registries — only back-office endpoints are exercised.
async fn make_app_with_ns_store(
    ns_store: Arc<dyn TeamNamespacePort>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let registries: HashMap<String, Arc<dyn RegistryClient>> = HashMap::new();
    let policies: HashMap<String, Arc<RegistryPolicy>> = HashMap::new();
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
    let access_config = access_config_for(&[]);
    let registry_map = registry_map_for(&[]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();

    finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults::default(),
        ns_store,
        test_auth_providers(),
    )
    .await
}

/// Build a local Cargo registry app wired with a `TeamNamespacePort`.
///
/// The `LocalRegistryService` uses the same store instance, so mutations made
/// through the back-office API are visible to the publish/download handlers in
/// the same test.
async fn make_ns_cargo_app(
    ns_store: Arc<dyn TeamNamespacePort>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_ns_cargo_app_with_backend(ns_store, Arc::new(InMemoryLocalRegistry::new())).await
}

/// Like `make_ns_cargo_app` but also returns the `InMemoryTeamNamespaceStore`
/// pre-wired to the same backend, so tests can pre-seed namespace claims and
/// `list_packages_in_namespace` can actually see published packages.
async fn make_ns_cargo_app_seeded(
    claims: Vec<TeamNamespace>,
) -> (
    Arc<InMemoryTeamNamespaceStore>,
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
) {
    let backend: Arc<InMemoryLocalRegistry> = Arc::new(InMemoryLocalRegistry::new());
    let ns_store =
        InMemoryTeamNamespaceStore::with_backend(backend.clone() as Arc<dyn LocalRegistryBackend>);
    for claim in claims {
        ns_store.claim_namespace(claim).await.unwrap();
    }
    let app = make_ns_cargo_app_with_backend(
        Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>,
        backend,
    )
    .await;
    (ns_store, app)
}

async fn make_ns_cargo_app_with_backend(
    ns_store: Arc<dyn TeamNamespacePort>,
    backend: Arc<InMemoryLocalRegistry>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        "local-cargo".to_owned(),
        FixedRegistry::new("cargo") as Arc<dyn RegistryClient>,
    )]
    .into();
    // Grant anonymous `source:read` so the registry RBAC allows cargo `download`
    // (which requires `source:read`); these tests isolate the per-package
    // *visibility* axis, which is enforced independently inside the local read.
    let policies: HashMap<String, Arc<RegistryPolicy>> = [(
        "local-cargo".to_owned(),
        Arc::new(rbac_policy_anon_source(repo_dyn.clone()).0),
    )]
    .into();

    let local_svc = Arc::new(LocalRegistryService {
        backend: backend.clone(),
        storage: storage.clone(),
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
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: Some(Arc::clone(&ns_store)),
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    });

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
            registries,
            policies,
            ..Default::default()
        }),
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
    let access_config = access_config(&[], &["local-cargo"]);
    let registry_map = registry_map_for(&[("local-cargo", "cargo")]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();
    let mode_map = RegistryModeMap::default();
    mode_map.insert("local-cargo".to_owned(), RegistryMode::Local);

    finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        cargo_indexes,
        ConfigureAppDefaults::default(),
        ns_store,
        team_ns_auth_providers(),
    )
    .await
}

// ── Namespace back-office endpoint tests ─────────────────────────────────────

#[actix_web::test]
async fn ns_list_empty_returns_200_with_empty_array() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

/// RFC 0004-bis A6: the claim list says how many packages each namespace holds.
///
/// `count_packages_in_namespace` had been on the port since it was written with
/// exactly one caller — the *delete* confirmation. The list showed a row per
/// claim with no way to tell a namespace holding four hundred packages from an
/// abandoned one, which is the question an operator opens this page with.
#[actix_web::test]
async fn ns_list_states_a_package_count_per_claim() {
    let store = InMemoryTeamNamespaceStore::new();
    let store_dyn: Arc<dyn TeamNamespacePort> = store.clone();
    let app = make_app_with_ns_store(store_dyn).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "frontend", "group_id": "team-fe"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;

    // Zero is an answer, and it is the one that distinguishes an abandoned
    // claim — so it has to be present, not omitted.
    assert_eq!(body[0]["package_count"], serde_json::json!(0));
}

#[actix_web::test]
async fn ns_claim_returns_204() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "frontend", "group_id": "team-fe"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
}

#[actix_web::test]
async fn ns_claim_shows_in_list() {
    let store = InMemoryTeamNamespaceStore::new();
    let store_dyn: Arc<dyn TeamNamespacePort> = store.clone();
    let app = make_app_with_ns_store(store_dyn).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "prefix": "backend",
            "group_id": "team-be",
            "claimed_by": "alice"
        }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let list = body.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["prefix"], "backend");
    assert_eq!(list[0]["group_id"], "team-be");
    assert_eq!(list[0]["claimed_by"], "alice");
}

#[actix_web::test]
async fn ns_claim_duplicate_returns_409() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(Arc::clone(&store)).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "lib", "group_id": "team-a"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "lib", "group_id": "team-b"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 409);
}

#[actix_web::test]
async fn ns_release_returns_204_and_removes_claim() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(Arc::clone(&store)).await;

    // Claim first.
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "ui", "group_id": "team-ui"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    // Release.
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/my-reg/namespaces/ui")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    // Should be gone from list.
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    let body: Value = read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

#[actix_web::test]
async fn ns_release_nonexistent_returns_204() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/my-reg/namespaces/does-not-exist")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);
}

#[actix_web::test]
async fn ns_release_with_slash_in_prefix() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(Arc::clone(&store)).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "org/team", "group_id": "g1"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    // DELETE with slash in prefix — the wildcard route must capture it.
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/my-reg/namespaces/org/team")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    let body: Value = read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

#[actix_web::test]
async fn ns_list_multiple_registries_are_isolated() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(Arc::clone(&store)).await;

    for (reg, prefix) in [("reg-a", "lib"), ("reg-b", "core"), ("reg-a", "util")] {
        let req = TestRequest::post()
            .uri(&format!("/api/v1/admin/registries/{reg}/namespaces"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({"prefix": prefix, "group_id": "g"}))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 204);
    }

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/reg-a/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    let body: Value = read_body_json(resp).await;
    let list = body.as_array().unwrap();
    assert_eq!(
        list.len(),
        2,
        "reg-a should have exactly 2 namespace claims"
    );
    // Sorted by prefix ascending.
    assert_eq!(list[0]["prefix"], "lib");
    assert_eq!(list[1]["prefix"], "util");
}

#[actix_web::test]
async fn ns_list_requires_admin() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn ns_claim_requires_admin() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"prefix": "x", "group_id": "g"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn ns_release_requires_admin() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/my-reg/namespaces/x")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn ns_claim_empty_prefix_returns_400() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "", "group_id": "g"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn ns_claim_empty_group_id_returns_400() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/my-reg/namespaces")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"prefix": "lib", "group_id": ""}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

// ── Visibility back-office endpoint tests ─────────────────────────────────────

#[actix_web::test]
async fn visibility_get_default_is_public() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["visibility"], "public");
}

// Visibility CRUD tests use make_ns_cargo_app so the package can be published first.
// PgTeamNamespaceStore::set_visibility operates on existing local_packages rows, so
// the package must exist before visibility can be set.

#[actix_web::test]
async fn visibility_set_internal_and_get() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;
    publish_and_get_name(&app, "my-pkg", "1.0.0").await;

    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/local-cargo/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"visibility": "internal"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/local-cargo/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["visibility"], "internal");
}

#[actix_web::test]
async fn visibility_set_team_and_get() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;
    publish_and_get_name(&app, "my-pkg", "1.0.0").await;

    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/local-cargo/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"visibility": "team"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/local-cargo/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["visibility"], "team");
}

#[actix_web::test]
async fn visibility_downgrade_team_to_public() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;
    publish_and_get_name(&app, "my-pkg", "1.0.0").await;

    for vis in ["team", "public"] {
        let req = TestRequest::put()
            .uri("/api/v1/admin/registries/local-cargo/packages/my-pkg/visibility")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({"visibility": vis}))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 204);
    }

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/local-cargo/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["visibility"], "public");
}

#[actix_web::test]
async fn visibility_set_invalid_value_returns_400() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/my-reg/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"visibility": "secret"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn visibility_get_requires_admin() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn visibility_set_requires_admin() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(store).await;
    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/my-reg/packages/my-pkg/visibility")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"visibility": "internal"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn visibility_slash_package_name_works() {
    let store: Arc<dyn TeamNamespacePort> = InMemoryTeamNamespaceStore::new();
    let app = make_app_with_ns_store(Arc::clone(&store)).await;

    // Set visibility for a package whose name contains slashes.
    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/my-reg/packages/frontend/utils/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"visibility": "internal"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/my-reg/packages/frontend/utils/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["visibility"], "internal");
}

// ── Namespace publish-enforcement tests (Cargo local registry) ────────────────

#[actix_web::test]
async fn cargo_publish_to_claimed_namespace_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    // Claim "internal" prefix for group "team-alpha".
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "internal".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    // NS_PLAIN_USER_TOKEN has no groups -> blocked.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .set_payload(make_publish_payload("internal/utils", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn cargo_publish_to_claimed_namespace_allows_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "internal".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    // NS_MEMBER_TOKEN has group "team-alpha" -> allowed.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .set_payload(make_publish_payload("internal/utils", "1.0.0"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "team member must be allowed to publish");
}

#[actix_web::test]
async fn cargo_publish_to_unclaimed_namespace_allows_any_user() {
    let ns_store = InMemoryTeamNamespaceStore::new(); // no claims
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .set_payload(make_publish_payload("any/package", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn cargo_admin_can_publish_to_any_claimed_namespace() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "secured".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    // ADMIN_TOKEN bypasses namespace gate.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_payload(make_publish_payload("secured/core", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn cargo_publish_anonymous_still_blocked_in_ns_mode() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .set_payload(make_publish_payload("any/pkg", "1.0.0"))
        .to_request();
    // Blocked by the base role check (User required), not namespace check.
    assert_eq!(call_service(&app, req).await.status(), 403);
}

// ── Visibility download tests (Cargo local registry) ─────────────────────────

/// Publish a crate and return its name/version.
async fn publish_and_get_name(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    name: &str,
    version: &str,
) {
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_payload(make_publish_payload(name, version))
        .to_request();
    let status = call_service(app, req).await.status();
    assert_eq!(status, 200, "pre-test publish must succeed");
}

#[actix_web::test]
async fn cargo_download_public_package_allows_anonymous() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "my-crate", "1.0.0").await;
    // Public visibility (default) -> anonymous download allowed.
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/my-crate/1.0.0/download")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn cargo_download_internal_package_blocks_anonymous() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "my-crate", "1.0.0").await;
    // Set to internal directly via the store.
    ns_store
        .set_visibility("local-cargo", "my-crate", Visibility::Internal)
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/my-crate/1.0.0/download")
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn cargo_download_internal_package_allows_authenticated_user() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "my-crate", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "my-crate", Visibility::Internal)
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/my-crate/1.0.0/download")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn cargo_download_team_package_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    // Claim the namespace so check_visibility can find the owning group.
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "secured".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    // Admin publishes so the publish gate is bypassed.
    publish_and_get_name(&app, "secured/pkg", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "secured/pkg", Visibility::Team)
        .await
        .unwrap();

    // NS_PLAIN_USER_TOKEN has no groups.
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/secured%2Fpkg/1.0.0/download")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn cargo_download_team_package_allows_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "secured".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "secured/pkg", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "secured/pkg", Visibility::Team)
        .await
        .unwrap();

    // NS_MEMBER_TOKEN has group "team-alpha".
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/secured%2Fpkg/1.0.0/download")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn cargo_download_admin_bypasses_team_visibility() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "secret-crate", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "secret-crate", Visibility::Team)
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/secret-crate/1.0.0/download")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── Visibility index tests (sparse Cargo index endpoint) ──────────────────────

#[actix_web::test]
async fn cargo_index_internal_blocks_anonymous() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "my-lib", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "my-lib", Visibility::Internal)
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/my/li/my-lib")
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn cargo_index_internal_allows_user() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "my-lib", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "my-lib", Visibility::Internal)
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/my/li/my-lib")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    // Index returns 200 with newline-delimited JSON entries.
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn cargo_index_team_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    // Claim the exact package name as the namespace prefix (exact-match rule).
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "priv-tool".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    // Admin publishes (bypasses namespace gate).
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;
    publish_and_get_name(&app, "priv-tool", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "priv-tool", Visibility::Team)
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/pr/iv/priv-tool")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn cargo_index_team_allows_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "priv-tool".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;
    publish_and_get_name(&app, "priv-tool", "1.0.0").await;
    ns_store
        .set_visibility("local-cargo", "priv-tool", Visibility::Team)
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/pr/iv/priv-tool")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn cargo_index_public_package_visible_to_user() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "open-crate", "1.0.0").await;
    // Default visibility is public — no visibility set needed.

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/op/en/open-crate")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── Visibility via back-office API (round-trip) ───────────────────────────────

// Use 'team' visibility so that an authenticated-but-non-member user
// (NS_PLAIN_USER_TOKEN) is blocked by the visibility check itself, not by
// the registry-level RBAC layer (anonymous has no registry access in
// make_ns_cargo_app regardless of visibility, so anonymous-blocks are
// ambiguous about which layer fired).
#[actix_web::test]
async fn visibility_set_via_api_then_download_blocked() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    // Claim the namespace so check_visibility can resolve the owning group.
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "lib-x".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "lib-x", "2.0.0").await;

    // Set to 'team' via back-office API.
    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/local-cargo/packages/lib-x/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"visibility": "team"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    // Authenticated non-member is blocked by visibility (not by RBAC — they have
    // User role and registry access, but are not in group "team-alpha").
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/lib-x/2.0.0/download")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);

    // Team member can download.
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/lib-x/2.0.0/download")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn visibility_set_to_public_after_internal_reopens_access() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(Arc::clone(&ns_store) as Arc<dyn TeamNamespacePort>).await;

    publish_and_get_name(&app, "lib-y", "1.0.0").await;

    // Set internal.
    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/local-cargo/packages/lib-y/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"visibility": "internal"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    // Re-open to public.
    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/local-cargo/packages/lib-y/visibility")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"visibility": "public"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 204);

    // Anonymous download should work again (but blocked by RBAC, not visibility).
    // Test with plain user to avoid registry-level RBAC:
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/lib-y/1.0.0/download")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── /api/v1/me/namespaces endpoint ────────────────────────────────────────────

#[actix_web::test]
async fn me_namespaces_returns_only_caller_groups_namespaces() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    // Claim for the caller's group.
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "team-pkg".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    // Claim for a different group — must NOT appear in the response.
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "other-pkg".to_owned(),
            group_id: "team-beta".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    let req = TestRequest::get()
        .uri("/api/v1/me/namespaces")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let namespaces = body.as_array().unwrap();
    assert_eq!(namespaces.len(), 1);
    assert_eq!(namespaces[0]["prefix"], "team-pkg");
    assert_eq!(namespaces[0]["group_id"], "team-alpha");
}

#[actix_web::test]
async fn me_namespaces_returns_empty_for_user_with_no_groups() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "team-pkg".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    let req = TestRequest::get()
        .uri("/api/v1/me/namespaces")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[actix_web::test]
async fn me_namespaces_requires_authentication() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    let req = TestRequest::get().uri("/api/v1/me/namespaces").to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

// ── /api/v1/me/namespaces/{registry}/{prefix}/packages endpoint ───────────────

#[actix_web::test]
async fn me_namespace_packages_lists_published_packages() {
    let (_, app) = make_ns_cargo_app_seeded(vec![TeamNamespace {
        registry: "local-cargo".to_owned(),
        prefix: "internal".to_owned(),
        group_id: "team-alpha".to_owned(),
        claimed_by: None,
        separator: '/',
    }])
    .await;

    // Publish two packages under the namespace.
    for name in &["internal/lib-a", "internal/lib-b"] {
        let req = TestRequest::put()
            .uri("/proxy/local-cargo/api/v1/crates/new")
            .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
            .set_payload(make_publish_payload(name, "1.0.0"))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 200);
    }

    let req = TestRequest::get()
        .uri("/api/v1/me/namespaces/local-cargo/internal/packages")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 2);
    let pkgs = body["items"].as_array().unwrap();
    assert_eq!(pkgs.len(), 2);
    let names: Vec<&str> = pkgs.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"internal/lib-a"));
    assert!(names.contains(&"internal/lib-b"));
    assert_eq!(pkgs[0]["visibility"], "public");
}

#[actix_web::test]
async fn me_namespace_packages_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-cargo".to_owned(),
            prefix: "internal".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_cargo_app(ns_store as Arc<dyn TeamNamespacePort>).await;

    let req = TestRequest::get()
        .uri("/api/v1/me/namespaces/local-cargo/internal/packages")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn me_namespace_packages_admin_can_query_any_namespace() {
    let (_, app) = make_ns_cargo_app_seeded(vec![TeamNamespace {
        registry: "local-cargo".to_owned(),
        prefix: "internal".to_owned(),
        group_id: "team-alpha".to_owned(),
        claimed_by: None,
        separator: '/',
    }])
    .await;

    // Publish as member.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .set_payload(make_publish_payload("internal/lib-c", "0.1.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // Admin queries — not a member of team-alpha but should still get results.
    let req = TestRequest::get()
        .uri("/api/v1/me/namespaces/local-cargo/internal/packages")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
}

#[actix_web::test]
async fn me_namespace_packages_pagination() {
    let (_, app) = make_ns_cargo_app_seeded(vec![TeamNamespace {
        registry: "local-cargo".to_owned(),
        prefix: "paged".to_owned(),
        group_id: "team-alpha".to_owned(),
        claimed_by: None,
        separator: '/',
    }])
    .await;

    // Publish three packages.
    for i in 0..3u8 {
        let req = TestRequest::put()
            .uri("/proxy/local-cargo/api/v1/crates/new")
            .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
            .set_payload(make_publish_payload(&format!("paged/lib-{i}"), "1.0.0"))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 200);
    }

    // Page 0, size 2 → 2 results.
    let req = TestRequest::get()
        .uri("/api/v1/me/namespaces/local-cargo/paged/packages?page=0&per_page=2")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 3);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);

    // Page 1, size 2 → 1 result.
    let req = TestRequest::get()
        .uri("/api/v1/me/namespaces/local-cargo/paged/packages?page=1&per_page=2")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 3);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
}
