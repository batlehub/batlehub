//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, init_service, read_body, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::EventFilter;
use batlehub_core::entities::{AccessAction, AccessEvent, PackageId, Role};
use batlehub_core::ports::{NoopWarmCoordinator, PackageRepository, StorageBackend, StorageMeta};
use batlehub_core::services::{EvictionConfig, EvictionService, ProxyMetrics, WarmingService};
use batlehub_web::handlers::back_office::ops::eviction::EvictionServiceMap;
use batlehub_web::handlers::back_office::ops::warming::WarmingServiceMap;
use batlehub_web::AuthMiddlewareFactory;
use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};

// ── Bulk operations ───────────────────────────────────────────────────────────

#[actix_web::test]
async fn bulk_yank_returns_200_with_empty_packages() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/bulk-yank")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"packages": []}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    // response: { "processed": 0, "succeeded": 0, "failed": [] }
    assert_eq!(body["processed"], 0);
    assert!(body["failed"].as_array().unwrap().is_empty());
}

#[actix_web::test]
async fn bulk_yank_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/bulk-yank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"packages": []}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn bulk_delete_returns_200() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/bulk-delete")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "packages": [{"name": "nonexistent", "version": "1.0.0"}]
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn bulk_unyank_returns_200() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/bulk-unyank")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"packages": []}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── Quota ─────────────────────────────────────────────────────────────────────

use batlehub_adapters::in_memory::InMemoryQuotaRepository;
use batlehub_core::ports::QuotaRepository;
use batlehub_core::services::{AdminService, QuotaService};

/// Minimal app wired with only the four quota endpoints and auth middleware.
async fn make_quota_app(
    quota_svc: Arc<QuotaService>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_quota_app_with(quota_svc, None).await
}

/// `make_quota_app`, plus whatever an operator wrote in a top-level `[grants]`
/// block — which is how a control verb becomes delegable at all.
async fn make_quota_app_with(
    quota_svc: Arc<QuotaService>,
    explicit: Option<batlehub_core::entities::GrantMap>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    use batlehub_web::handlers::back_office::ops::quota::{
        get_quota_for_user, list_quota, list_quota_for_registry, reset_quota_for_user,
    };
    let admin_svc = Arc::new(AdminService::new(InMemoryRepo::new()));
    let app = actix_web::App::new()
        // RFC 0015 §4.2 — this app registers only the handlers under test, so the
        // hot lock the control-verb check reads has to be registered with them.
        .app_data(actix_web::web::Data::new(
            batlehub_core::services::hot_config::new_hot_lock(
                batlehub_core::services::hot_config::HotConfig {
                    instance: Some(std::sync::Arc::new(
                        batlehub_core::services::authz::translate::instance_node(explicit.as_ref()),
                    )),
                    ..Default::default()
                },
            ),
        ))
        .app_data(actix_web::web::Data::new(quota_svc))
        .app_data(actix_web::web::Data::new(admin_svc))
        .service(list_quota)
        .service(list_quota_for_registry)
        .service(get_quota_for_user)
        .service(reset_quota_for_user);
    init_service(app.wrap(AuthMiddlewareFactory::new(test_auth_providers()))).await
}

fn empty_quota_svc() -> Arc<QuotaService> {
    Arc::new(QuotaService::new(
        InMemoryQuotaRepository::new(),
        HashMap::new(),
    ))
}

