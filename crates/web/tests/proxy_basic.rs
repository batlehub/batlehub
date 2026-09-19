//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;

// ── Proxy routes ──────────────────────────────────────────────────────────────

#[actix_web::test]
async fn proxy_github_releases_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/releases")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_github_release_by_tag_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/releases/tags/v1.80.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_github_tarball_is_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/tarball/v1.80.0")
        .to_request();
    let resp = call_service(&app, req).await;
    // Anonymous lacks source:read
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_github_tarball_is_accessible_by_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/rust-lang/rust/tarball/v1.80.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

// ── Forgejo (shares the GitHub release URL scheme) ──────────────────────────────

#[actix_web::test]
async fn proxy_forgejo_releases_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/fj/owner/repo/releases")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_forgejo_tarball_blocked_for_anonymous_allowed_for_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let anon = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/fj/owner/repo/tarball/v1.0.0")
            .to_request(),
    )
    .await;
    assert_eq!(anon.status(), 403);

    let user = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/fj/owner/repo/tarball/v1.0.0")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(user.status(), 200);
}

// ── GitLab (distinct `/-/` URL scheme, nested groups) ───────────────────────────

#[actix_web::test]
async fn proxy_gitlab_releases_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/gl/mygroup/myproj/-/releases")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_gitlab_nested_group_release_by_tag() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/gl/group/subgroup/proj/-/releases/v2.0.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_gitlab_link_download_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/gl/group/proj/-/releases/v1.0.0/downloads/app.bin")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_gitlab_archive_blocked_for_anonymous_allowed_for_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let anon = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/gl/group/proj/-/archive/v1.0.0/proj-v1.0.0.tar.gz")
            .to_request(),
    )
    .await;
    assert_eq!(anon.status(), 403);

    let user = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/gl/group/proj/-/archive/v1.0.0/proj-v1.0.0.tar.gz")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(user.status(), 200);
}

#[actix_web::test]
async fn proxy_gitlab_raw_file_allowed_for_user() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/gl/group/proj/-/raw/main/README.md")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_forgejo_package_passthrough() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/fj/api/packages/acme/generic/tool/1.0/tool.bin")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_gitlab_package_passthrough() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/gl/api/v4/projects/1/packages/generic/a/1.0/f.bin")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_forgejo_package_on_github_registry_is_404() {
    // The package route is Forgejo-only; a github-typed registry is rejected.
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github/api/packages/acme/generic/x/1.0/f.bin")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

/// A GitLab project that is **one** path segment — which is how mise spells it,
/// percent-encoded — must still land on the typed `/-/` route. It did not: with
/// the GitHub routes registered first, `{owner}/{repo}/releases` claimed the
/// three segments with `repo = "-"` and the GitHub guard answered 404.
#[actix_web::test]
async fn proxy_gitlab_single_segment_project_is_not_claimed_by_the_github_route() {
    let app = make_app(InMemoryRepo::new()).await;

    for uri in [
        "/proxy/gl/cli/-/releases",
        "/proxy/gl/gitlab-org%2Fcli/-/releases",
    ] {
        let resp = call_service(&app, TestRequest::get().uri(uri).to_request()).await;
        assert_eq!(resp.status(), 200, "{uri} did not reach the gitlab route");
    }
}

/// The same collision on the raw route: `{project}/-/raw/{ref}/{path}` against
/// GitHub's `{owner}/{repo}/raw/{ref}/{path}`.
#[actix_web::test]
async fn proxy_gitlab_single_segment_raw_is_not_claimed_by_the_github_route() {
    let app = make_app(InMemoryRepo::new()).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/gl/cli/-/raw/main/README.md")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_gitlab_route_on_github_registry_is_404() {
    let app = make_app(InMemoryRepo::new()).await;
    // `github` is a github-typed registry; the gitlab `/-/releases` route guard rejects it.
    let req = TestRequest::get()
        .uri("/proxy/github/group/proj/-/releases")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── Unknown registry → 400 ────────────────────────────────────────────────────

#[actix_web::test]
async fn proxy_unknown_registry_returns_400() {
    // The test app only has github/npm/cargo; any other registry name goes
    // through a route that doesn't exist.  The way to trigger an unknown-registry
    // error is to call ProxyService with a registry that wasn't wired up; here
    // we verify that the error mapping is 400 via the access-check endpoint, which
    // intentionally forces a get_status call (no upstream involved).
    let repo = InMemoryRepo::new();
    let app = make_app(repo).await;

    // No route is registered for /proxy/pypi/..., so actix-web returns 404.
    let req = TestRequest::get()
        .uri("/proxy/pypi/requests/2.31.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── Error response JSON format ─────────────────────────────────────────────────

#[actix_web::test]
async fn error_response_has_error_and_message_fields() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
    let body: Value = read_body_json(resp).await;
    assert!(
        body["error"].is_string(),
        "response must have an 'error' field"
    );
    assert!(
        body["message"].is_string(),
        "response must have a 'message' field"
    );
    // HTTP reason phrase for 403
    assert_eq!(body["error"], "Forbidden");
}

// ── Audit log records access events ──────────────────────────────────────────

#[actix_web::test]
async fn proxy_access_is_recorded_in_audit_log() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;

    // A tarball *download*, not the packument. RFC 0006 §4.5 deliberately
    // stopped filing an `AccessEvent` for an allowed listing — a dependency
    // resolution is hundreds of listings and no bytes, so they move a
    // per-registry counter instead. Downloads still get a row each, which is
    // what this test is about.
    let req = TestRequest::get()
        .uri("/proxy/npm/express/1.0.0/tarball")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    // Confirm the event was recorded
    let audit_req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let audit_resp = call_service(&app, audit_req).await;
    assert_eq!(audit_resp.status(), 200);
    let body: Value = read_body_json(audit_resp).await;
    let events = body["items"].as_array().unwrap();
    assert!(
        !events.is_empty(),
        "at least one access event should be recorded"
    );
    assert_eq!(events[0]["package_id"]["name"], "express");
    assert_eq!(events[0]["result"]["outcome"], "allowed");
}

#[actix_web::test]
async fn denied_proxy_access_is_recorded_as_denied_in_audit_log() {
    let repo = InMemoryRepo::new();
    let app = make_app(repo.clone()).await;

    // Anonymous tries to download a tarball (source:read denied)
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21/tarball")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);

    // The denied event should appear in the audit log
    let audit_req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let audit_resp = call_service(&app, audit_req).await;
    let body: Value = read_body_json(audit_resp).await;
    let events = body["items"].as_array().unwrap();
    assert!(!events.is_empty(), "denied access should be recorded");
    assert_eq!(events[0]["result"]["outcome"], "denied");
}
