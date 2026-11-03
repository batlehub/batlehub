//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use chrono::Utc;
use serde_json::Value;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_core::{
    entities::{AccessEvent, PackageId, PackageStatus, Role},
    ports::PackageRepository,
};

// ── /api/v1/me ────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn me_without_auth_returns_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get().uri("/api/v1/me").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["role"], "anonymous");
    assert!(body["user_id"].is_null());
}

#[actix_web::test]
async fn me_with_admin_token_returns_admin_identity() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/me")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["role"], "admin");
    assert_eq!(body["user_id"], "admin");
}

#[actix_web::test]
async fn me_with_user_token_returns_user_identity() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/me")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["role"], "user");
    assert_eq!(body["user_id"], "user-1");
}

#[actix_web::test]
async fn me_with_invalid_token_falls_back_to_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/me")
        .insert_header(("Authorization", "Bearer not-a-real-token"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["role"], "anonymous");
}

// ── /api/v1/packages ──────────────────────────────────────────────────────────

#[actix_web::test]
async fn packages_list_is_empty_on_fresh_repo() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get().uri("/api/v1/packages").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["total"], 0);
}

#[actix_web::test]
async fn packages_list_shows_packages_after_access() {
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("npm", "lodash", "4.17.21");
    repo.record_access(AccessEvent::allowed_download(
        pkg.clone(),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get().uri("/api/v1/packages").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["name"], "lodash");
}

// ── /api/v1/packages/access ───────────────────────────────────────────────────

#[actix_web::test]
async fn access_check_returns_true_for_available_package() {
    let app = make_app(InMemoryRepo::new()).await;
    // An npm download asks `source:read`, which this fixture grants users and
    // not anonymous callers: `why` agrees with the download for both.
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=npm&name=lodash&version=4.17.21")
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=npm&name=lodash&version=4.17.21")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["can_access"], true);
    assert!(body["reason"].is_null());
}

#[actix_web::test]
async fn access_check_returns_false_for_blocked_package() {
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("npm", "evil-pkg", "1.0.0");
    repo.set_status(
        &pkg,
        PackageStatus::Blocked {
            reason: "security vulnerability".to_owned(),
            blocked_by: "admin".to_owned(),
            blocked_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/packages/access?registry=npm&name=evil-pkg&version=1.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["can_access"], false);
    assert_eq!(body["headline"], "refused");
    // RFC 0040 T2: the administrator's note is theirs; a non-admin caller
    // learns that it is a block, and its code, not why.
    assert_eq!(body["reason"], "blocked by an administrator");
    let block = body["causes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "block")
        .unwrap();
    assert_eq!(block["state"], "refuse");
    assert_eq!(block["reason_code"], "BLOCK_LIST");
    assert!(
        !body.to_string().contains("security vulnerability"),
        "{body}"
    );
}

#[actix_web::test]
async fn access_check_answers_an_inaccessible_registry_as_not_found() {
    // make_app doesn't register or grant access to "pypi" for any role.
    // RFC 0040 T1: the `200 {reason: "registry not accessible"}` this used to
    // answer confirmed the registry exists; now it is the not-found an unknown
    // registry gets, byte for byte.
    let app = make_app(InMemoryRepo::new()).await;
    let ask = |reg: &str| {
        TestRequest::get()
            .uri(&format!(
                "/api/v1/packages/access?registry={reg}&name=evil-pkg&version=1.0.0"
            ))
            .to_request()
    };
    let hidden = call_service(&app, ask("pypi")).await;
    assert_eq!(hidden.status(), 404);
    let hidden = read_body(hidden).await;
    let unknown = call_service(&app, ask("no-such-registry")).await;
    assert_eq!(unknown.status(), 404);
    assert_eq!(hidden, read_body(unknown).await);
}

// ── /api/v1/admin/packages ────────────────────────────────────────────────────

#[actix_web::test]
async fn admin_packages_returns_403_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn admin_packages_returns_403_for_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn admin_packages_returns_200_for_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["items"].is_array());
    assert!(body["total"].is_number());
    assert_eq!(body["page"], 0);
    assert_eq!(body["per_page"], 50);
}

// ── /api/v1/admin/packages/block & /unblock ───────────────────────────────────

#[actix_web::test]
async fn admin_block_returns_403_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/block")
        .set_json(serde_json::json!({
            "registry": "npm", "name": "pkg", "version": "1.0.0", "reason": "test"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn admin_block_succeeds_for_admin() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/block")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm", "name": "lodash", "version": "4.17.21",
            "reason": "supply-chain risk"
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["success"], true);
}

#[actix_web::test]
async fn admin_block_then_proxy_returns_403() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;

    // Block via API
    let block_req = TestRequest::post()
        .uri("/api/v1/admin/packages/block")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm", "name": "lodash", "version": "4.17.21",
            "reason": "blocked for test"
        }))
        .to_request();
    let block_resp = call_service(&app, block_req).await;
    assert_eq!(block_resp.status(), 200);

    // Attempt proxy fetch — should be denied
    let proxy_req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21")
        .to_request();
    let proxy_resp = call_service(&app, proxy_req).await;
    assert_eq!(proxy_resp.status(), 403);
}

