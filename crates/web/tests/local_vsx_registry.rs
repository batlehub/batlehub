//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_config::schema::RegistryMode;
use batlehub_core::ports::TeamNamespacePort as _;

// ── Local / Hybrid private VS Code extension (openvsx) registry ───────────────

/// Build a test app with a single openvsx registry in the given mode.
/// Registry name is `"local-vsx"`, type `"openvsx"`.
async fn make_local_vsx_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-vsx", "openvsx", mode, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

#[actix_web::test]
async fn vsix_publish_user_can_publish() {
    let app = make_local_vsx_app(RegistryMode::Local).await;
    let req = TestRequest::put()
        .uri("/proxy/local-vsx/my-org.my-ext/1.0.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(b"PK\x03\x04fake-vsix-content".to_vec())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["ok"], true);
}

#[actix_web::test]
async fn vsix_publish_duplicate_returns_409() {
    let app = make_local_vsx_app(RegistryMode::Local).await;

    let payload = b"PK\x03\x04fake-vsix".to_vec();
    for _ in 0..2 {
        let req = TestRequest::put()
            .uri("/proxy/local-vsx/pub.ext/0.1.0/vsix")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .insert_header(("Content-Type", "application/octet-stream"))
            .set_payload(payload.clone())
            .to_request();
        call_service(&app, req).await;
    }

    let req = TestRequest::put()
        .uri("/proxy/local-vsx/pub.ext/0.1.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(payload)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 409);
}

#[actix_web::test]
async fn vsix_publish_anonymous_returns_401() {
    let app = make_local_vsx_app(RegistryMode::Local).await;
    let req = TestRequest::put()
        .uri("/proxy/local-vsx/my-org.my-ext/1.0.0/vsix")
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(b"PK\x03\x04fake".to_vec())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn vsix_publish_proxy_mode_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::put()
        .uri("/proxy/openvsx/my-org.my-ext/1.0.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(b"PK\x03\x04fake".to_vec())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn vsix_download_returns_artifact_after_publish() {
    let app = make_local_vsx_app(RegistryMode::Local).await;
    let vsix_bytes = b"PK\x03\x04fake-vsix-bytes".to_vec();

    let req = TestRequest::put()
        .uri("/proxy/local-vsx/my-org.my-ext/2.0.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(vsix_bytes.clone())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-vsx/my-org.my-ext/2.0.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    assert_eq!(body.as_ref(), vsix_bytes.as_slice());
}

/// The route ends in `vsix`, so a browser saving from it wrote a file called
/// `vsix`. The name here is OpenVSX's own — the one its `…/file/{filename}`
/// route serves the package under — so saving from this proxy and saving from
/// upstream land the same file on disk.
#[actix_web::test]
async fn vsix_download_names_the_package_file() {
    let app = make_local_vsx_app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-vsx/my-org.my-ext/2.0.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(b"PK\x03\x04fake-vsix-bytes".to_vec())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-vsx/my-org.my-ext/2.0.0/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("Content-Disposition")
            .expect("a route ending in a verb must name its file")
            .to_str()
            .unwrap(),
        "attachment; filename=\"my-org.my-ext-2.0.0.vsix\""
    );
}

#[actix_web::test]
async fn vsix_download_unknown_version_returns_404() {
    let app = make_local_vsx_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-vsx/no-pub.no-ext/9.9.9/vsix")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── `openvsx:namespace:claim` over HTTP ──────────────────────────────────────
//
// The verb has core-level tests, and until this section the route did not: it
// went into `WRITE_ROUTE_INVENTORY` as the one write nobody had classified,
// which is exactly the finding class that inventory exists to produce. The
// write matrix cannot hold the row — its fingerprint is `get_versions` for a
// coordinate, and a namespace claim publishes nothing, so both the denial and
// its positive control would fingerprint identically and the control would fail
// by construction. So the row lives here, where the assertion can look at the
// thing the route actually changes: whether the namespace is claimed.

/// An app whose `LocalRegistryService` has a team-namespace store.
///
/// Without one `claim_openvsx_namespace` answers `500` *after* authorizing, so
/// a test using the default factory would see the 403 half correctly and could
/// never see the allowed half at all.
async fn make_vsx_namespace_app() -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    std::sync::Arc<batlehub_adapters::in_memory::InMemoryTeamNamespaceStore>,
) {
    use batlehub_core::services::LocalRegistryService;
    use std::sync::Arc;

    let mut parts = local_registry_app_parts("local-vsx", "openvsx", RegistryMode::Local, None);
    // `new()` already hands back an `Arc`, as the other in-memory stores do.
    let ns = batlehub_adapters::in_memory::InMemoryTeamNamespaceStore::new();

    let cur = parts.local_svc.clone();
    parts.local_svc = Arc::new(LocalRegistryService {
        backend: cur.backend.clone(),
        storage: cur.storage.clone(),
        hot: cur.hot.clone(),
        quota: cur.quota.clone(),
        ownership: cur.ownership.clone(),
        team_namespace: Some(ns.clone() as Arc<dyn batlehub_core::ports::TeamNamespacePort>),
        sbom: cur.sbom.clone(),
        explore_cache: cur.explore_cache.clone(),
        package_repo: cur.package_repo.clone(),
        readme: cur.readme.clone(),
    });

    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, ns)
}

/// Anonymous is refused, **and** no namespace is claimed.
///
/// The second half is the one that matters: OpenVSX's own semantics are
/// first-come self-service, so a handler that claimed first and refused
/// afterwards would answer `403` and still have handed the namespace out.
#[actix_web::test]
async fn openvsx_namespace_claim_anonymous_is_refused_and_claims_nothing() {
    let (app, ns) = make_vsx_namespace_app().await;

    let req = TestRequest::post()
        .uri("/proxy/local-vsx/api/-/namespace/create?name=digital")
        .set_json(serde_json::json!({ "group_id": "eng" }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        401,
        "`openvsx:namespace:claim` is not granted to anonymous by any translation rule"
    );

    assert!(
        ns.list_namespaces("local-vsx")
            .await
            .expect("list")
            .is_empty(),
        "the refusal has to be a refusal to claim, not a claim followed by a 403"
    );
}

/// A plain `USER` is refused too — the verb is not in rule 5's write set.
///
/// This is the assertion that distinguishes the model from the one it replaced.
/// Publishing a VSIX is `role:user` work, and OpenVSX upstream lets any signed-in
/// account claim a free namespace; here it is administrative, and the difference
/// is visible only because this row makes an authenticated non-admin request.
#[actix_web::test]
async fn openvsx_namespace_claim_is_not_self_service_for_a_user() {
    let (app, ns) = make_vsx_namespace_app().await;

    let req = TestRequest::post()
        .uri("/proxy/local-vsx/api/-/namespace/create?name=digital")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "group_id": "eng" }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);

    assert!(ns
        .list_namespaces("local-vsx")
        .await
        .expect("list")
        .is_empty());
}

/// The positive control: the same request, as a holder of the verb, works.
///
/// Without this the two rows above are satisfied by a route that refuses
/// everybody — including one that 403s because the payload is malformed.
#[actix_web::test]
async fn openvsx_namespace_claim_admin_claims_and_the_claim_is_stored() {
    let (app, ns) = make_vsx_namespace_app().await;

    let req = TestRequest::post()
        .uri("/proxy/local-vsx/api/-/namespace/create?name=digital")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "group_id": "eng team" }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        200,
        "the identical request must work for someone"
    );
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["ok"], true);

    let claimed = ns.list_namespaces("local-vsx").await.expect("list");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].prefix, "digital");
    assert_eq!(
        claimed[0].group_id, "engteam",
        "stored with spaces stripped, or `check_team_visibility` never matches it"
    );
    assert_eq!(
        claimed[0].separator, '.',
        "an openvsx namespace is `publisher.extension`; storing `/` would claim a \
         prefix that no extension id is under"
    );

    // Second claim of the same prefix is a conflict, not a silent takeover.
    let req = TestRequest::post()
        .uri("/proxy/local-vsx/api/-/namespace/create?name=digital")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "group_id": "other" }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 409);
}

/// A namespace is one segment. `..` in it would reach the storage layer as a
/// prefix that covers coordinates it has no business covering.
#[actix_web::test]
async fn openvsx_namespace_claim_traversal_name_returns_400() {
    let (app, ns) = make_vsx_namespace_app().await;

    for name in ["../../etc/x", "a/b", ""] {
        let req = TestRequest::post()
            .uri(&format!(
                "/proxy/local-vsx/api/-/namespace/create?name={name}"
            ))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({ "group_id": "eng" }))
            .to_request();
        assert_eq!(
            call_service(&app, req).await.status(),
            400,
            "name = {name:?}"
        );
    }

    assert!(ns
        .list_namespaces("local-vsx")
        .await
        .expect("list")
        .is_empty());
}
