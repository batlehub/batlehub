//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body_json, TestRequest};
use serde_json::Value;

use std::sync::Arc;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_core::entities::{AccessAction, AccessEvent, EventFilter, PackageId, Role};
use batlehub_core::ports::PackageRepository;

/// Every event of one action in a repository.
async fn recorded(repo: &Arc<InMemoryRepo>, action: AccessAction) -> Vec<AccessEvent> {
    repo.list_events(EventFilter {
        actions: vec![action],
        limit: 100,
        ..Default::default()
    })
    .await
    .unwrap()
}

// ── /api/v1/admin/health ──────────────────────────────────────────────────────

#[actix_web::test]
async fn health_non_admin_returns_403() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/health")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn health_without_activity_returns_zeroed_stats() {
    // The health handler now sources package/event stats from the
    // `PackageRepository` port (backed by `InMemoryRepo` in tests) instead of
    // a raw `PgPool`, so — unlike the old raw-SQL handler, which special-cased
    // "no pool" into an early `[]` — it always returns one entry per
    // configured registry, with zeroed stats when nothing has been recorded.
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/health")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let entries = body.as_array().expect("array response");
    assert!(!entries.is_empty(), "expected one entry per registry");
    for entry in entries {
        assert_eq!(entry["package_count"], serde_json::json!(0));
        assert_eq!(entry["cached_artifact_count"], serde_json::json!(0));
        assert_eq!(entry["pulls_last_hour"], serde_json::json!(0));
        assert_eq!(entry["pulls_last_day"], serde_json::json!(0));
        assert_eq!(entry["recent_errors"], serde_json::json!([]));
        assert!(entry["last_pull_at"].is_null());
    }
}

/// RFC 0004-bis A2: mode and beta-channel state on the health row.
///
/// "Cached artifacts: 0, last pull: never" reads identically for a broken proxy
/// and for a healthy `local` registry that has nothing to pull by definition.
/// Without the mode there is no way to tell those apart from this response, and
/// the console was fetching a second endpoint to find out.
#[actix_web::test]
async fn health_states_the_mode_and_beta_channel_of_each_registry() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/health")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;

    for entry in body.as_array().expect("array response") {
        // The test app configures no explicit modes, so every registry takes
        // the default — which must still be *stated* rather than omitted.
        assert_eq!(entry["mode"], "proxy", "every row names its mode");
        assert_eq!(entry["beta_channel_enabled"], serde_json::json!(false));
    }
}

// ── /api/v1/admin/registries/{registry}/clear-cache ──────────────────────────

#[actix_web::test]
async fn clear_cache_non_admin_returns_403() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/clear-cache")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn clear_cache_unknown_registry_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/no-such-registry/clear-cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn clear_cache_known_registry_returns_200() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/clear-cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["cleared"].is_number());
}

/// **The bluntest of the four `cache:evict` surfaces**, and until now the only
/// destructive endpoint in the server that left nothing behind at all.
///
/// Registry-scoped, and recorded even when it cleared nothing: "who emptied the
/// cache" has to survive the answer being "there was nothing in it".
#[actix_web::test]
async fn clear_cache_records_a_registry_scoped_event() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/npm/clear-cache")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let events = recorded(&repo, AccessAction::CacheClear).await;
    assert_eq!(events.len(), 1);
    let coord = events[0].package_id.as_ref().unwrap();
    assert_eq!(coord.registry, "npm");
    assert!(
        coord.name.is_empty() && coord.version.is_empty(),
        "a prefix delete never knew the coordinates"
    );
    assert!(
        recorded(&repo, AccessAction::CacheEvict).await.is_empty(),
        "and it is not a pile of single-artifact drops"
    );
}

/// `POST /packages/invalidate` is the older spelling of
/// `DELETE /registries/{r}/cache`; two surfaces for one operation must not
/// produce two different trails.
#[actix_web::test]
async fn invalidate_records_the_same_event_the_cache_delete_does() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/invalidate")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm", "name": "lodash", "version": "4.17.21", "artifact": null
        }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let events = recorded(&repo, AccessAction::CacheEvict).await;
    assert_eq!(events.len(), 1);
    let coord = events[0].package_id.as_ref().unwrap();
    assert_eq!(
        (coord.name.as_str(), coord.version.as_str()),
        ("lodash", "4.17.21")
    );
}