#[actix_web::test]
async fn admin_unblock_restores_proxy_access() {
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("npm", "lodash", "4.17.21");

    // Pre-block
    repo.set_status(
        &pkg,
        PackageStatus::Blocked {
            reason: "test".to_owned(),
            blocked_by: "admin".to_owned(),
            blocked_at: Utc::now(),
        },
    )
    .await
    .unwrap();

    let app = make_app(repo).await;

    // Unblock via API
    let unblock_req = TestRequest::post()
        .uri("/api/v1/admin/packages/unblock")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "npm", "name": "lodash", "version": "4.17.21"
        }))
        .to_request();
    let unblock_resp = call_service(&app, unblock_req).await;
    assert_eq!(unblock_resp.status(), 200);

    // Proxy should succeed now
    let proxy_req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21")
        .to_request();
    let proxy_resp = call_service(&app, proxy_req).await;
    assert_eq!(proxy_resp.status(), 200);
}

// ── /api/v1/admin/audit-log ───────────────────────────────────────────────────

#[actix_web::test]
async fn audit_log_returns_403_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn audit_log_returns_events_for_admin() {
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("npm", "lodash", "4.17.21");
    repo.record_access(AccessEvent::allowed_download(
        pkg,
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    let app = make_app(repo).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 1);
    let events = body["items"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["package_id"]["name"], "lodash");
}

/// A caller who may reach **nothing** sees nothing — not everything.
///
/// `PackageFilter::registries` is a scope, and every implementation of it reads
/// an empty vector as *unfiltered*: `prepare_registries_param` binds `NULL` and
/// the SQL is `$7::text[] IS NULL OR ps.registry = ANY($7)`; the in-memory
/// repository is `filter.registries.is_empty() || contains(…)`. The handler
/// derives that vector from `accessible_registries_for`, so a caller entitled to
/// no registry at all produced the same empty vector as a caller asking for no
/// restriction — and was handed the entire catalogue, block reasons and access
/// counts included.
///
/// The sibling `/api/v1/explore/packages` grew this guard first; this endpoint
/// shipped without it. The paired assertion below keeps the empty answer honest:
/// the same rows *are* served once the registries are reachable, so this tests
/// the gate and not an empty repository.
#[actix_web::test]
async fn packages_list_denied_everywhere_returns_nothing_rather_than_all_of_them() {
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::allowed_download(
        PackageId::new("npm", "lodash", "4.17.21"),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    // An access config that grants no registry to any tier.
    let app =
        make_app_with_defaults_and_access(repo, Default::default(), Some(access_config(&[], &[])))
            .await;

    for auth in [None, Some(USER_TOKEN), Some(ADMIN_TOKEN)] {
        let mut req = TestRequest::get().uri("/api/v1/packages");
        if let Some(token) = auth {
            req = req.insert_header(("Authorization", bearer(token)));
        }
        let resp = call_service(&app, req.to_request()).await;
        assert_eq!(resp.status(), 200, "{auth:?}");
        let body: Value = read_body_json(resp).await;
        assert_eq!(body["items"], serde_json::json!([]), "{auth:?}: {body}");
        assert_eq!(
            body["total"], 0,
            "{auth:?}: an empty accessible set must scope to nothing, not to everything: {body}"
        );
    }

    // …and the same row is served once the registry is reachable.
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::allowed_download(
        PackageId::new("npm", "lodash", "4.17.21"),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();
    let allowed = make_app(repo).await;
    let resp = call_service(
        &allowed,
        TestRequest::get().uri("/api/v1/packages").to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 1, "{body}");
}

/// The named-registry path is gated too, and for the same reason.
#[actix_web::test]
async fn packages_list_named_registry_outside_the_accessible_set_returns_nothing() {
    let repo = InMemoryRepo::new();
    repo.record_access(AccessEvent::allowed_download(
        PackageId::new("npm", "lodash", "4.17.21"),
        Some("user-1".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    // `cargo` is reachable; `npm` — where the row lives — is not.
    let app = make_app_with_defaults_and_access(
        repo,
        Default::default(),
        Some(access_config(&["cargo"], &["cargo"])),
    )
    .await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/packages?registry=npm")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 0, "{body}");
}
