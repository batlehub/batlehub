//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_config::schema::RegistryMode;

// ── packages.json ─────────────────────────────────────────────────────────────

/// Upload a package as an ordinary user, and answer with the status.
async fn upload<S: TestService>(app: &S, name: &str, version: &str) -> actix_web::http::StatusCode {
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_composer_zip(name, version))
        .to_request();
    call_service(app, req).await.status()
}

/// A read as an ordinary user, asserting `200`, returning the JSON body.
async fn user_json<S: TestService>(app: &S, uri: &str) -> Value {
    let req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "{uri} should be served");
    read_body_json(resp).await
}

/// A read as an ordinary user, asserting `200`, returning the body as text.
async fn user_text<S: TestService>(app: &S, uri: &str) -> String {
    let req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "{uri} should be served");
    String::from_utf8(read_body(resp).await.to_vec()).expect("body is valid UTF-8")
}

#[actix_web::test]
async fn composer_packages_json_proxy_mode_returns_metadata_url() {
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    let body: Value = user_json(&app, "/proxy/local-composer/packages.json").await;
    let metadata_url = body["metadata-url"].as_str().unwrap();
    assert!(
        metadata_url.contains("/proxy/local-composer/p2/%package%.json"),
        "metadata-url must point to our p2 endpoint"
    );
    // `available-packages` must be **absent**, not empty. Composer reads it as
    // the complete contents of the repository, so `[]` says "there is nothing
    // here" and it stops: it never requests `metadata-url` for any package and
    // reports each one as "could not be found in any version".
    //
    // This assertion used to require `[]`, which is how the bug survived — the
    // wire shape was pinned without asking what the client does with it.
    // Measured with Composer 2.10.2 against a real server (RFC 0009 §12.10).
    assert!(
        body.get("available-packages").is_none(),
        "proxy mode cannot enumerate upstream, so claiming a complete list \
         makes Composer resolve nothing; got {:?}",
        body.get("available-packages")
    );
}