// ── /api/v1/admin/packages/bulk-block ────────────────────────────────────────

#[actix_web::test]
async fn bulk_block_non_admin_returns_403() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-block")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "items": [] }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn bulk_block_admin_empty_items_returns_200() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-block")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "items": [] }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["succeeded_count"], 0);
}

#[actix_web::test]
async fn bulk_block_admin_one_item_succeeds() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-block")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "items": [
                { "registry": "npm", "name": "lodash", "version": "4.17.21",
                  "artifact": null, "reason": "bulk test" }
            ]
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["succeeded_count"], 1);
    assert_eq!(body["failed_count"], 0);
}

// ── /api/v1/admin/packages/bulk-unblock ──────────────────────────────────────

#[actix_web::test]
async fn bulk_unblock_non_admin_returns_403() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-unblock")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "items": [] }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn bulk_unblock_admin_returns_200() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;

    // Block first
    let block_req = TestRequest::post()
        .uri("/api/v1/admin/packages/block")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm", "name": "lodash", "version": "4.17.21", "reason": "test"
        }))
        .to_request();
    call_service(&app, block_req).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-unblock")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "items": [
                { "registry": "npm", "name": "lodash", "version": "4.17.21", "artifact": null }
            ]
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["succeeded_count"], 1);
}

// ── /api/v1/admin/packages/invalidate ────────────────────────────────────────

#[actix_web::test]
async fn invalidate_non_admin_returns_403() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/invalidate")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm", "name": "lodash", "version": "4.17.21", "artifact": null
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn invalidate_admin_returns_200() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/invalidate")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm", "name": "lodash", "version": "4.17.21", "artifact": null
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["success"], true);
}

// ── /api/v1/admin/packages/bulk-delete ───────────────────────────────────────
//
// The instance-tier sibling of bulk-block: it removes the package records and
// purges whatever the proxy had cached for them.

#[actix_web::test]
async fn bulk_delete_non_admin_returns_403() {
    // Empty items on purpose. The verb is resolved on the instance tier rather
    // than on the registries the body names, so a request that names nothing is
    // still authorized — a check a caller can skip by sending less is not a check.
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-delete")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "items": [] }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn bulk_delete_admin_empty_items_returns_200() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-delete")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "items": [] }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["succeeded_count"], 0);
    assert_eq!(body["failed_count"], 0);
}

#[actix_web::test]
async fn bulk_delete_removes_the_known_package_and_reports_the_unknown_one() {
    // One of each in a single request: a bulk endpoint that stopped at the first
    // failure would leave the caller unable to tell which half went through.
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::allowed_download(
        PackageId::new("npm", "lodash", "4.17.21"),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();
    let app = make_app(repo.clone()).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/bulk-delete")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "items": [
                { "registry": "npm", "name": "lodash", "version": "4.17.21", "artifact": null },
                { "registry": "npm", "name": "never-seen", "version": "1.0.0", "artifact": null }
            ]
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["succeeded_count"], 1);
    assert_eq!(body["failed_count"], 1);
    assert_eq!(body["failures"][0]["name"], "never-seen");
    assert_eq!(body["failures"][0]["error"], "package not found");

    // The record is gone from the catalogue, not merely reported as deleted.
    let listed = call_service(
        &app,
        TestRequest::get().uri("/api/v1/packages").to_request(),
    )
    .await;
    let listed: Value = read_body_json(listed).await;
    assert_eq!(listed["total"], 0, "the deleted package is still listed");

    // And the deletion is on the audit trail, as every admin mutation is.
    let deletes = recorded(&repo, AccessAction::Delete).await;
    assert_eq!(deletes.len(), 1);
    let deleted = deletes[0]
        .package_id
        .as_ref()
        .expect("the event names a package");
    assert_eq!(deleted.name, "lodash");
}
