//! `?` and `#` in a coordinate are a `400` at the edge.
//!
//! actix decodes `%3F` / `%23` in a path parameter and `validate_path_safe`
//! lets both through. Interpolated into an upstream URL they start a query or
//! a fragment, so the upstream answered about another coordinate than the one
//! the rules judged: a block on `pkg@1.0.0` did not match `1.0.0?`, and the
//! upstream served `1.0.0`. `server/src/name_injection_probe.rs` checks every
//! kind's client; this checks the request never gets that far.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::call_service;

#[actix_web::test]
async fn a_delimiter_in_the_name_or_version_is_a_400_not_a_download() {
    let app = proxy_registry_app("npm-r", "npm").await;
    // The clean coordinate is served: the 400s below are about the delimiter.
    let ok = call_service(&app, admin_get("/proxy/npm-r/pkg/1.0.0/tarball")).await;
    assert_eq!(ok.status(), 200);
    for uri in [
        "/proxy/npm-r/pkg%3Fx=1/1.0.0/tarball",
        "/proxy/npm-r/pkg%23x/1.0.0/tarball",
        "/proxy/npm-r/pkg/1.0.0%3Fx=1/tarball",
        "/proxy/npm-r/pkg/1.0.0%23x/tarball",
    ] {
        let resp = call_service(&app, admin_get(uri)).await;
        assert_eq!(resp.status(), 400, "{uri}");
    }
}

/// The bypass this closes: a block names `pkg@1.0.0`, and `1.0.0?` was not
/// it, while the upstream URL built from it fetched `1.0.0`.
#[actix_web::test]
async fn a_blocked_version_is_not_reached_by_appending_a_delimiter() {
    let app = proxy_registry_app("npm-b", "npm").await;
    block_version(&app, "npm-b", "pkg", "1.0.0").await;
    let blocked = call_service(&app, admin_get("/proxy/npm-b/pkg/1.0.0/tarball")).await;
    assert_eq!(blocked.status(), 403);
    for uri in [
        "/proxy/npm-b/pkg/1.0.0%3F/tarball",
        "/proxy/npm-b/pkg/1.0.0%23/tarball",
    ] {
        let resp = call_service(&app, admin_get(uri)).await;
        assert_eq!(resp.status(), 400, "{uri}");
    }
}

/// Git allows `#` in a tag, and the forge clients encode every URL segment,
/// so a forge release whose tag holds one is still served. `?` is in no git
/// ref name and stays a `400` there too.
#[actix_web::test]
async fn a_forge_tag_holding_a_hash_is_still_served() {
    let app = proxy_registry_app("gh", "github").await;
    let resp = call_service(&app, admin_get("/proxy/gh/o/r/tarball/release%232")).await;
    assert_eq!(resp.status(), 200);
    let resp = call_service(
        &app,
        admin_get("/proxy/gh/o/r/releases/download/release%232/tool.tar.gz"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let resp = call_service(&app, admin_get("/proxy/gh/o/r/tarball/release%3F2")).await;
    assert_eq!(resp.status(), 400);
}