/// Hybrid knows its own packages and not upstream's, so it cannot make the
/// completeness claim either — advertising the local list would hide every
/// proxied package from resolution.
#[actix_web::test]
async fn composer_packages_json_hybrid_mode_omits_available_packages() {
    let app = make_local_composer_app(RegistryMode::Hybrid).await;

    assert_eq!(upload(&app, "acme/my-pkg", "1.0.0").await, 200);

    let body: Value = user_json(&app, "/proxy/local-composer/packages.json").await;
    assert!(
        body.get("available-packages").is_none(),
        "a hybrid registry that lists only its local packages tells Composer \
         upstream's do not exist; got {:?}",
        body.get("available-packages")
    );
    // The package is still resolvable — through `metadata-url`, which is the
    // endpoint a proxy can actually answer for anything.
    let req = TestRequest::get()
        .uri("/proxy/local-composer/p2/acme/my-pkg.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn composer_packages_json_local_mode_lists_published_packages() {
    let app = make_local_composer_app(RegistryMode::Local).await;

    // Publish a package first so it appears in the listing.
    let zip = make_composer_zip("acme/my-pkg", "1.0.0");
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body: Value = user_json(&app, "/proxy/local-composer/packages.json").await;
    let available = body["available-packages"].as_array().unwrap();
    assert!(
        available.iter().any(|v| v.as_str() == Some("acme/my-pkg")),
        "available-packages must list published package name"
    );
}

#[actix_web::test]
async fn composer_packages_json_unknown_registry_returns_404() {
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    let req = TestRequest::get()
        .uri("/proxy/no-such-registry/packages.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── p2 metadata ───────────────────────────────────────────────────────────────

#[actix_web::test]
async fn composer_p2_proxy_mode_returns_artifact_body() {
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    // FixedRegistry returns "artifact:composer:…" — assert content originates from the registry call
    let body_str = user_text(&app, "/proxy/local-composer/p2/vendor/pkg.json").await;
    assert!(
        body_str.contains("vendor/pkg"),
        "response body must reference the requested package name; got: {body_str:?}"
    );
}

#[actix_web::test]
async fn composer_p2_dev_variant_returns_200_and_body() {
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    // ~dev.json is a valid variant — the parse helper strips the suffix.
    let body_str = user_text(&app, "/proxy/local-composer/p2/vendor/pkg~dev.json").await;
    assert!(
        body_str.contains("vendor/pkg"),
        "response body must reference the requested package name; got: {body_str:?}"
    );
}

#[actix_web::test]
async fn composer_p2_local_mode_published_package_found() {
    let app = make_local_composer_app(RegistryMode::Local).await;

    assert_eq!(upload(&app, "acme/my-lib", "2.0.0").await, 200);

    let body: Value = user_json(&app, "/proxy/local-composer/p2/acme/my-lib.json").await;
    assert!(body["packages"]["acme/my-lib"].is_array());
}

#[actix_web::test]
async fn composer_p2_local_mode_unknown_package_returns_404() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-composer/p2/ghost/pkg.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn composer_p2_hybrid_mode_falls_back_to_proxy() {
    // In hybrid mode with no local packages the request falls back to FixedRegistry.
    let app = make_local_composer_app(RegistryMode::Hybrid).await;
    let req = TestRequest::get()
        .uri("/proxy/local-composer/p2/vendor/remote-pkg.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

// ── dist artifact ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn composer_dist_proxy_mode_streams_artifact() {
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    let req = TestRequest::get()
        .uri("/proxy/local-composer/dist/vendor/pkg/1.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn composer_dist_local_mode_serves_stored_artifact() {
    let app = make_local_composer_app(RegistryMode::Local).await;

    let zip = make_composer_zip("acme/zippkg", "3.1.0");
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip.clone())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::get()
        .uri("/proxy/local-composer/dist/acme/zippkg/3.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    assert_eq!(body.as_ref(), zip.as_slice());
}

#[actix_web::test]
async fn composer_dist_local_mode_unknown_version_returns_404() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-composer/dist/ghost/pkg/9.9.9")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn composer_dist_hybrid_falls_back_to_proxy() {
    let app = make_local_composer_app(RegistryMode::Hybrid).await;
    let req = TestRequest::get()
        .uri("/proxy/local-composer/dist/vendor/remote/1.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

// ── upload ────────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn composer_upload_user_can_publish() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    let zip = make_composer_zip("myvendor/mypkg", "1.0.0");
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["status"], "success");
    assert_eq!(body["name"], "myvendor/mypkg");
    assert_eq!(body["version"], "1.0.0");
}

#[actix_web::test]
async fn composer_upload_version_override_via_query_param() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    // ZIP has version "1.0.0" in composer.json but we override to "2.5.0".
    let zip = make_composer_zip("myvendor/override-pkg", "1.0.0");
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload?version=2.5.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["version"], "2.5.0");
}

#[actix_web::test]
async fn composer_upload_anonymous_returns_403() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    let zip = make_composer_zip("myvendor/anon-pkg", "1.0.0");
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        // No Authorization header — anonymous identity.
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn composer_upload_proxy_mode_returns_404() {
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    let zip = make_composer_zip("myvendor/proxy-pkg", "1.0.0");
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn composer_upload_duplicate_version_returns_409() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    let zip = make_composer_zip("myvendor/dup-pkg", "1.0.0");

    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip.clone())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 409);
}

#[actix_web::test]
async fn composer_upload_invalid_zip_returns_422() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"this is not a zip file".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 422);
}

