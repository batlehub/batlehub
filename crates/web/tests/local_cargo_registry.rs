//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_config::schema::RegistryMode;
// `list_owners` is a trait method; the ownership tests below read the store back.
use batlehub_core::ports::OwnershipPort;

// ── config.json ───────────────────────────────────────────────────────────────

#[actix_web::test]
async fn local_cargo_config_returns_dl_and_api_url() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/config.json")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(
        body["dl"].as_str().unwrap().contains("/proxy/local-cargo/"),
        "dl must contain registry path"
    );
    assert!(
        body["api"].as_str().unwrap().contains("/proxy/local-cargo"),
        "api field must be present for local mode"
    );
}

#[actix_web::test]
async fn hybrid_cargo_config_returns_api_url() {
    let app = make_local_registry_app(RegistryMode::Hybrid).await;
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/config.json")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(
        body["api"].as_str().is_some(),
        "api field must be present for hybrid mode"
    );
}

// ── cargo publish ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn cargo_publish_user_can_publish() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("my-crate", "0.1.0"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(
        body["warnings"].is_object(),
        "response must have warnings shape"
    );
}

/// cargo sends `Authorization: <token>` with no scheme (registry web API);
/// measured against cargo 1.98 in tests/heavy/cargo.sh, where a publish that
/// carried the admin token was refused as anonymous.
#[actix_web::test]
async fn cargo_publish_with_a_bare_token_authenticates() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", USER_TOKEN))
        .set_payload(make_publish_payload("bare-crate", "0.1.0"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn cargo_publish_traversal_version_returns_400() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("my-crate", "../../etc/x"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn cargo_publish_duplicate_version_returns_409() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("dup-crate", "1.0.0"))
        .to_request();
    let first = call_service(&app, req).await;
    assert_eq!(first.status(), 200);

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("dup-crate", "1.0.0"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 409);
}

#[actix_web::test]
async fn cargo_publish_anonymous_returns_403() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        // no Authorization header
        .set_payload(make_publish_payload("my-crate", "0.1.0"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn cargo_publish_proxy_mode_registry_returns_404() {
    // `cargo` registry in make_app uses mode=Proxy (default) — publish must be rejected
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::put()
        .uri("/proxy/cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("my-crate", "0.1.0"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── sparse index ──────────────────────────────────────────────────────────────

#[actix_web::test]
async fn local_cargo_index_unknown_crate_returns_404() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/my/cr/my-crate")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn local_cargo_index_returns_entry_after_publish() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("idx-crate", "0.1.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/id/x-/idx-crate")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let raw = read_body(resp).await;
    let entry: Value = serde_json::from_slice(&raw).expect("index line must be valid JSON");
    assert_eq!(entry["name"], "idx-crate");
    assert_eq!(entry["vers"], "0.1.0");
    assert!(
        entry["cksum"]
            .as_str()
            .map(|s| s.len() == 64)
            .unwrap_or(false),
        "cksum must be 64-char hex SHA-256"
    );
}

// ── download ─────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn local_cargo_download_unknown_returns_404() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/no-crate/1.0.0/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn local_cargo_download_returns_artifact_after_publish() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("dl-crate", "0.1.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/dl-crate/0.1.0/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    assert_eq!(body.as_ref(), b"fake-crate-content");
}

// ── yank / unyank ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn cargo_yank_user_can_yank() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("yank-crate", "1.0.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-cargo/api/v1/crates/yank-crate/1.0.0/yank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["ok"], true);
}

#[actix_web::test]
async fn cargo_unyank_user_can_unyank() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("yank-crate", "1.0.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-cargo/api/v1/crates/yank-crate/1.0.0/yank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/yank-crate/1.0.0/unyank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["ok"], true);
}

// ── deprecate / unlist ────────────────────────────────────────────────────────

#[actix_web::test]
async fn unlist_hides_from_index_but_keeps_download() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("unlist-crate", "1.0.0"))
        .to_request();
    call_service(&app, req).await;

    // Sharded sparse-index path for a name >= 4 chars: first2/next2/name.
    let index_uri = "/proxy/local-cargo/registry/un/li/unlist-crate";

    // Present in the index before unlisting.
    let req = TestRequest::get()
        .uri(index_uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    assert!(String::from_utf8_lossy(&body).contains("unlist-crate"));

    // Unlist (admin).
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/local-cargo/unlist")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "unlist-crate", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // Gone from the index (no visible versions → 404 in local mode).
    let req = TestRequest::get()
        .uri(index_uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);

    // But still downloadable by exact coordinate.
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/unlist-crate/1.0.0/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // Relist restores it.
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/local-cargo/relist")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"name": "unlist-crate", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::get()
        .uri(index_uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn deprecate_keeps_version_listed_with_message() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("dep-crate", "1.0.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/local-cargo/deprecate")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "name": "dep-crate", "version": "1.0.0", "message": "use newer-crate instead"
        }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // Still listed, and the index line carries the deprecation message.
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/de/p-/dep-crate")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("dep-crate"));
    assert!(text.contains("use newer-crate instead"));
}

#[actix_web::test]
async fn deprecate_requires_admin() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/registries/local-cargo/deprecate")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"name": "x", "version": "1.0.0"}))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn cargo_yank_proxy_mode_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::delete()
        .uri("/proxy/cargo/api/v1/crates/my-crate/1.0.0/yank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn cargo_unyank_proxy_mode_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::put()
        .uri("/proxy/cargo/api/v1/crates/my-crate/1.0.0/unyank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── owners ────────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn cargo_owners_returns_404_for_unknown_crate() {
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/api/v1/crates/nonexistent/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn cargo_owners_returns_publisher_after_publish() {
    let app = make_local_registry_app(RegistryMode::Local).await;

    // USER_TOKEN → user_id = "user-1" in test_auth_providers
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("owned-crate", "0.1.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/api/v1/crates/owned-crate/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let users = body["users"].as_array().expect("users array");
    assert!(!users.is_empty());
    assert_eq!(users[0]["login"], "user-1");
}

// ── hybrid mode ───────────────────────────────────────────────────────────────

#[actix_web::test]
async fn hybrid_cargo_index_serves_locally_published_crate() {
    let app = make_local_registry_app(RegistryMode::Hybrid).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("hybrid-crate", "0.1.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/registry/hy/br/hybrid-crate")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let raw = read_body(resp).await;
    let entry: Value = serde_json::from_slice(&raw).expect("index JSON");
    assert_eq!(entry["name"], "hybrid-crate");
}

#[actix_web::test]
async fn hybrid_cargo_download_prefers_local_artifact() {
    let app = make_local_registry_app(RegistryMode::Hybrid).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("hybrid-crate", "0.2.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-cargo/hybrid-crate/0.2.0/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    assert_eq!(body.as_ref(), b"fake-crate-content");
}

// ── Ownership management (`cargo owner --add` / `--remove`) ───────────────────
//
// These routes gate on ownership itself, which made the unowned case subtle:
// `can_publish` returns `true` for a crate nobody owns, because that is how a
// first publish claims a name. Reusing it here meant "nobody owns this" was
// read as "anybody may take it" — and since nothing upstream rejects an
// unauthenticated request, "anybody" included callers with no credentials.
//
// The rule these tests pin: ownership is created *only* by first publish.
// This route edits an existing owner list and never establishes one.

#[actix_web::test]
async fn cargo_add_owners_unauthenticated_is_refused() {
    let (app, ownership, _grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;

    // No Authorization header, and a crate nobody owns.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/acme-internal-auth/owners")
        .set_json(serde_json::json!({ "users": ["mallory"] }))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(
        resp.status(),
        401,
        "an anonymous caller must not be able to claim an unowned crate"
    );
    assert!(
        ownership
            .list_owners("local-cargo", "acme-internal-auth")
            .await
            .unwrap()
            .is_empty(),
        "no owner row may be created by a refused request"
    );
}

#[actix_web::test]
async fn cargo_add_owners_on_unowned_crate_is_refused_even_when_authenticated() {
    let (app, ownership, _grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/never-published/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "users": ["user-1"] }))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(
        resp.status(),
        403,
        "ownership is established by publishing, not by claiming through this route"
    );
    assert!(ownership
        .list_owners("local-cargo", "never-published")
        .await
        .unwrap()
        .is_empty());
}

#[actix_web::test]
async fn cargo_first_publish_registers_the_publisher_as_owner() {
    let (app, ownership, _grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("owned-crate", "0.1.0"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let owners = ownership
        .list_owners("local-cargo", "owned-crate")
        .await
        .unwrap();
    assert_eq!(
        owners.len(),
        1,
        "first publish is the one path that creates ownership"
    );
    assert_eq!(owners[0].principal_id, "user-1");
}

#[actix_web::test]
async fn cargo_add_owners_lets_an_existing_owner_delegate() {
    let (app, ownership, _grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;

    // Publish first: that is what makes user-1 the owner.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("delegated-crate", "0.1.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/delegated-crate/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "users": ["colleague"] }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "an owner may still grant to others");

    let owners = ownership
        .list_owners("local-cargo", "delegated-crate")
        .await
        .unwrap();
    assert!(owners.iter().any(|o| o.principal_id == "colleague"));
}

#[actix_web::test]
async fn cargo_add_owners_refuses_a_non_owner_of_an_owned_crate() {
    let (app, _ownership, _grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;

    // user-1 publishes and therefore owns it.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("someone-elses", "0.1.0"))
        .to_request();
    call_service(&app, req).await;

    // The admin token is a different principal ("admin"), and ownership is not
    // role-based: being an admin does not make you an owner on this route.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/someone-elses/owners")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "users": ["mallory"] }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn cargo_remove_owners_unauthenticated_is_refused() {
    let (app, ownership, _grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("keepme", "0.1.0"))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-cargo/api/v1/crates/keepme/owners")
        .set_json(serde_json::json!({ "users": ["user-1"] }))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 401);
    assert_eq!(
        ownership
            .list_owners("local-cargo", "keepme")
            .await
            .unwrap()
            .len(),
        1,
        "the owner must survive an unauthenticated removal attempt"
    );
}

// ── RFC 0015 §10 rule 9 — ownership and its grants stay in step ──────────────
//
// Ownership rows *are* package-tier grants: `releases:publish`, `owners:read`
// and `owners:write` on the one package. Migration 042 moved the rows that
// existed and a first publish wrote the grant for new ones — and nothing else
// did, so `cargo owner --add` and `cargo owner --remove` wrote `package_owners`
// alone and the two stores diverged from the first owner change on any estate.
// A removed owner kept `releases:publish` and `owners:write` on the package
// permanently, and `explain` reported it as live.
//
// Asserted through the real routes rather than on the projection alone, because
// what went wrong was never the arithmetic — it was that four call sites did not
// perform it.

/// The verbs written for `subject` on a package's node, if any.
async fn owner_grant(
    grants: &batlehub_adapters::in_memory::InMemoryGrantRepository,
    package: &str,
    subject: &str,
) -> Option<Vec<batlehub_core::entities::Action>> {
    use batlehub_core::entities::SubjectMatcher;
    use batlehub_core::ports::{GrantRepository, NodeKind};

    let want = SubjectMatcher::parse(subject).expect("a subject form");
    GrantRepository::grants_on_node(grants, "local-cargo", NodeKind::Package, package)
        .await
        .unwrap()
        .into_iter()
        .find(|g| g.subject == want)
        .map(|g| g.actions)
}

/// Publish `name` as `user-1`, which is what makes them its owner.
async fn publish_owned<S: TestService>(app: &S, name: &str) {
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload(name, "0.1.0"))
        .to_request();
    assert_eq!(
        call_service(app, req).await.status(),
        200,
        "{name} must publish, or the ownership assertions below are vacuous"
    );
}

/// `cargo owner --add` writes the delegate's package-tier grant.
#[actix_web::test]
async fn cargo_add_owners_writes_the_package_tier_grant() {
    let (app, ownership, grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;
    publish_owned(&app, "delegating").await;

    // The control: the publisher's own grant is there, so a missing one below is
    // the delegation and not the fixture.
    assert!(owner_grant(&grants, "delegating", "user:user-1")
        .await
        .is_some());
    assert!(
        owner_grant(&grants, "delegating", "user:colleague")
            .await
            .is_none(),
        "nobody has delegated yet"
    );

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/delegating/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "users": ["colleague"] }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let actions = owner_grant(&grants, "delegating", "user:colleague")
        .await
        .expect("the delegate's grant must follow the owner row");
    for verb in batlehub_core::services::ownership_grants::OWNERSHIP_ACTIONS {
        assert!(
            actions.contains(verb),
            "{verb} must be projected: {actions:?}"
        );
    }
    // …and the owner row and the grant agree about who is an owner.
    let owners = ownership
        .list_owners("local-cargo", "delegating")
        .await
        .unwrap();
    assert!(owners.iter().any(|o| o.principal_id == "colleague"));
}

/// `cargo owner --remove` takes it back.
///
/// The half that was missing for longest, and the one with a consequence: a
/// removed owner whose grant outlives them holds `releases:publish` and
/// `owners:write` on the package for good.
#[actix_web::test]
async fn cargo_remove_owners_takes_the_package_tier_grant_back() {
    let (app, ownership, grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;
    publish_owned(&app, "revoking").await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/revoking/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "users": ["colleague"] }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
    assert!(
        owner_grant(&grants, "revoking", "user:colleague")
            .await
            .is_some(),
        "the control: the grant has to exist before removing it can prove anything"
    );

    let req = TestRequest::delete()
        .uri("/proxy/local-cargo/api/v1/crates/revoking/owners")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "users": ["colleague"] }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    assert!(
        owner_grant(&grants, "revoking", "user:colleague")
            .await
            .is_none(),
        "a removed owner must not keep releases:publish and owners:write on the package"
    );
    assert!(
        ownership
            .list_owners("local-cargo", "revoking")
            .await
            .unwrap()
            .iter()
            .all(|o| o.principal_id != "colleague"),
        "…and the two stores must agree about it"
    );
    // The remaining owner is untouched: removal is per subject, not per node.
    assert!(owner_grant(&grants, "revoking", "user:user-1")
        .await
        .is_some());
}

/// A refused `--add` writes no grant either.
///
/// The projection runs after the inner store, so a call the store refuses never
/// reaches it — asserted rather than assumed, because "the mutation happened"
/// and "the request returned 200" are different facts and the write half of
/// §11.1 exists because a handler can do one without the other.
#[actix_web::test]
async fn a_refused_owner_change_writes_no_grant() {
    let (app, _ownership, grants) = make_local_cargo_ownership_app(RegistryMode::Local).await;
    publish_owned(&app, "guarded").await;

    // The admin token is a different principal, and ownership is not role-based.
    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/guarded/owners")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "users": ["mallory"] }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);

    assert!(
        owner_grant(&grants, "guarded", "user:mallory")
            .await
            .is_none(),
        "a refused request must leave no trace in either store"
    );
}