#[actix_web::test]
async fn admin_quota_list_returns_403_for_anonymous() {
    let app = make_quota_app(empty_quota_svc()).await;
    let req = TestRequest::get().uri("/api/v1/admin/quota").to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn admin_quota_list_returns_403_for_non_admin_user() {
    let app = make_quota_app(empty_quota_svc()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/quota")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn admin_quota_list_returns_empty_initially() {
    let app = make_quota_app(empty_quota_svc()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/quota")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

#[actix_web::test]
async fn admin_quota_list_for_registry_returns_empty_initially() {
    let app = make_quota_app(empty_quota_svc()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/quota/cargo")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}

#[actix_web::test]
async fn admin_quota_get_for_user_returns_200_with_zero_usage() {
    let app = make_quota_app(empty_quota_svc()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/quota/cargo/alice")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["user_id"], "alice");
    assert_eq!(body["registry"], "cargo");
    assert_eq!(body["bytes_published"], 0);
    assert_eq!(body["packages_count"], 0);
}

#[actix_web::test]
async fn admin_quota_reset_returns_200() {
    let repo = InMemoryQuotaRepository::new();
    repo.record_publish("alice", "cargo", 1024).await.unwrap();
    let svc = Arc::new(QuotaService::new(repo.clone(), HashMap::new()));
    let app = make_quota_app(svc).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/quota/cargo/alice")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let after = repo.get_usage("alice", "cargo").await.unwrap();
    assert_eq!(after.bytes_published, 0);
}

#[actix_web::test]
async fn admin_quota_list_requires_admin() {
    let app = make_quota_app(empty_quota_svc()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/quota/cargo")
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn admin_quota_reset_requires_admin() {
    let app = make_quota_app(empty_quota_svc()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/quota/cargo/alice")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

// ── The read verb is a read verb ──────────────────────────────────────────────
//
// `admin_quota_reset_requires_admin` above cannot see this: it uses a bare
// `role:user` holding no grant at all, so it passes whether the reset is gated
// on `quota:read` or on `quota:write`. The delegation is the whole return on
// §4.2's decomposition, and it is the delegated caller — not the anonymous one —
// who is standing in front of the wrong verb.

/// A delegate granted `quota:read` reads usage and **cannot reset it**.
#[actix_web::test]
async fn quota_read_delegated_to_a_user_does_not_confer_the_reset() {
    use batlehub_core::entities::{Action, GrantMap, Role, SubjectMatcher};

    let repo = InMemoryQuotaRepository::new();
    repo.record_publish("alice", "cargo", 1024).await.unwrap();
    let svc = Arc::new(QuotaService::new(repo.clone(), HashMap::new()));
    let app = make_quota_app_with(
        svc,
        Some(GrantMap::new().grant(SubjectMatcher::Role(Role::User), [Action::QuotaRead])),
    )
    .await;

    // The read the grant names.
    let req = TestRequest::get()
        .uri("/api/v1/admin/quota/cargo/alice")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        200,
        "a positive control: without this the assertion below passes for the \
         wrong reason, because every request from this caller is refused"
    );

    // The write it does not.
    let req = TestRequest::delete()
        .uri("/api/v1/admin/quota/cargo/alice")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        403,
        "`quota:read` is published as \"read quota usage\"; an operator \
         delegating it to a support engineer is not also handing them the \
         ability to defeat the limit on every user in the registry"
    );
    assert_eq!(
        repo.get_usage("alice", "cargo")
            .await
            .unwrap()
            .bytes_published,
        1024,
        "and the refusal has to happen before the reset, not after it"
    );
}

/// …and `quota:write` is what does confer it.
#[actix_web::test]
async fn quota_write_delegated_to_a_user_is_honoured() {
    use batlehub_core::entities::{Action, GrantMap, Role, SubjectMatcher};

    let repo = InMemoryQuotaRepository::new();
    repo.record_publish("alice", "cargo", 1024).await.unwrap();
    let svc = Arc::new(QuotaService::new(repo.clone(), HashMap::new()));
    let app = make_quota_app_with(
        svc,
        Some(GrantMap::new().grant(SubjectMatcher::Role(Role::User), [Action::QuotaWrite])),
    )
    .await;

    let req = TestRequest::delete()
        .uri("/api/v1/admin/quota/cargo/alice")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
    assert_eq!(
        repo.get_usage("alice", "cargo")
            .await
            .unwrap()
            .bytes_published,
        0
    );
}

// ── Package ownership ─────────────────────────────────────────────────────────

#[actix_web::test]
async fn list_package_owners_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/npm/packages/lodash/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

/// The admin gets past the verb check and is stopped by the missing port, not
/// by authorization — the distinction the operator needs to fix the deployment.
///
/// This assertion used to accept success, client error *and* server error, which
/// is every status there is: the row passed whatever the handler did.
#[actix_web::test]
async fn list_package_owners_returns_503_when_ownership_is_not_configured() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/registries/npm/packages/lodash/owners")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 503);
}

#[actix_web::test]
async fn add_package_owner_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/packages/lodash/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(
            serde_json::json!({"principal_type": "user", "principal_id": "alice", "role": "admin"}),
        )
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn remove_package_owner_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/packages/lodash/owners/user/alice")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn remove_package_owner_returns_503_when_ownership_is_not_configured() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/packages/lodash/owners/user/alice")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 503);
}

/// Add, list, remove, list — the whole administrative ownership surface against
/// a store that is actually wired, which is the only fixture where the three
/// routes do more than refuse.
#[actix_web::test]
async fn package_owner_add_list_remove_round_trip() {
    let (app, _ownership, _grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;
    let owners_uri = "/api/v1/admin/registries/local-cargo/packages/pkg/owners";

    let added = call_service(
        &app,
        TestRequest::post()
            .uri(owners_uri)
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({
                "principal_type": "user", "principal_id": "alice", "role": "maintainer"
            }))
            .to_request(),
    )
    .await;
    assert_eq!(added.status(), 204);

    let listed: Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri(owners_uri)
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    assert_eq!(listed.as_array().expect("owner array").len(), 1);
    assert_eq!(listed[0]["principal_id"], "alice");

    let removed = call_service(
        &app,
        TestRequest::delete()
            .uri(&format!("{owners_uri}/user/alice"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(removed.status(), 204);

    // Removed for real: the listing is the only place an admin can confirm it,
    // and a `204` that leaves the row behind is the failure worth catching.
    let after: Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri(owners_uri)
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    assert!(
        after.as_array().expect("owner array").is_empty(),
        "the owner survived its removal: {after}"
    );
}

// ── Cache invalidation ────────────────────────────────────────────────────────

#[actix_web::test]
async fn invalidate_package_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/invalidate")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"registry": "npm", "name": "lodash", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn invalidate_package_clears_cached_metadata() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/invalidate")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm",
            "name": "lodash",
            "version": "4.17.21"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["success"].as_bool().unwrap_or(false));
}

// ── Cache warming ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn warm_registry_returns_404_when_not_configured() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/warm")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"package": "lodash"}))
        .to_request();
    // Warming map is empty in make_app → 404
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn warm_registry_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/warm")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"package": "lodash"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

fn npm_warming_service(storage: Arc<dyn StorageBackend>) -> Arc<WarmingService> {
    Arc::new(WarmingService {
        client: FixedRegistry::new("npm"),
        storage,
        artifact_meta: NoopArtifactMeta::arc(),
        registry_name: "npm".to_owned(),
        latest_n: 3,
        concurrency: 4,
        coordinator: Arc::new(NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(ProxyMetrics::new(&["npm".to_owned()])),
    })
}

#[actix_web::test]
async fn get_warming_status_requires_admin() {
    let (app, _storage) = make_app_with_warming(WarmingServiceMap::default()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/warming")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn get_warming_status_lists_configured_registries() {
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let warming_map: WarmingServiceMap = [("npm".to_owned(), npm_warming_service(storage))].into();
    let (app, _storage) = make_app_with_warming(warming_map).await;

    let req = TestRequest::get()
        .uri("/api/v1/admin/warming")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let registries = body["registries"].as_array().unwrap();
    assert_eq!(registries.len(), 1);
    assert_eq!(registries[0]["name"], "npm");
    assert_eq!(registries[0]["latest_n"], 3);
    assert_eq!(registries[0]["concurrency"], 4);
}

#[actix_web::test]
async fn warm_registry_rejects_empty_body() {
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let warming_map: WarmingServiceMap = [("npm".to_owned(), npm_warming_service(storage))].into();
    let (app, _storage) = make_app_with_warming(warming_map).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/warm")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn warm_registry_warms_pinned_package_version() {
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let warming_map: WarmingServiceMap =
        [("npm".to_owned(), npm_warming_service(storage.clone()))].into();
    let (app, _storage) = make_app_with_warming(warming_map).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/warm")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"package": "lodash@4.17.21"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["warmed"], 1);
    assert_eq!(body["errors"], 0);
}

#[actix_web::test]
async fn warm_registry_warms_path_and_honours_versions_override() {
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let warming_map: WarmingServiceMap =
        [("npm".to_owned(), npm_warming_service(storage.clone()))].into();
    let (app, _storage) = make_app_with_warming(warming_map).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/warm")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "package": "lodash@4.17.21",
            "path": "extra/asset.tgz",
            "versions": 1,
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["warmed"], 2, "one pinned package + one path");
    assert_eq!(body["errors"], 0);
}

#[actix_web::test]
async fn warm_registry_returns_404_for_unknown_registry() {
    let (app, _storage) = make_app_with_warming(WarmingServiceMap::default()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/does-not-exist/warm")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"package": "lodash"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn warm_registry_accepts_paths_body() {
    let app = make_app(InMemoryRepo::new()).await;
    // The `paths` body must deserialize (the old shape required `package`, which
    // would 400 here). The warming map is empty in make_app, so a valid body
    // routes through to 404 "not configured" rather than a 400 deserialize error.
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/jb/warm")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"paths": ["idea/idea-2026.1.3.tar.gz"]}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── Cache eviction ────────────────────────────────────────────────────────────

/// Every event of one action in a repository, newest first.
async fn recorded(
    repo: &Arc<dyn PackageRepository>,
    action: AccessAction,
) -> Vec<batlehub_core::entities::AccessEvent> {
    repo.list_events(EventFilter {
        actions: vec![action],
        limit: 100,
        ..Default::default()
    })
    .await
    .unwrap()
}

/// A download's audit row carries the caller's address and agent.
///
/// The regression this guards: `ProxyRequest` documented both fields as being
/// "for audit log enrichment", the column existed, the CSV export printed it and
/// `list_pullers` grouped by it — but nothing ever set them, so every row was
/// null and the audit trail could attribute nothing to an address.
#[actix_web::test]
async fn a_downloads_audit_row_carries_the_callers_address_and_agent() {
    let parts = local_registry_app_parts("npm", "npm", RegistryMode::Proxy, None);
    let repo = Arc::clone(&parts.proxy_svc.repo);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let req = TestRequest::get()
        .uri("/proxy/npm/pkg/1.1.0/tarball")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header(("User-Agent", "npm/10.2.4 node/v20.11.0"))
        .peer_addr("203.0.113.9:54321".parse().unwrap())
        .to_request();
    let resp = call_service(&app, req).await;
    assert!(resp.status().is_success(), "{}", resp.status());

    let events = recorded(&repo, AccessAction::Download).await;
    let e = events.first().expect("the download was audited");
    assert_eq!(e.ip_address.as_deref(), Some("203.0.113.9"));
    assert_eq!(e.user_agent.as_deref(), Some("npm/10.2.4 node/v20.11.0"));
}

/// And it is the peer's address, not one the caller asked for.
///
/// With no trusted proxy configured, `X-Forwarded-For` is attacker-supplied:
/// believing it would let any caller write whatever source address it liked into
/// its own audit row, which is the one field an operator reads to find out who
/// pulled something.
#[actix_web::test]
async fn a_forwarded_for_header_cannot_forge_the_audited_address() {
    let parts = local_registry_app_parts("npm", "npm", RegistryMode::Proxy, None);
    let repo = Arc::clone(&parts.proxy_svc.repo);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let req = TestRequest::get()
        .uri("/proxy/npm/pkg/1.1.0/tarball")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header(("X-Forwarded-For", "198.51.100.7"))
        .peer_addr("203.0.113.9:54321".parse().unwrap())
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());

    let events = recorded(&repo, AccessAction::Download).await;
    let e = events.first().expect("the download was audited");
    assert_eq!(
        e.ip_address.as_deref(),
        Some("203.0.113.9"),
        "the peer, never the header"
    );
}

#[actix_web::test]
async fn evict_registry_returns_404_when_not_configured() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/evict")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    // Eviction map is empty in make_app → 404
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn evict_registry_returns_404_for_unknown_registry() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/does-not-exist/evict")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn evict_registry_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/evict")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn evict_registry_runs_configured_service_and_reports_zero_counts() {
    // One strategy configured over a `NoopArtifactMeta` that lists nothing: the
    // run reaches the service and comes back with every counter at zero.
    //
    // A strategy is *required* now — `EvictionConfig::default()` configures
    // none, and a registry with nothing to evict answers `404` rather than a
    // `200 {"total": 0}` that would tell an operator the sweep ran.
    let svc = Arc::new(EvictionService::new(
        NoopArtifactMeta::arc(),
        batlehub_adapters::in_memory::InMemoryStorageBackend::new(),
        EvictionConfig {
            keep_latest_n: Some(1),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    ));
    let eviction_map: EvictionServiceMap = [("npm".to_owned(), svc)].into();
    let (app, _storage) = make_app_with_eviction(eviction_map).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/evict")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 0);
    assert_eq!(body["evicted_ttl"], 0);
    assert_eq!(body["evicted_idle"], 0);
    assert_eq!(body["evicted_old_versions"], 0);
    assert_eq!(body["evicted_lru"], 0);
}

/// A preview through the endpoint: the keys come back, the bytes do not go.
#[actix_web::test]
async fn evict_registry_dry_run_reports_keys_without_deleting() {
    let meta = NoopArtifactMeta::arc();
    let storage: Arc<dyn StorageBackend> =
        batlehub_adapters::in_memory::InMemoryStorageBackend::new();
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    // `keep_latest_n` over a `NoopArtifactMeta` lists nothing, so this asserts
    // the plumbing — the flag reaching the service and the shape coming back —
    // rather than the arithmetic, which `core`'s own suite covers exhaustively.
    let svc = Arc::new(
        EvictionService::new(
            meta,
            storage,
            EvictionConfig {
                keep_latest_n: Some(1),
                registry: "npm".to_owned(),
                ..Default::default()
            },
        )
        .with_audit(repo.clone()),
    );
    let eviction_map: EvictionServiceMap = [("npm".to_owned(), svc)].into();
    let (app, _storage) = make_app_with_eviction_and_repo(eviction_map, repo.clone()).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/evict?dry_run=true")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["dry_run"], true, "{body}");
    assert!(body["evicted_keys"].is_array(), "{body}");

    let events = repo
        .list_events(EventFilter {
            actions: vec![AccessAction::CacheEvictDryRun],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1, "the preview is on the record");
    assert!(
        repo.list_events(EventFilter {
            actions: vec![AccessAction::CacheEvictRun],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap()
        .is_empty(),
        "and never as a run that could have written"
    );
}

/// A live sweep records one registry-scoped event, whoever ran it.
#[actix_web::test]
async fn evict_registry_records_the_run() {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let svc = Arc::new(
        EvictionService::new(
            NoopArtifactMeta::arc(),
            batlehub_adapters::in_memory::InMemoryStorageBackend::new(),
            EvictionConfig {
                keep_latest_n: Some(1),
                registry: "npm".to_owned(),
                ..Default::default()
            },
        )
        .with_audit(repo.clone()),
    );
    let eviction_map: EvictionServiceMap = [("npm".to_owned(), svc)].into();
    let (app, _storage) = make_app_with_eviction_and_repo(eviction_map, repo.clone()).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/evict")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let events = repo
        .list_events(EventFilter {
            actions: vec![AccessAction::CacheEvictRun],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].package_id.as_ref().unwrap().registry, "npm");
}

// ── Cache coherence sweep ─────────────────────────────────────────────────────

/// An eviction service with **no strategy configured** — the state most
/// registries are in, and the one that used to make the coherence sweep
/// unreachable because the map skipped them.
fn coherence_only_map(
    storage: Arc<dyn StorageBackend>,
    repo: Arc<dyn PackageRepository>,
) -> EvictionServiceMap {
    let svc = Arc::new(
        EvictionService::new(
            NoopArtifactMeta::arc(),
            storage,
            EvictionConfig {
                registry: "npm".to_owned(),
                ..Default::default()
            },
        )
        .with_audit(repo),
    );
    [("npm".to_owned(), svc)].into()
}

#[actix_web::test]
async fn coherence_sweep_requires_admin() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/coherence")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn coherence_sweep_returns_404_for_unknown_registry() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/does-not-exist/coherence")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

/// **The point of moving the 404.** Orphaned blobs do not wait for someone to
/// configure a TTL, so the sweep has to be reachable on a registry with no
/// eviction policy — while `/evict`, which really would do nothing, still says
/// so.
#[actix_web::test]
async fn coherence_is_reachable_where_eviction_is_not_configured() {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> =
        batlehub_adapters::in_memory::InMemoryStorageBackend::new();
    let (app, _storage) =
        make_app_with_eviction_and_repo(coherence_only_map(storage, repo.clone()), repo.clone())
            .await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/coherence")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/evict")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        404,
        "a registry with no strategy still has nothing to evict"
    );
}

/// The sweep reports the orphan, deletes nothing on the first pass, and files
/// the run under its own action.
#[actix_web::test]
async fn coherence_sweep_reports_orphans_and_records_the_run() {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> =
        batlehub_adapters::in_memory::InMemoryStorageBackend::new();
    storage
        .store(
            "artifact:npm/orphan/1.0.0",
            Bytes::from_static(b"orphan"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    let (app, _s) = make_app_with_eviction_and_repo(
        coherence_only_map(storage.clone(), repo.clone()),
        repo.clone(),
    )
    .await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/coherence")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["dry_run"], false, "{body}");
    assert_eq!(body["orphaned_deleted"], 0, "first pass defers: {body}");
    assert_eq!(body["first_seen_orphaned"], 1, "{body}");
    assert_eq!(body["first_seen_keys"][0], "artifact:npm/orphan/1.0.0");
    assert!(
        storage.exists("artifact:npm/orphan/1.0.0").await.unwrap(),
        "and the blob is still there"
    );

    assert_eq!(
        recorded(&repo, AccessAction::CacheCoherenceRun).await.len(),
        1
    );
}

/// A preview is filed as a preview, and — the property that matters — does not
/// arm the deletion it describes.
#[actix_web::test]
async fn coherence_sweep_dry_run_is_recorded_and_arms_nothing() {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> =
        batlehub_adapters::in_memory::InMemoryStorageBackend::new();
    storage
        .store(
            "artifact:npm/orphan/1.0.0",
            Bytes::from_static(b"orphan"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    let (app, _s) = make_app_with_eviction_and_repo(
        coherence_only_map(storage.clone(), repo.clone()),
        repo.clone(),
    )
    .await;

    for _ in 0..2 {
        let req = TestRequest::post()
            .uri("/api/v1/admin/registries/npm/coherence?dry_run=true")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request();
        let body: Value = read_body_json(call_service(&app, req).await).await;
        assert_eq!(body["dry_run"], true, "{body}");
        assert_eq!(body["orphaned_deleted"], 0, "{body}");
    }

    // Two previews, then a live run: still a first sighting, so still deferred.
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/coherence")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(
        body["orphaned_deleted"], 0,
        "previewing twice must not delete on the next run: {body}"
    );
    assert!(storage.exists("artifact:npm/orphan/1.0.0").await.unwrap());

    assert_eq!(
        recorded(&repo, AccessAction::CacheCoherenceDryRun)
            .await
            .len(),
        2
    );
    assert_eq!(
        recorded(&repo, AccessAction::CacheCoherenceRun).await.len(),
        1
    );
}

// ── Targeted proxy-cache artifact deletion ────────────────────────────────────

#[actix_web::test]
async fn delete_cached_artifact_requires_admin() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"name": "lodash", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn delete_cached_artifact_returns_404_for_unknown_registry() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/does-not-exist/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "lodash", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn delete_cached_artifact_rejects_missing_name_or_version() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "lodash"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn delete_cached_artifact_rejects_traversal_in_name() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "../etc/passwd", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn delete_cached_artifact_rejects_empty_path() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"path": ""}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn delete_cached_artifact_by_name_version_returns_false_when_absent() {
    let (app, _storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "lodash", "version": "1.0.0"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["deleted"], false);
    assert_eq!(body["artifact_key"], "artifact:npm/lodash/1.0.0");
}

#[actix_web::test]
async fn delete_cached_artifact_by_name_version_deletes_stored_artifact() {
    let (app, storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    storage
        .store(
            "artifact:npm/lodash/1.0.0",
            Bytes::from_static(b"tarball"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "lodash", "version": "1.0.0"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["deleted"], true);
    assert_eq!(body["artifact_key"], "artifact:npm/lodash/1.0.0");

    assert!(!storage.exists("artifact:npm/lodash/1.0.0").await.unwrap());
}

/// Dropping one cached artifact by hand is an operator's decision about one
/// package, so it carries the coordinate — and it is `cache_evict`, never
/// `delete`: the bytes come back on the next request.
#[actix_web::test]
async fn delete_cached_artifact_records_a_cache_eviction() {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let (app, storage) =
        make_app_with_eviction_and_repo(EvictionServiceMap::default(), repo.clone()).await;
    storage
        .store(
            "artifact:npm/lodash/1.0.0",
            Bytes::from_static(b"tarball"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "lodash", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let events = recorded(&repo, AccessAction::CacheEvict).await;
    assert_eq!(events.len(), 1);
    let coord = events[0].package_id.as_ref().unwrap();
    assert_eq!(
        (
            coord.registry.as_str(),
            coord.name.as_str(),
            coord.version.as_str()
        ),
        ("npm", "lodash", "1.0.0")
    );
    assert!(
        recorded(&repo, AccessAction::Delete).await.is_empty(),
        "a cached copy is not the package"
    );
}

/// An artifact that was not cached was not dropped, and must not leave an event
/// saying it was.
#[actix_web::test]
async fn deleting_an_absent_cached_artifact_records_nothing() {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let (app, _storage) =
        make_app_with_eviction_and_repo(EvictionServiceMap::default(), repo.clone()).await;

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "lodash", "version": "1.0.0"}))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["deleted"], false);
    assert!(recorded(&repo, AccessAction::CacheEvict).await.is_empty());
}

/// A path-addressed registry has no name/version, and the event still has to
/// say which file went.
#[actix_web::test]
async fn deleting_a_path_addressed_artifact_records_the_path() {
    let repo: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let (app, storage) =
        make_app_with_eviction_and_repo(EvictionServiceMap::default(), repo.clone()).await;
    storage
        .store(
            "artifact:npm/repo/_/idea/idea-2026.1.3.tar.gz",
            Bytes::from_static(b"blob"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"path": "idea/idea-2026.1.3.tar.gz"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let events = recorded(&repo, AccessAction::CacheEvict).await;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].package_id.as_ref().unwrap().artifact.as_deref(),
        Some("idea/idea-2026.1.3.tar.gz")
    );
}

#[actix_web::test]
async fn delete_cached_artifact_by_path_deletes_stored_artifact() {
    let (app, storage) = make_app_with_eviction(EvictionServiceMap::default()).await;
    storage
        .store(
            "artifact:npm/repo/_/idea/idea-2026.1.3.tar.gz",
            Bytes::from_static(b"binary"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/npm/cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"path": "idea/idea-2026.1.3.tar.gz"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["deleted"], true);
    assert_eq!(
        body["artifact_key"],
        "artifact:npm/repo/_/idea/idea-2026.1.3.tar.gz"
    );
}

// ── Audit log ─────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn audit_log_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn audit_log_returns_200_for_admin_with_empty_events() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    // Might be empty list or paginated response
    assert!(body.is_array() || body.is_object());
}

#[actix_web::test]
async fn audit_log_returns_seeded_events_and_respects_denied_only_filter() {
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::allowed_download(
        PackageId::new("npm", "lodash", "4.17.21"),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();
    repo.record_access(AccessEvent::denied_download(
        PackageId::new("npm", "evil-pkg", "1.0.0"),
        Some("user-1".to_owned()),
        Role::User,
        "blocked".to_owned(),
    ))
    .await
    .unwrap();

    let app = make_app(repo).await;

    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);

    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?denied_only=true")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["result"]["outcome"], "denied");

    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?registry=npm&user_id=user-1&page=0&per_page=1")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 2, "count ignores the page-size limit");
    assert_eq!(
        body["items"].as_array().unwrap().len(),
        1,
        "list respects per_page"
    );
}

/// The filter that makes "what was deleted here" answerable at all.
///
/// Without it the only way to find deletions is to page over the whole trail,
/// which is downloads by three orders of magnitude.
#[actix_web::test]
async fn audit_log_filters_by_action_and_accepts_a_set() {
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::allowed_download(
        PackageId::new("npm", "lodash", "4.17.21"),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();
    for action in [AccessAction::Delete, AccessAction::RetentionReclaim] {
        let mut ev = AccessEvent::allowed_download(
            PackageId::new("npm", "gone", "1.0.0"),
            Some("admin-1".to_owned()),
            Role::Admin,
        );
        ev.action = action;
        repo.record_access(ev).await.unwrap();
    }

    let app = make_app(repo).await;

    // One action, snake_case — the spelling the CSV export and the package
    // timeline hand the operator.
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?action=retention_reclaim")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["action"], "retentionreclaim");

    // The same action in the spelling this very endpoint's JSON just used.
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?action=retentionreclaim")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 1, "both spellings on the wire must parse");

    // Both deletion kinds at once: the actual question an auditor asks.
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?action=delete,retention_reclaim")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);

    // And the count must describe the same set as the rows.
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?action=download")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 1);
}

/// A typo must not read as "nothing happened": the two are indistinguishable to
/// the caller, and one of them is a wrong answer to an audit question.
#[actix_web::test]
async fn audit_log_rejects_an_unknown_action() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?action=deleted")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(
        body.contains("delete"),
        "the error must name the alternatives: {body}"
    );
}

#[actix_web::test]
async fn audit_log_filters_by_package_name() {
    let repo = InMemoryRepo::new();
    for name in ["lodash", "express"] {
        repo.record_access(AccessEvent::allowed_download(
            PackageId::new("npm", name, "1.0.0"),
            Some("user-1".to_owned()),
            Role::User,
        ))
        .await
        .unwrap();
    }
    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?package_name=lodash")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["package_id"]["name"], "lodash");
}

// ── Audit log export ──────────────────────────────────────────────────────────

#[actix_web::test]
async fn export_audit_log_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log/export")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn export_audit_log_defaults_to_json() {
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::allowed_download(
        PackageId::new("npm", "lodash", "4.17.21"),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log/export")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );
    assert!(resp
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("audit-log-"));
    let body: Value = read_body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[actix_web::test]
async fn export_audit_log_supports_csv_format() {
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::denied_download(
        PackageId::new("npm", "evil-pkg", "1.0.0"),
        Some("user-1".to_owned()),
        Role::User,
        "blocked by policy".to_owned(),
    ))
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log/export?format=csv")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/csv");
    let body = read_body(resp).await;
    let csv = String::from_utf8(body.to_vec()).unwrap();
    assert!(csv.starts_with("id,timestamp,user_id"));
    assert!(csv.contains("evil-pkg"));
    assert!(csv.contains("blocked by policy"));
    assert!(csv.contains("denied"));
}

/// An export has to be able to describe the same set the table did — the
/// reasoning `denied_only` already followed — and its `action` column has to be
/// a name you can paste back into `?action=`.
#[actix_web::test]
async fn export_audit_log_honours_the_action_filter_and_writes_parseable_names() {
    let repo = InMemoryRepo::new();
    let mut viewed = AccessEvent::allowed_download(
        PackageId::new("npm", "seen", "1.0.0"),
        Some("user-1".to_owned()),
        Role::User,
    );
    viewed.action = AccessAction::ViewMetadata;
    repo.record_access(viewed).await.unwrap();
    let mut reclaimed = AccessEvent::allowed_download(
        PackageId::new("npm", "gone", "1.0.0"),
        Some("admin-1".to_owned()),
        Role::Admin,
    );
    reclaimed.action = AccessAction::RetentionReclaim;
    repo.record_access(reclaimed).await.unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log/export?format=csv&action=retention_reclaim")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let csv = String::from_utf8(read_body(call_service(&app, req).await).await.to_vec()).unwrap();
    assert!(csv.contains("gone"));
    assert!(
        !csv.contains("seen"),
        "the export must not widen the filter: {csv}"
    );
    assert!(
        csv.contains("retention_reclaim"),
        "the action column must round-trip through ?action=: {csv}"
    );
}

// ── Audit log purge ────────────────────────────────────────────────────────────

#[actix_web::test]
async fn purge_audit_log_requires_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::delete()
        .uri("/api/v1/admin/audit-log?before=2026-01-01T00:00:00Z")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn purge_audit_log_returns_200_with_deleted_count() {
    // `InMemoryPackageRepository` doesn't override `purge_events_before`, so it
    // falls back to the port's default no-op (`Ok(0)`) — only Postgres actually
    // purges. This still exercises the handler's query parsing, admin check,
    // and response shape end to end.
    let app = make_app(InMemoryRepo::new()).await;
    // Avoid a literal `+` in the query string (form-urlencoded decoding turns
    // it into a space), so format with a bare `Z` offset instead of `to_rfc3339`.
    let cutoff = (Utc::now() - ChronoDuration::days(30)).format("%Y-%m-%dT%H:%M:%SZ");
    let req = TestRequest::delete()
        .uri(&format!("/api/v1/admin/audit-log?before={cutoff}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["deleted"], 0);
}

/// An app carrying only the audit handlers, with `explicit` unioned onto the
/// instance node exactly as a top-level `[grants]` block would be.
async fn make_audit_app(
    explicit: batlehub_core::entities::GrantMap,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    use batlehub_web::handlers::back_office::audit::{audit_log, purge_audit_log};

    let admin_svc = Arc::new(AdminService::new(InMemoryRepo::new()));
    let app = actix_web::App::new()
        .app_data(actix_web::web::Data::new(
            batlehub_core::services::hot_config::new_hot_lock(
                batlehub_core::services::hot_config::HotConfig {
                    instance: Some(std::sync::Arc::new(
                        batlehub_core::services::authz::translate::instance_node(Some(&explicit)),
                    )),
                    ..Default::default()
                },
            ),
        ))
        .app_data(actix_web::web::Data::new(admin_svc))
        .service(audit_log)
        .service(purge_audit_log);
    init_service(app.wrap(AuthMiddlewareFactory::new(test_auth_providers()))).await
}

/// **A reviewer who may read the trail may not erase it.**
///
/// `purge_audit_log_requires_admin` above cannot see this — a bare `role:user`
/// holds nothing, so it is refused whichever verb the handler asks for. The
/// delegation is the case that matters: `audit:read` is published as "read the
/// audit log", and an estate granting it to a compliance reviewer is doing the
/// thing §4.2's decomposition exists to make possible. If the purge shared that
/// verb, the reviewer could delete the trail — including the record of their own
/// actions, and including the `audit:purge` event the purge writes, which a
/// second call with the same cutoff removes.
#[actix_web::test]
async fn audit_read_delegated_to_a_user_does_not_confer_the_purge() {
    use batlehub_core::entities::{Action, GrantMap, Role, SubjectMatcher};

    let app = make_audit_app(
        GrantMap::new().grant(SubjectMatcher::Role(Role::User), [Action::AuditRead]),
    )
    .await;

    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        200,
        "the positive control: the grant does reach the read it names"
    );

    let req = TestRequest::delete()
        .uri("/api/v1/admin/audit-log?before=2026-01-01T00:00:00Z")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        403,
        "reading the audit log and destroying it are one grant apart, and the \
         endpoint was `require_admin` before the decomposition — so this is a \
         reduction nobody asked for, not a preserved capability"
    );
}

/// …and `audit:purge` is what does confer it.
#[actix_web::test]
async fn audit_purge_delegated_to_a_user_is_honoured() {
    use batlehub_core::entities::{Action, GrantMap, Role, SubjectMatcher};

    let app = make_audit_app(
        GrantMap::new().grant(SubjectMatcher::Role(Role::User), [Action::AuditPurge]),
    )
    .await;

    let req = TestRequest::delete()
        .uri("/api/v1/admin/audit-log?before=2026-01-01T00:00:00Z")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}