#[actix_web::test]
async fn composer_upload_then_p2_shows_package() {
    let app = make_local_composer_app(RegistryMode::Local).await;

    assert_eq!(upload(&app, "acme/seq-pkg", "1.2.3").await, 200);

    let body: Value = user_json(&app, "/proxy/local-composer/p2/acme/seq-pkg.json").await;
    let versions = body["packages"]["acme/seq-pkg"].as_array().unwrap();
    assert!(!versions.is_empty());
    assert_eq!(versions[0]["version"], "1.2.3");
    assert!(versions[0]["dist"]["url"]
        .as_str()
        .unwrap()
        .contains("/proxy/local-composer/dist/acme/seq-pkg/1.2.3"));
}

// ── yank ──────────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn composer_yank_excludes_version_from_p2() {
    // Yanked versions are removed from the Packagist v2 response because Composer
    // clients have no standard `yanked` field — they would otherwise install yanked releases.
    let app = make_local_composer_app(RegistryMode::Local).await;

    assert_eq!(upload(&app, "acme/yankable", "4.0.0").await, 200);

    // Verify the version appears before yanking.
    let body: Value = user_json(&app, "/proxy/local-composer/p2/acme/yankable.json").await;
    assert!(!body["packages"]["acme/yankable"]
        .as_array()
        .unwrap()
        .is_empty());

    let req = TestRequest::delete()
        .uri("/proxy/local-composer/api/packages/acme/yankable/versions/4.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // After yanking the only version, the p2 endpoint should return 404.
    let req = TestRequest::get()
        .uri("/proxy/local-composer/p2/acme/yankable.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn composer_yank_anonymous_returns_403() {
    let app = make_local_composer_app(RegistryMode::Local).await;
    let req = TestRequest::delete()
        .uri("/proxy/local-composer/api/packages/acme/anon-pkg/versions/1.0.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn composer_yank_proxy_mode_returns_404() {
    let app = make_local_composer_app(RegistryMode::Proxy).await;
    let req = TestRequest::delete()
        .uri("/proxy/local-composer/api/packages/acme/proxy-pkg/versions/1.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── misc ──────────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn composer_wrong_registry_type_returns_404() {
    // "npm" registry exists but is type "npm", not "composer".
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/packages.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    // Since TD-0004 §1 a request is routed on its own registry kind's routes
    // only: the other kind's handler is never reached, whatever answers.
    let resp = call_service(&app, req).await;
    assert_ne!(
        resp.request().match_pattern().as_deref(),
        Some("/proxy/{registry}/packages.json")
    );
}

/// Composer verifies `dist.shasum` with **SHA-1** — RFC 0009 §12.16.
///
/// The p2 document published the artifact's stored SHA-256 there, so
/// `composer install` of a locally published package downloaded the zip,
/// hashed it, disagreed with itself and stopped:
/// *"The checksum verification of the file failed"*. Every route was right and
/// no package could be installed. Found by `tests/heavy/composer.sh`.
///
/// The assertion is that the digest is the SHA-1 **of the bytes the dist URL
/// serves** — not that it is 40 characters long, which a truncated SHA-256
/// would also be.
#[actix_web::test]
async fn composer_p2_dist_shasum_is_the_sha1_of_the_artifact() {
    let app = make_local_composer_app(RegistryMode::Local).await;

    let zip = make_composer_zip("acme/shapkg", "1.0.0");
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(zip.clone())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::get()
        .uri("/proxy/local-composer/p2/acme/shapkg.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let doc: Value = read_body_json(call_service(&app, req).await).await;
    let shasum = doc["packages"]["acme/shapkg"][0]["dist"]["shasum"]
        .as_str()
        .expect("the dist entry must carry a shasum");

    let expected = batlehub_core::services::sha1_hex(&zip);
    assert_eq!(
        shasum, expected,
        "dist.shasum must be the SHA-1 Composer computes over the downloaded file"
    );

    // And the bookkeeping key it came from is not served to the client.
    assert!(
        doc["packages"]["acme/shapkg"][0]
            .get(batlehub_core::services::COMPOSER_DIST_SHA1)
            .is_none(),
        "the internal sha1 field must be stripped from the published entry"
    );
}
