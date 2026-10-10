//! What a version *listing* writes to the audit trail.
//!
//! A listing is not a download, and once every ecosystem's listing routes go
//! through `ProxyService` it happens a great deal more often than one: a
//! `cargo build` over a 400-crate dependency graph is 400 listing fetches on
//! the hottest path in the system. RFC 0006 §4.5 splits the two outcomes:
//!
//! - an **allowed** listing moves a per-registry counter and writes no row,
//!   because a row per listing is volume with no question behind it;
//! - a **denied** listing writes exactly one row, with the identity, the
//!   coordinate and the refusal reason, because a denial is a security event
//!   that has to be inspectable one at a time;
//! - an artifact **download** keeps its own row, unchanged.
//!
//! The first of those is the regression test for the volume problem, and is
//! what fails loudly if someone reinstates a per-request row.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use batlehub_config::schema::RegistryMode;
use serde_json::Value;

/// Every audit row currently in the log, newest-first, as the admin console
/// would read them.
async fn audit_rows<S>(app: &S) -> Vec<Value>
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::get()
        .uri("/api/v1/admin/audit-log?per_page=100")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(app, req).await).await;
    body["items"].as_array().cloned().unwrap_or_default()
}

/// A packument read an identity is allowed must leave the audit trail alone.
///
/// This is the assertion that keeps the volume fix in place: it fails the
/// moment anyone reinstates an `AccessEvent` on the allowed listing path.
#[actix_web::test]
async fn an_allowed_listing_writes_no_audit_row_and_moves_the_counter() {
    let parts = local_registry_app_parts("local-npm", "npm", RegistryMode::Proxy, None);
    let metrics = Arc::clone(&parts.proxy_svc.metrics);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    assert!(
        audit_rows(&app).await.is_empty(),
        "the log starts empty, or the assertion below proves nothing"
    );

    for _ in 0..3 {
        let req = TestRequest::get()
            .uri("/proxy/local-npm/lodash")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request();
        assert!(call_service(&app, req).await.status().is_success());
    }

    assert!(
        audit_rows(&app).await.is_empty(),
        "three listings wrote audit rows; a listing transfers no bytes and there \
         is one of these per dependency in every build"
    );
    assert_eq!(
        metrics.all().get("local-npm").unwrap().listing_reads(),
        3,
        "the counter is the durable record now, so it has to actually move"
    );
}

/// A refusal is filed individually — with who, what, and why — because there
/// are few of them and each one is a question an operator will ask.
#[actix_web::test]
async fn a_denied_listing_writes_exactly_one_view_metadata_row() {
    let parts = local_registry_app_parts("local-npm", "npm", RegistryMode::Proxy, None);
    let metrics = Arc::clone(&parts.proxy_svc.metrics);
    {
        // The denial comes from the **hierarchy**, not from a fixture `RbacRule`.
        // §5.1 took that rule out of the chain `build_policy` assembles, and
        // `authorize_listing` no longer runs one — so a policy carrying it
        // describes a mechanism production does not have (§13.14).
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.grants = [(
            "local-npm".to_owned(),
            Arc::new(fixture_grants(
                "local-npm",
                "npm",
                &RegistryMode::Proxy,
                &rbac_policy_deny_anonymous_perms(),
            )),
        )]
        .into();
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let req = TestRequest::get()
        .uri("/proxy/local-npm/lodash")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403, "anonymous must not read this registry");

    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 1, "one denial, one row: {rows:?}");
    assert_eq!(
        rows[0]["action"], "viewmetadata",
        "a listing that transferred no bytes must not read as a download"
    );
    assert_eq!(rows[0]["result"]["outcome"], "denied");
    assert!(
        rows[0]["result"]["reason"]
            .as_str()
            .is_some_and(|r| !r.is_empty()),
        "the refusal reason is the point of keeping the row: {}",
        rows[0]
    );
    assert_eq!(rows[0]["package_id"]["name"], "lodash");

    assert_eq!(
        metrics.all().get("local-npm").unwrap().listing_reads(),
        0,
        "a refused listing is not a read"
    );
}

/// The half that did not change: an artifact download is still one row each,
/// with the identity that pulled it.
#[actix_web::test]
async fn an_artifact_download_still_writes_its_own_row() {
    let parts = local_registry_app_parts("local-npm", "npm", RegistryMode::Proxy, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let req = TestRequest::get()
        .uri("/proxy/local-npm/lodash/1.0.0/tarball")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());

    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 1, "downloads are still filed one at a time");
    assert_eq!(rows[0]["action"], "download");
    assert_eq!(rows[0]["result"]["outcome"], "allowed");
    assert_eq!(rows[0]["user_id"], "admin");
}

/// The marketplace documents render from metadata instead of going through
/// `proxy_document`, and so moved no counter at all — `jbmarket` was absent
/// from the per-registry cost ranking. Each one is a listing like any other.
#[actix_web::test]
async fn marketplace_listings_move_the_counter() {
    for (kind, uri) in [
        (
            "jetbrains-marketplace",
            "/proxy/market/api/plugins/org.example.plugin/updates",
        ),
        ("vscode-marketplace", "/proxy/market/api/example/ext"),
    ] {
        let parts = local_registry_app_parts("market", kind, RegistryMode::Proxy, None);
        let metrics = Arc::clone(&parts.proxy_svc.metrics);
        let app =
            build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

        let req = TestRequest::get()
            .uri(uri)
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request();
        call_service(&app, req).await;

        assert_eq!(
            metrics.all().get("market").unwrap().listing_reads(),
            1,
            "{kind}: {uri} resolved a listing and counted nothing"
        );
    }
}
