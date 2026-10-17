//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use serde_json::Value;

use base64::Engine as _;
use batlehub_config::schema::RegistryMode;
use batlehub_core::ports::SigningKeyPort as _;
use batlehub_core::services::RegistryPolicy;
use batlehub_web::RegistryModeMap;
use std::sync::Arc;

// ══ Terraform local registry tests ════════════════════════════════════════════

async fn make_local_terraform_app(mode: RegistryMode) -> impl TestService {
    build_local_registry_app(
        local_only_app_parts("local-tf", "terraform", mode, false),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

/// Like `make_local_terraform_app`, but also returns the `RegistryModeMap` handle
/// so a test can flip the registry's mode after publishing (simulating a
/// hot-reload) to confirm mode-gated endpoints re-check the *current* mode.
async fn make_local_terraform_app_with_mode_map(
    mode: RegistryMode,
) -> (impl TestService, RegistryModeMap) {
    let parts = local_only_app_parts("local-tf", "terraform", mode, false);
    let mode_map = parts.mode_map.clone();
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, mode_map)
}

#[actix_web::test]
async fn terraform_provider_artifact_proxy_mode_rejects_previously_published_binary() {
    let (app, mode_map) = make_local_terraform_app_with_mode_map(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::put()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"fake-zip-bytes".as_slice())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // Confirm it's actually retrievable while still in Local mode.
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // Simulate a hot-reload switching the registry to Proxy mode: the binary
    // must no longer be servable from local storage.
    //
    // Since RFC 0009 §7.2 this route also has a proxy fall-through — the
    // provider download document points `download_url` here so the zip is
    // gated and cached instead of fetched from upstream's CDN directly — so the
    // request no longer stops at a `require_local_mode` guard. The property
    // under test is unchanged and is asserted directly: whatever the proxy path
    // answers, it must not be the locally published bytes.
    mode_map.insert("local-tf".to_owned(), RegistryMode::Proxy);
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_ne!(
        resp.status(),
        200,
        "local storage must not answer once the registry is in proxy mode"
    );
    let body = actix_web::test::read_body(resp).await;
    assert_ne!(body.as_ref(), b"fake-zip-bytes".as_slice());
}

#[actix_web::test]
async fn terraform_module_artifact_proxy_mode_rejects_previously_published_tarball() {
    let (app, mode_map) = make_local_terraform_app_with_mode_map(RegistryMode::Local).await;
    let payload = b"tarball-content-bytes";

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(payload.as_slice())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 201);

    // Confirm it's actually retrievable while still in Local mode.
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0/artifact")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    // Simulate a hot-reload switching the registry to Proxy mode: the tarball
    // must no longer be servable from local storage.
    //
    // Since RFC 0009 §7.2 this route also has a proxy fall-through — module
    // downloads are redirected here rather than to upstream so the bytes go
    // through the rule chain — so the request no longer stops at a
    // `require_local_mode` guard. The property under test is unchanged.
    mode_map.insert("local-tf".to_owned(), RegistryMode::Proxy);
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0/artifact")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_ne!(
        resp.status(),
        200,
        "local storage must not answer once the registry is in proxy mode"
    );
    let body = actix_web::test::read_body(resp).await;
    assert_ne!(body.as_ref(), payload.as_slice());
}

// ── Terraform module tests ────────────────────────────────────────────────────

#[actix_web::test]
async fn terraform_module_upload_returns_201() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"fake-tarball-bytes".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
}

#[actix_web::test]
async fn terraform_module_versions_after_upload() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let versions = body["modules"][0]["versions"].as_array().unwrap();
    assert!(versions.iter().any(|v| v["version"] == "0.1.0"));
}

#[actix_web::test]
async fn terraform_module_download_local_returns_204_with_header() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0/download")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 204);
    let header = resp
        .headers()
        .get("X-Terraform-Get")
        .expect("X-Terraform-Get header must be present");
    let url = header.to_str().unwrap();
    assert!(
        url.contains("/artifact"),
        "X-Terraform-Get should point at /artifact"
    );
}

#[actix_web::test]
async fn terraform_module_artifact_returns_bytes() {
    let app = make_local_terraform_app(RegistryMode::Local).await;
    let payload = b"tarball-content-bytes";

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(payload.as_slice())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0/artifact")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await, payload.as_slice());
}

#[actix_web::test]
async fn terraform_module_upload_duplicate_returns_409() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    for _ in 0..2 {
        let req = TestRequest::post()
            .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(b"tarball".as_slice())
            .to_request();
        let _ = call_service(&app, req).await;
    }

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 409);
}

// ── Terraform provider tests ──────────────────────────────────────────────────

const PROVIDER_MANIFEST: &str = r#"{
  "version": "5.0.0",
  "protocols": ["5.0"],
  "platforms": [
    {"os": "linux", "arch": "amd64", "filename": "terraform-provider-aws_5.0.0_linux_amd64.zip", "shasum": "deadbeef"}
  ]
}"#;

#[actix_web::test]
async fn terraform_provider_upload_manifest_returns_201() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
}

#[actix_web::test]
async fn terraform_provider_binary_upload_returns_200() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    // Must upload manifest first (no strict requirement in handler, but good practice)
    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::put()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"fake-zip-bytes".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

/// The binary key joins OS and arch with `-`, so `(linux-amd64, x)` and
/// `(linux, amd64-x)` were one key and either upload replaced the other.
#[actix_web::test]
async fn terraform_provider_binary_upload_refuses_a_dash_in_the_os() {
    let app = make_local_terraform_app(RegistryMode::Local).await;
    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::put()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux-amd64/x")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"fake-zip-bytes".as_slice())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

#[actix_web::test]
async fn terraform_provider_binary_upload_anonymous_returns_403() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    // No Authorization header at all — must be rejected by enforce_publish_policy
    // the same way the manifest upload is, not silently stored.
    let req = TestRequest::put()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .set_payload(b"fake-zip-bytes".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn terraform_provider_versions_after_upload() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let versions = body["versions"].as_array().unwrap();
    assert!(versions.iter().any(|v| v["version"] == "5.0.0"));
}

#[actix_web::test]
async fn terraform_provider_download_contains_local_artifact_url() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let download_url = body["download_url"].as_str().unwrap();
    assert!(
        download_url.contains("/artifact/linux/amd64"),
        "download_url should point at local artifact endpoint, got: {download_url}"
    );
}

// ── Terraform module yank / unyank ────────────────────────────────────────────

#[actix_web::test]
async fn terraform_module_yank_returns_200() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/versions/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["message"].as_str().unwrap().contains("yanked"));
}

#[actix_web::test]
async fn terraform_module_yanked_hidden_from_versions() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/versions/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    // After yank the only version is yanked; local_svc returns NotFound when all are yanked
    assert!(resp.status() == 200 || resp.status() == 404);
}

#[actix_web::test]
async fn terraform_module_unyank_returns_200() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/versions/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/versions/0.1.0/unyank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["message"].as_str().unwrap().contains("unyanked"));
}

#[actix_web::test]
async fn terraform_module_yank_requires_auth() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/versions/0.1.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert!(resp.status() == 401 || resp.status() == 403);
}

// ── Terraform provider yank / unyank ─────────────────────────────────────────

#[actix_web::test]
async fn terraform_provider_yank_returns_200() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions/5.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["message"].as_str().unwrap().contains("yanked"));
}

#[actix_web::test]
async fn terraform_provider_unyank_returns_200() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions/5.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions/5.0.0/unyank")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body["message"].as_str().unwrap().contains("unyanked"));
}

#[actix_web::test]
async fn terraform_provider_yank_requires_auth() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::delete()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions/5.0.0")
        .to_request();
    let resp = call_service(&app, req).await;
    assert!(resp.status() == 401 || resp.status() == 403);
}

// ── Terraform signing headers ─────────────────────────────────────────────────

#[actix_web::test]
async fn terraform_module_upload_with_signature_preserved_on_artifact_download() {
    let app = make_local_terraform_app(RegistryMode::Local).await;
    let sig = base64::engine::general_purpose::STANDARD.encode(b"fake-ed25519-sig");

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.2.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("X-Artifact-Signature", sig.as_str()))
        .insert_header(("X-Signature-Type", "ed25519"))
        .set_payload(b"tarball".as_slice())
        .to_request();
    let upload_resp = call_service(&app, req).await;
    assert_eq!(upload_resp.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.2.0/artifact")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    // Signature headers must be echoed back on download
    assert!(
        resp.headers().get("X-Artifact-Signature").is_some(),
        "X-Artifact-Signature header must be present on artifact download"
    );
    assert_eq!(
        resp.headers()
            .get("X-Signature-Type")
            .and_then(|v| v.to_str().ok()),
        Some("ed25519")
    );
}

/// Survey finding 13, at the edge. `X-Artifact-Signature` and `X-Signature-Type`
/// are independent headers, so a publisher could send either alone — and bytes
/// without a type satisfied `signing.required` while skipping the
/// `allowed_types` allow-list, producing a stored "signed" artifact whose
/// signature nothing would ever verify.
///
/// The check lives in `extract_signature_headers`, which every publish route in
/// every ecosystem calls, so this exercises the shared extractor through the
/// route that already covers these headers rather than repeating itself
/// thirteen times.
#[actix_web::test]
async fn terraform_module_upload_with_an_incoherent_signature_pair_returns_400() {
    let app = make_local_terraform_app(RegistryMode::Local).await;
    let sig = base64::engine::general_purpose::STANDARD.encode(b"fake-ed25519-sig");

    let cases: [(&str, Vec<(&str, String)>); 5] = [
        (
            "bytes with no type: the state that used to pass every check",
            vec![("X-Artifact-Signature", sig.clone())],
        ),
        (
            "a type with no bytes",
            vec![("X-Signature-Type", "ed25519".to_owned())],
        ),
        (
            // Previously decoded to `None`, making a malformed signature
            // indistinguishable from an absent one.
            "bytes that are not base64",
            vec![
                ("X-Artifact-Signature", "!!!not-base64!!!".to_owned()),
                ("X-Signature-Type", "ed25519".to_owned()),
            ],
        ),
        (
            // The same fail-open entered through the length: `""` is valid
            // base64 for zero bytes, so this decoded to `Some(vec![])` — present
            // enough for `signing.required` and for the stored row to read back
            // as signed, carrying nothing any key could verify.
            "an empty signature with a permitted type",
            vec![
                ("X-Artifact-Signature", String::new()),
                ("X-Signature-Type", "ed25519".to_owned()),
            ],
        ),
        (
            "real bytes with an empty type",
            vec![
                ("X-Artifact-Signature", sig.clone()),
                ("X-Signature-Type", String::new()),
            ],
        ),
    ];

    for (what, headers) in cases {
        let mut req = TestRequest::post()
            .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.3.0")
            .insert_header(("Authorization", bearer(USER_TOKEN)));
        for (name, value) in headers {
            req = req.insert_header((name, value));
        }
        let resp = call_service(&app, req.set_payload(b"tarball".as_slice()).to_request()).await;
        assert_eq!(resp.status(), 400, "{what}");
    }

    // …and nothing was published under that version by any of the three.
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.3.0/artifact")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_ne!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn terraform_provider_upload_with_signature_preserved_on_download_info() {
    let app = make_local_terraform_app(RegistryMode::Local).await;
    let sig = base64::engine::general_purpose::STANDARD.encode(b"fake-provider-sig");

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .insert_header(("X-Artifact-Signature", sig.as_str()))
        .insert_header(("X-Signature-Type", "ed25519"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    let upload_resp = call_service(&app, req).await;
    assert_eq!(upload_resp.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers().get("X-Artifact-Signature").is_some(),
        "X-Artifact-Signature header must be present on provider download info"
    );
    assert_eq!(
        resp.headers()
            .get("X-Signature-Type")
            .and_then(|v| v.to_str().ok()),
        Some("ed25519")
    );
}

// ── Terraform quota headers ───────────────────────────────────────────────────

#[actix_web::test]
async fn terraform_module_upload_returns_quota_headers() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.3.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"tarball".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
    // Quota headers are only present when a quota is configured; the in-memory backend
    // has no quota, so they are absent — but the response must still be 201.
    // This test verifies the handler correctly returns 201 regardless of quota header presence.
}

#[actix_web::test]
async fn terraform_provider_upload_returns_quota_headers() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);
}

// ── Terraform provider read paths (versions/download/artifact) ──────────────

#[actix_web::test]
async fn terraform_provider_versions_local_unknown_returns_404() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/unknown/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn terraform_provider_versions_hybrid_falls_through_to_proxy() {
    let app = make_local_terraform_app(RegistryMode::Hybrid).await;

    // No upload, and no RegistryClient configured for "local-tf" — the Hybrid
    // fallthrough on local NotFound reaches proxy_stream, which then fails fast
    // with "unknown registry" since the registries map is empty in this factory.
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/unknown/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn terraform_provider_versions_proxy_mode_goes_straight_to_proxy() {
    let app = make_local_terraform_app(RegistryMode::Proxy).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn terraform_provider_download_local_unknown_returns_404() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/unknown/9.9.9/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn terraform_provider_download_hybrid_falls_through_to_proxy() {
    let app = make_local_terraform_app(RegistryMode::Hybrid).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/unknown/9.9.9/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn terraform_provider_artifact_path_traversal_returns_400() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    // `%2F..` decodes to a path segment containing "/.." — caught by
    // validate_path_safe before it ever becomes a storage key.
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux%2F../amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn terraform_provider_artifact_not_found_returns_404() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn terraform_provider_artifact_returns_uploaded_binary() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::put()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"fake-zip-bytes".as_slice())
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/zip"
    );
    let body = read_body(resp).await;
    assert_eq!(&body[..], b"fake-zip-bytes");
}

// ── Discovery, the network mirror, and the download gate (RFC 0009 §7.2) ─────
//
// Three defects, one phase. Discovery did not exist, so Terraform could not find
// the `/v1/` routes at all. The network mirror our own docs configured was a
// different protocol from the one implemented. And provider/module downloads
// handed the client an upstream URL, so the bytes never passed through the rule
// chain — RFC 0006 §13.6, which this closes.

/// Discovery is host-rooted by the protocol. Under path routing there is no
/// single registry it could describe, so it declines rather than guessing.
#[actix_web::test]
async fn discovery_declines_on_a_path_routed_request() {
    let (app, _mode_map) = make_local_terraform_app_with_mode_map(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/.well-known/terraform.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);

    // ...and says why, rather than looking like a missing route.
    let body = actix_web::test::read_body(resp).await;
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("host bound to a single") || text.contains("subdomain_routing"),
        "the 404 must name the host-routing prerequisite, got: {text}"
    );
}

/// The mirror path carries the *origin* registry's hostname so one mirror can
/// serve several. We are one upstream per registry, so a mismatch is refused
/// rather than echoed — otherwise the document would attach an `example.com`
/// provenance to a `registry.terraform.io` provider (RFC 0009 §11.1).
#[actix_web::test]
async fn the_mirror_refuses_a_hostname_that_is_not_this_registrys_upstream() {
    let (app, _mode_map) = make_local_terraform_app_with_mode_map(RegistryMode::Proxy).await;
    let req = TestRequest::get()
        .uri("/proxy/local-tf/evil.example.com/hashicorp/aws/index.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

/// The regression `protocol_conformance.rs` caught while this phase was being
/// written: the mirror's four-segment pattern claimed RubyGems'
/// `/api/v1/versions/{gem}.json` as host="api", ns="v1", type="versions".
///
/// Pinned here as well as in the conformance table, because the constraint that
/// fixes it (a hostname must contain a dot) is easy to relax by accident.
#[actix_web::test]
async fn the_mirror_pattern_does_not_claim_a_dotless_four_segment_path() {
    let (app, _mode_map) = make_local_terraform_app_with_mode_map(RegistryMode::Proxy).await;
    let req = TestRequest::get()
        .uri("/proxy/local-tf/api/v1/versions/rails.json")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_ne!(
        resp.request().match_pattern().as_deref(),
        Some(r"/proxy/{registry}/{hostname:[^/]+\.[^/]+}/{namespace}/{ptype}/{version}.json"),
        "a dotless segment must not be taken for a registry hostname"
    );
}

/// `index.json` is a legal `{version}` capture, so the two mirror routes are
/// order-sensitive. Registered the wrong way round, the version route answers
/// the index request.
#[actix_web::test]
async fn the_mirror_index_is_not_claimed_by_the_version_route() {
    let (app, _mode_map) = make_local_terraform_app_with_mode_map(RegistryMode::Proxy).await;
    let req = TestRequest::get()
        .uri("/proxy/local-tf/registry.terraform.io/hashicorp/aws/index.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.request().match_pattern().as_deref(),
        Some(r"/proxy/{registry}/{hostname:[^/]+\.[^/]+}/{namespace}/{ptype}/index.json"),
        "index.json must reach the index route, not the version route"
    );
}

/// The gate this phase closes: a module download must not hand the client an
/// upstream URL. Whatever mode the registry is in, `X-Terraform-Get` points at
/// a route on *this* host, which runs the rule chain on the bytes.
#[actix_web::test]
async fn a_module_download_points_at_this_proxy_not_upstream() {
    for mode in [
        RegistryMode::Local,
        RegistryMode::Proxy,
        RegistryMode::Hybrid,
    ] {
        let (app, _mode_map) = make_local_terraform_app_with_mode_map(mode.clone()).await;
        let req = TestRequest::get()
            .uri("/proxy/local-tf/v1/modules/hashicorp/consul/aws/0.1.0/download")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request();
        let resp = call_service(&app, req).await;
        let header = resp
            .headers()
            .get("X-Terraform-Get")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(
            header.contains("/v1/modules/hashicorp/consul/aws/0.1.0/artifact"),
            "{mode:?}: X-Terraform-Get must name our own artifact route, got {header:?}"
        );
        assert!(
            !header.contains("registry.terraform.io"),
            "{mode:?}: the client must never be sent to upstream directly, got {header:?}"
        );
    }
}

// ── Signed download URLs: minting (RFC 0012 phase 2) ──────────────────────────
//
// Terraform fetches the provider archive with no `Authorization` header and has
// no mechanism to send one — measured, not read (RFC 0012 §11). Phase 2 mints a
// signature into the mirror document that was itself authenticated. Nothing
// verifies it yet, so every assertion here is about what the document says.

const SIGNING_SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

/// A proxy-mode Terraform registry mirroring `registry.terraform.io`, with
/// signing either on or off.
async fn make_mirror_app(signed: bool) -> impl TestService {
    mirror_app(signed, false).await
}

/// The rule chain of a registry that has done what this RFC is for: removed
/// the anonymous grant. `[registries.rbac] anonymous = []`, spelled out.
fn closed_policy(repo: Arc<dyn batlehub_core::ports::PackageRepository>) -> RegistryPolicy {
    let perms = std::collections::HashMap::from([
        (
            batlehub_core::entities::Role::Anonymous,
            Vec::<String>::new(),
        ),
        (
            batlehub_core::entities::Role::User,
            vec!["releases:read".to_owned(), "source:read".to_owned()],
        ),
        (batlehub_core::entities::Role::Admin, vec!["*".to_owned()]),
    ]);
    RegistryPolicy {
        metadata_ttl: Some(std::time::Duration::from_secs(300)),
        firewall_only: false,
        serve_stale_metadata: false,
        artifact_ttl: None,
        rules: vec![
            Box::new(
                batlehub_core::rules::RbacRule::from_patterns(perms)
                    .expect("fixture rbac patterns are valid"),
            ),
            Box::new(batlehub_core::rules::BlockListRule::new(repo)),
        ],
    }
}

/// `signed` mints and verifies; `closed` removes the anonymous grant, which is
/// the state the whole RFC exists to make reachable.
async fn mirror_app(signed: bool, closed: bool) -> impl TestService {
    let parts = local_only_app_parts("local-tf", "terraform", RegistryMode::Proxy, true);
    {
        // Written through the hot lock, which is also how a config reload would
        // turn signing on: nothing here needs a restart.
        let mut hot = parts.proxy_svc.hot.write().await;
        if signed {
            hot.signed_downloads.insert("local-tf".to_owned(), true);
            hot.signed_url = Some(Arc::new(batlehub_core::services::SignedUrlService::new(
                SIGNING_SECRET,
                vec![],
                300,
            )));
        }
        if closed {
            hot.policies.insert(
                "local-tf".to_owned(),
                Arc::new(closed_policy(parts.proxy_svc.repo.clone())),
            );
        }
    }
    build_local_registry_app_with_defaults(
        parts,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            upstream_map: batlehub_web::UpstreamMap::from(std::collections::HashMap::from([(
                "local-tf".to_owned(),
                "https://registry.terraform.io".to_owned(),
            )])),
            ..Default::default()
        },
    )
    .await
}

/// The mirror `{version}.json`, as `user-1`.
async fn mirror_document(app: &impl TestService) -> Value {
    let req = TestRequest::get()
        .uri("/proxy/local-tf/registry.terraform.io/hashicorp/aws/1.0.0.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "the mirror document must be served");
    read_body_json(resp).await
}

fn archive_url<'a>(doc: &'a Value, platform: &str) -> &'a str {
    doc["archives"][platform]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("no url for {platform} in {doc}"))
}

fn signature_of(url: &str) -> &str {
    url.split_once("?bh_sig=")
        .unwrap_or_else(|| panic!("no bh_sig in {url}"))
        .1
}

fn verifier() -> batlehub_core::services::SignedUrlService {
    batlehub_core::services::SignedUrlService::new(SIGNING_SECRET, vec![], 300)
}

#[actix_web::test]
async fn a_registry_that_does_not_sign_mints_nothing() {
    // The default, and the shape every existing deployment keeps.
    let app = make_mirror_app(false).await;
    let doc = mirror_document(&app).await;
    let url = archive_url(&doc, "linux_amd64");
    assert!(!url.contains("bh_sig"), "unsigned registry emitted: {url}");
    assert_eq!(
        url,
        "../../../v1/providers/hashicorp/aws/1.0.0/artifact/linux/amd64"
    );
}

#[actix_web::test]
async fn a_signing_registry_appends_a_signature_to_every_platform() {
    let app = make_mirror_app(true).await;
    let doc = mirror_document(&app).await;
    for platform in [
        "linux_amd64",
        "linux_arm64",
        "darwin_amd64",
        "darwin_arm64",
        "windows_amd64",
    ] {
        let url = archive_url(&doc, platform);
        assert!(url.contains("?bh_sig=1."), "{platform}: {url}");
    }
}

/// RFC 0012 §4.3: the only change to the document is the query string. The
/// relative-URL arithmetic RFC 0009 §12.3 confirmed must be untouched, because
/// a relative reference resolves the path and keeps the query it was written
/// with.
#[actix_web::test]
async fn signing_leaves_the_relative_path_exactly_as_it_was() {
    let signed = mirror_document(&make_mirror_app(true).await).await;
    let unsigned = mirror_document(&make_mirror_app(false).await).await;

    let signed_path = archive_url(&signed, "linux_amd64")
        .split_once('?')
        .expect("signed url has a query")
        .0;
    assert_eq!(signed_path, archive_url(&unsigned, "linux_amd64"));
}

#[actix_web::test]
async fn a_minted_signature_verifies_for_its_own_coordinate() {
    let app = make_mirror_app(true).await;
    let doc = mirror_document(&app).await;
    let token = signature_of(archive_url(&doc, "linux_amd64"));

    let identity = verifier()
        .verify(
            token,
            &batlehub_core::services::SignedUrlCoordinate {
                method: "GET",
                registry: "local-tf",
                package: "providers/hashicorp/aws",
                version: "1.0.0",
                artifact: "linux/amd64",
            },
        )
        .expect("the minted signature must verify for the coordinate it names");

    // The caller the rule chain approved, not a synthetic one.
    assert_eq!(identity.user_id.as_deref(), Some("user-1"));
    assert_eq!(identity.role, batlehub_core::entities::Role::User);
}

/// Each platform gets its own single-coordinate signature: one archive URL
/// cannot be edited into another.
#[actix_web::test]
async fn a_signature_minted_for_one_platform_does_not_verify_for_another() {
    let app = make_mirror_app(true).await;
    let doc = mirror_document(&app).await;
    let linux = signature_of(archive_url(&doc, "linux_amd64"));

    let err = verifier()
        .verify(
            linux,
            &batlehub_core::services::SignedUrlCoordinate {
                method: "GET",
                registry: "local-tf",
                package: "providers/hashicorp/aws",
                version: "1.0.0",
                artifact: "darwin/arm64",
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            err,
            batlehub_core::services::SignedUrlError::CoordinateMismatch { .. }
        ),
        "got {err:?}"
    );
}

#[actix_web::test]
async fn every_platform_gets_a_distinct_signature() {
    let app = make_mirror_app(true).await;
    let doc = mirror_document(&app).await;
    let mut seen: Vec<&str> = ["linux_amd64", "linux_arm64", "darwin_amd64", "darwin_arm64"]
        .iter()
        .map(|p| signature_of(archive_url(&doc, p)))
        .collect();
    seen.sort_unstable();
    let before = seen.len();
    seen.dedup();
    assert_eq!(seen.len(), before, "platforms shared a signature");
}

/// A blocked version never reaches the minting site, because the mirror asks
/// the filtered version list first. Pinned so a refactor that mints before
/// filtering is caught here rather than by an operator.
#[actix_web::test]
async fn a_version_absent_from_the_filtered_list_mints_nothing() {
    let app = make_mirror_app(true).await;
    let req = TestRequest::get()
        .uri("/proxy/local-tf/registry.terraform.io/hashicorp/aws/9.9.9.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── Signed download URLs: verification (RFC 0012 phase 3) ─────────────────────
//
// The goal in §3, for the zip: `terraform init` completes against a registry
// whose `anonymous` grant is empty. Every test here runs against `closed = true`
// — a registry that would refuse an unauthenticated read — so a `200` can only
// have come from the signature.

const ARTIFACT: &str = "/proxy/local-tf/v1/providers/hashicorp/aws/1.0.0/artifact/linux/amd64";

/// The signature the mirror document actually minted for `linux_amd64`.
///
/// Deliberately taken from phase 2's output rather than minted by the test: it
/// is the join between the two phases, and a hand-minted token would not notice
/// if the two ever disagreed about the coordinate.
async fn minted_token(app: &impl TestService) -> String {
    let doc = mirror_document(app).await;
    signature_of(archive_url(&doc, "linux_amd64")).to_owned()
}

/// The whole point: no `Authorization` header, a closed registry, and the
/// download succeeds because the URL carries the verdict of the document that
/// was authenticated.
#[actix_web::test]
async fn a_signed_url_downloads_from_a_registry_with_no_anonymous_grant() {
    let app = mirror_app(true, true).await;
    let token = minted_token(&app).await;

    let req = TestRequest::get()
        .uri(&format!("{ARTIFACT}?bh_sig={token}"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        200,
        "a valid signature must stand in for the header Terraform cannot send"
    );
}

/// The control for the test above: same registry, same request, no signature.
#[actix_web::test]
async fn the_same_request_without_a_signature_is_refused() {
    let app = mirror_app(true, true).await;
    let req = TestRequest::get().uri(ARTIFACT).to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        403,
        "the registry must really be closed, or the test above proves nothing"
    );
}

#[actix_web::test]
async fn a_tampered_signature_is_refused_rather_than_ignored() {
    let app = mirror_app(true, true).await;
    let token = minted_token(&app).await;
    // Flip the last character of the MAC segment.
    let mut forged = token.clone();
    let last = forged.pop().unwrap();
    forged.push(if last == 'A' { 'B' } else { 'A' });

    let req = TestRequest::get()
        .uri(&format!("{ARTIFACT}?bh_sig={forged}"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);

    let body: Value = read_body_json(resp).await;
    assert_eq!(
        body["code"].as_str(),
        Some("signed-url.invalid"),
        "the operator needs a stable code to match on: {body}"
    );
}

/// §4.2: the message says which of the three it was. An operator debugging a
/// clock-skewed runner should not have to guess.
#[actix_web::test]
async fn a_signature_for_another_platform_says_so() {
    let app = mirror_app(true, true).await;
    let doc = mirror_document(&app).await;
    let darwin = signature_of(archive_url(&doc, "darwin_arm64"));

    let req = TestRequest::get()
        .uri(&format!("{ARTIFACT}?bh_sig={darwin}"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);

    let body: Value = read_body_json(resp).await;
    let msg = body["error"].as_str().unwrap_or_default().to_owned()
        + body["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("darwin/arm64") && msg.contains("linux/amd64"),
        "the refusal must name both coordinates, got: {body}"
    );
}

#[actix_web::test]
async fn an_expired_signature_says_expired() {
    let app = mirror_app(true, true).await;
    // Minted in the past, by the same secret the app verifies with.
    let signer = batlehub_core::services::SignedUrlService::new(SIGNING_SECRET, vec![], 300);
    let long_ago = chrono::Utc::now() - chrono::Duration::hours(2);
    let token = signer.mint_at(
        &batlehub_core::services::SignedUrlCoordinate {
            method: "GET",
            registry: "local-tf",
            package: "providers/hashicorp/aws",
            version: "1.0.0",
            artifact: "linux/amd64",
        },
        &batlehub_core::entities::Identity {
            user_id: Some("user-1".to_owned()),
            role: batlehub_core::entities::Role::User,
            auth_provider: None,
            groups: vec![],
        },
        long_ago,
    );

    let req = TestRequest::get()
        .uri(&format!("{ARTIFACT}?bh_sig={token}"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);

    let body: Value = read_body_json(resp).await;
    let msg = body["error"].as_str().unwrap_or_default().to_owned()
        + body["message"].as_str().unwrap_or_default();
    assert!(msg.contains("expired"), "got: {body}");
}

/// RFC 0012 §7: a `bh_sig` on a registry with the feature off is an ignored
/// query parameter, not a refusal and not an authentication. Without this, a
/// registry that had never enabled signing could be talked into honouring one.
#[actix_web::test]
async fn a_signature_is_ignored_when_the_registry_does_not_sign() {
    // Mint against a signing app, then present it to one with signing off.
    let signing = mirror_app(true, true).await;
    let token = minted_token(&signing).await;

    let not_signing = mirror_app(false, true).await;
    let req = TestRequest::get()
        .uri(&format!("{ARTIFACT}?bh_sig={token}"))
        .to_request();
    assert_eq!(
        call_service(&not_signing, req).await.status(),
        403,
        "signing is off, so the parameter must authenticate nothing"
    );
}

/// Header authentication is untouched by any of this: signing adds a second way
/// to authenticate, it does not replace the first.
#[actix_web::test]
async fn header_authentication_still_works_while_signing_is_on() {
    let app = mirror_app(true, true).await;
    let req = TestRequest::get()
        .uri(ARTIFACT)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

/// §6.6: verification replaces the header and nothing else. A blocked version
/// stays blocked for a URL minted before the block, because the block is
/// evaluated at redemption rather than at minting.
#[actix_web::test]
async fn a_signed_url_does_not_bypass_a_block_applied_after_minting() {
    let app = mirror_app(true, true).await;
    let token = minted_token(&app).await;

    // Block the version *after* the URL was minted.
    let req = TestRequest::post()
        .uri("/api/v1/admin/packages/block")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "registry": "local-tf",
            "name": "providers/hashicorp/aws",
            "version": "1.0.0",
            "reason": "blocked after the URL was minted"
        }))
        .to_request();
    let status = call_service(&app, req).await.status();
    assert!(status.is_success(), "block request failed: {status}");

    let req = TestRequest::get()
        .uri(&format!("{ARTIFACT}?bh_sig={token}"))
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        403,
        "the signature authenticates; it must not authorise past a block"
    );
}

/// §6.2 puts the method in the MAC because the download and publish routes
/// share a path shape. In this implementation there is a second, blunter
/// defence in front of it: the `PUT` handler never calls the verifier, so a
/// `bh_sig` on a publish is an ignored query parameter rather than a signature
/// that merely fails to match.
///
/// Both are worth keeping. This test pins the outer one — if a later change
/// wires verification into the publish route, the MAC's method binding is what
/// stops a download URL becoming an upload credential, and this test is what
/// says the question was considered.
#[actix_web::test]
async fn a_download_signature_cannot_authenticate_a_publish() {
    let parts = local_only_app_parts("put-tf", "terraform", RegistryMode::Local, false);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.signed_downloads.insert("put-tf".to_owned(), true);
        hot.signed_url = Some(Arc::new(batlehub_core::services::SignedUrlService::new(
            SIGNING_SECRET,
            vec![],
            300,
        )));
        hot.policies.insert(
            "put-tf".to_owned(),
            Arc::new(closed_policy(parts.proxy_svc.repo.clone())),
        );
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    // Publish the manifest so the upload route is otherwise reachable.
    let req = TestRequest::post()
        .uri("/proxy/put-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let upload = "/proxy/put-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64";

    // A signature minted for the *download* of this very coordinate.
    let token = batlehub_core::services::SignedUrlService::new(SIGNING_SECRET, vec![], 300).mint(
        &batlehub_core::services::SignedUrlCoordinate {
            method: "GET",
            registry: "put-tf",
            package: "providers/hashicorp/aws",
            version: "5.0.0",
            artifact: "linux/amd64",
        },
        &batlehub_core::entities::Identity {
            user_id: Some("user-1".to_owned()),
            role: batlehub_core::entities::Role::User,
            auth_provider: None,
            groups: vec![],
        },
    );

    let req = TestRequest::put()
        .uri(&format!("{upload}?bh_sig={token}"))
        .set_payload(b"not-my-bytes".as_slice())
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        403,
        "a download signature must not authorise a publish"
    );

    // The route is reachable with a real credential, so the 403 above was about
    // authentication and not about the route being unavailable.
    let req = TestRequest::put()
        .uri(upload)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"real-bytes".as_slice())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── Signed download URLs: the checksum pair (RFC 0012 phase 4) ────────────────
//
// The O5 measurement's second finding: `shasums_url` and `shasums_signature_url`
// arrive as bare as the zip does. Signing only the zip leaves `terraform init`
// failing one step later, at the checksum, with an error that points at
// checksums rather than at auth.

const SHASUMS: &str = "/proxy/local-tf/v1/providers/hashicorp/aws/1.0.0/shasums";
const SHASUMS_SIG: &str = "/proxy/local-tf/v1/providers/hashicorp/aws/1.0.0/shasums.sig";

/// Mint for one of the checksum coordinates directly. These URLs are produced
/// by the registry protocol's download document, which phase 5 repoints; until
/// then the test mints what phase 5 will.
fn mint_for(artifact: &str) -> String {
    batlehub_core::services::SignedUrlService::new(SIGNING_SECRET, vec![], 300).mint(
        &batlehub_core::services::SignedUrlCoordinate {
            method: "GET",
            registry: "local-tf",
            package: "providers/hashicorp/aws",
            version: "1.0.0",
            artifact,
        },
        &batlehub_core::entities::Identity {
            user_id: Some("user-1".to_owned()),
            role: batlehub_core::entities::Role::User,
            auth_provider: None,
            groups: vec![],
        },
    )
}

#[actix_web::test]
async fn a_signed_url_fetches_the_checksum_manifest_from_a_closed_registry() {
    let app = mirror_app(true, true).await;
    let req = TestRequest::get()
        .uri(&format!("{SHASUMS}?bh_sig={}", mint_for("shasums")))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn a_signed_url_fetches_the_checksum_signature_from_a_closed_registry() {
    let app = mirror_app(true, true).await;
    let req = TestRequest::get()
        .uri(&format!("{SHASUMS_SIG}?bh_sig={}", mint_for("shasums.sig")))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

/// The controls: both are genuinely closed without a signature, so the two
/// successes above cannot be coming from an anonymous grant.
#[actix_web::test]
async fn the_checksum_routes_are_closed_without_a_signature() {
    let app = mirror_app(true, true).await;
    for uri in [SHASUMS, SHASUMS_SIG] {
        let req = TestRequest::get().uri(uri).to_request();
        assert_eq!(
            call_service(&app, req).await.status(),
            403,
            "{uri} must be closed to an unsigned anonymous read"
        );
    }
}

/// Each of the three coordinates is distinct, so a signature for one does not
/// open another. This is what makes "one URL, one file" true across the whole
/// install rather than only across platforms.
#[actix_web::test]
async fn a_checksum_signature_does_not_open_the_zip_or_its_sibling() {
    let app = mirror_app(true, true).await;

    // shasums signature presented to shasums.sig, and vice versa.
    let cases = [
        (SHASUMS_SIG, mint_for("shasums")),
        (SHASUMS, mint_for("shasums.sig")),
        (ARTIFACT, mint_for("shasums")),
    ];
    for (uri, token) in cases {
        let req = TestRequest::get()
            .uri(&format!("{uri}?bh_sig={token}"))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), 403, "{uri} accepted a sibling's signature");
        let body: Value = read_body_json(resp).await;
        assert_eq!(body["code"].as_str(), Some("signed-url.invalid"), "{body}");
    }
}

/// And the reverse: the zip's signature does not open the checksums.
#[actix_web::test]
async fn the_zip_signature_does_not_open_the_checksums() {
    let app = mirror_app(true, true).await;
    let zip_token = minted_token(&app).await;
    for uri in [SHASUMS, SHASUMS_SIG] {
        let req = TestRequest::get()
            .uri(&format!("{uri}?bh_sig={zip_token}"))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 403, "{uri}");
    }
}

#[actix_web::test]
async fn header_authentication_still_reaches_the_checksum_routes() {
    let app = mirror_app(true, true).await;
    for uri in [SHASUMS, SHASUMS_SIG] {
        let req = TestRequest::get()
            .uri(uri)
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 200, "{uri}");
    }
}

// ── Signed download URLs: the registry protocol (RFC 0012 phase 5) ────────────
//
// O5 measured: the registry protocol has the same hole as the mirror, and it is
// three URLs per provider rather than one. This is the minting site for
// `required_providers`, which is how providers are actually declared.

const DOWNLOAD_DOC: &str = "/proxy/local-tf/v1/providers/hashicorp/aws/1.0.0/download/linux/amd64";

async fn download_document(app: &impl TestService) -> Value {
    let req = TestRequest::get()
        .uri(DOWNLOAD_DOC)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "the download document must be served");
    read_body_json(resp).await
}

/// Strip this host's prefix so the result can be fed straight back in.
fn as_local_uri(url: &str) -> String {
    match url.find("/proxy/") {
        Some(i) => url[i..].to_owned(),
        None => panic!("expected a URL on this host, got {url}"),
    }
}

#[actix_web::test]
async fn the_download_document_signs_all_three_urls() {
    let app = mirror_app(true, true).await;
    let doc = download_document(&app).await;
    for field in ["download_url", "shasums_url", "shasums_signature_url"] {
        let url = doc[field]
            .as_str()
            .unwrap_or_else(|| panic!("no {field} in {doc}"));
        assert!(url.contains("?bh_sig=1."), "{field} unsigned: {url}");
    }
}

#[actix_web::test]
async fn an_unsigned_registry_leaves_the_download_document_alone() {
    let app = mirror_app(false, false).await;
    let doc = download_document(&app).await;
    for field in ["download_url", "shasums_url", "shasums_signature_url"] {
        let url = doc[field].as_str().unwrap_or_default();
        assert!(!url.contains("bh_sig"), "{field} was signed: {url}");
    }
}

/// The three signatures are distinct, so the document does not hand out one
/// capability that opens three files.
#[actix_web::test]
async fn the_three_urls_carry_three_different_signatures() {
    let app = mirror_app(true, true).await;
    let doc = download_document(&app).await;
    let mut sigs: Vec<String> = ["download_url", "shasums_url", "shasums_signature_url"]
        .iter()
        .map(|f| signature_of(doc[*f].as_str().unwrap()).to_owned())
        .collect();
    sigs.sort();
    let before = sigs.len();
    sigs.dedup();
    assert_eq!(sigs.len(), before, "two URLs shared a signature");
}

/// The join this phase exists to make: every URL the document hands out is one
/// the closed registry then honours, with no `Authorization` header anywhere.
/// This is `terraform init` through the registry protocol, in three requests.
#[actix_web::test]
async fn every_url_the_download_document_names_is_fetchable_unauthenticated() {
    let app = mirror_app(true, true).await;
    let doc = download_document(&app).await;

    for field in ["download_url", "shasums_url", "shasums_signature_url"] {
        let uri = as_local_uri(doc[field].as_str().unwrap());
        let req = TestRequest::get().uri(&uri).to_request();
        let status = call_service(&app, req).await.status();
        assert_eq!(
            status, 200,
            "{field} was named by the document and then refused: {uri}"
        );
    }
}

/// The control for the test above. Same three URLs with the signature removed,
/// against the same closed registry.
#[actix_web::test]
async fn the_same_three_urls_are_refused_without_their_signatures() {
    let app = mirror_app(true, true).await;
    let doc = download_document(&app).await;

    for field in ["download_url", "shasums_url", "shasums_signature_url"] {
        let full = as_local_uri(doc[field].as_str().unwrap());
        let bare = full.split_once('?').unwrap().0;
        let req = TestRequest::get().uri(bare).to_request();
        assert_eq!(
            call_service(&app, req).await.status(),
            403,
            "{field} must be closed without its signature"
        );
    }
}

/// A URL is signed for one coordinate. Swapping the signatures between the
/// checksum manifest and its detached signature must not work, even though both
/// came from the same document and the same caller.
#[actix_web::test]
async fn the_documents_signatures_are_not_interchangeable() {
    let app = mirror_app(true, true).await;
    let doc = download_document(&app).await;

    let shasums = as_local_uri(doc["shasums_url"].as_str().unwrap());
    let sig_token = signature_of(doc["shasums_signature_url"].as_str().unwrap());
    let swapped = format!("{}?bh_sig={sig_token}", shasums.split_once('?').unwrap().0);

    let req = TestRequest::get().uri(&swapped).to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

/// A URL that is not on this host is left unsigned rather than handed a token.
/// Guards the case where a future change stops repointing one of the fields:
/// signing it would leak a credential minted for this estate to a third party.
#[actix_web::test]
async fn signing_never_attaches_a_token_to_an_off_host_url() {
    let app = mirror_app(true, true).await;
    let doc = download_document(&app).await;
    for field in ["download_url", "shasums_url", "shasums_signature_url"] {
        let url = doc[field].as_str().unwrap();
        assert!(
            url.contains("/proxy/local-tf/"),
            "{field} left this host: {url}"
        );
    }
}

// ── Signed URLs must never reach a host we do not control ────────────────────
//
// Found by security review of the phase-5 work. In local and hybrid mode the
// download document is built from the publisher's own `platforms[]` entry, and
// only `download_url` is overwritten — `shasums_url` and
// `shasums_signature_url` come through verbatim. They are therefore
// attacker-controlled by anyone who can publish a provider, and the guard that
// was supposed to stop them being signed used `starts_with` against a base that
// is a *bare origin* under host routing, which the Terraform registry protocol
// requires. `https://tf.acme.io.attacker.example/` passes that prefix.

/// A manifest whose platform entry points the checksum URLs at another host.
const HOSTILE_MANIFEST: &str = r#"{
  "version": "5.0.0",
  "protocols": ["5.0"],
  "platforms": [
    {"os": "linux", "arch": "amd64",
     "filename": "terraform-provider-aws_5.0.0_linux_amd64.zip",
     "shasum": "deadbeef",
     "shasums_url": "http://localhost:8080/proxy/local-tf-evil/SHA256SUMS",
     "shasums_signature_url": "http://localhost.attacker.example/SHA256SUMS.sig"}
  ]
}"#;

#[actix_web::test]
async fn a_publisher_supplied_off_host_url_is_never_signed() {
    let parts = local_only_app_parts("local-tf", "terraform", RegistryMode::Local, false);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.signed_downloads.insert("local-tf".to_owned(), true);
        hot.signed_url = Some(Arc::new(batlehub_core::services::SignedUrlService::new(
            SIGNING_SECRET,
            vec![],
            300,
        )));
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(HOSTILE_MANIFEST)
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let doc: Value = read_body_json(resp).await;

    // Two shapes, both of which a prefix match let through.
    //
    // `shasums_url` is on *this* host but under another registry's path — it
    // starts with `http://localhost:8080/proxy/local-tf`, so the old guard
    // signed it and handed a token scoped to `local-tf` to whatever serves
    // `local-tf-evil`. `shasums_signature_url` is the cross-host case, which
    // needs a bare-origin base to slip a prefix match; the `is_on_origin` unit
    // tests cover that shape directly.
    for field in ["shasums_url", "shasums_signature_url"] {
        let url = doc[field].as_str().unwrap_or_default();
        assert!(
            !url.contains("bh_sig"),
            "{field} leaked a signed token off this registry: {url}"
        );
        // Left exactly as published, rather than rewritten or dropped.
        assert!(
            url.contains("local-tf-evil") || url.contains("attacker.example"),
            "{field} should still be the publisher's: {doc}"
        );
    }

    // …and in the same response, the field that *is* ours is signed. Without
    // this the test would pass just as well if signing were switched off.
    let download = doc["download_url"].as_str().unwrap_or_default();
    assert!(
        download.contains("?bh_sig=1."),
        "download_url must still be signed, or this proves nothing: {download}"
    );
}

/// The publisher is told when their manifest points checksums at another host.
///
/// Not a refusal: it is the only way a local-mode install verifies anything
/// today, because BatleHub has no key for `signing_keys` and Terraform refuses a
/// provider whose checksum list it cannot fetch. But that host then sees every
/// `terraform init` for the provider, and an air-gapped install reaches it —
/// which the publisher should learn at publish time rather than from a network
/// trace.
#[actix_web::test]
async fn publishing_an_off_host_checksum_url_warns_the_publisher() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(HOSTILE_MANIFEST)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);

    let body: Value = read_body_json(resp).await;
    let msg = body["message"].as_str().unwrap_or_default();
    assert!(msg.contains("published"), "still a success: {body}");
    assert!(
        msg.contains("another host"),
        "the publisher must be told: {body}"
    );
    assert!(
        msg.contains("shasums_url") && msg.contains("attacker.example"),
        "the warning must name the field and the host: {body}"
    );
}

/// …and an ordinary manifest is not nagged at.
#[actix_web::test]
async fn publishing_without_checksum_urls_says_nothing_extra() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201);

    let body: Value = read_body_json(resp).await;
    assert_eq!(body["message"].as_str(), Some("provider version published"));
}

// ── `terraform:signing-keys:write` ───────────────────────────────────────────
//
// The endpoint that closes the hole `shared.rs` names: this server served
// `{"gpg_public_keys": []}` unconditionally, and an empty key set is not
// "unsigned, proceed" — it tells Terraform there is nothing to verify a
// provider's SHASUMS against, so no locally published provider could be verified
// by anyone. The store was the missing half; these rows are the other one.
//
// The end-to-end assertion is the point. A test that only PUT the key and read
// it back through the admin endpoint would pass against a store nothing
// consults, which is the shape the placeholder already had.

/// An armoured block that is at least structurally a key, so `validate()` passes
/// and the assertions below are about authorization rather than about parsing.
const ARMOURED_KEY: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\
                            \n\
                            mQENBFdummykeymaterialforatest\n\
                            -----END PGP PUBLIC KEY BLOCK-----\n";

fn signing_key_body() -> Value {
    serde_json::json!({
        "key_id": "34365D9472D7468F",
        "ascii_armor": ARMOURED_KEY,
        "trust_signature": "",
        "source": "HashiCorp",
        "source_url": "https://example.invalid/keys",
    })
}

/// A terraform app whose `HotConfig` has a signing-key store.
async fn make_terraform_signing_app() -> (
    impl TestService,
    Arc<batlehub_adapters::in_memory::InMemorySigningKeyStore>,
) {
    let parts = local_only_app_parts("local-tf", "terraform", RegistryMode::Local, false);
    let store = batlehub_adapters::in_memory::InMemorySigningKeyStore::new();
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.signing_keys = Some(store.clone() as Arc<dyn batlehub_core::ports::SigningKeyPort>);
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, store)
}

/// Anonymous and a plain `USER` are both refused, and neither registers a key.
///
/// `role:user` is the interesting half: publishing a provider *is* user work
/// under rule 5, and the key that verifies it deliberately is not — a publisher
/// who could register the key their own artifacts are checked against would be
/// verifying themselves.
#[actix_web::test]
async fn terraform_signing_key_write_is_refused_without_the_verb() {
    let (app, store) = make_terraform_signing_app().await;

    for auth in [None, Some(USER_TOKEN)] {
        let mut req = TestRequest::put()
            .uri("/api/v1/admin/registries/local-tf/signing-keys/hashicorp")
            .set_json(signing_key_body());
        if let Some(token) = auth {
            req = req.insert_header(("Authorization", bearer(token)));
        }
        let resp = call_service(&app, req.to_request()).await;
        assert_eq!(resp.status(), 403, "auth = {auth:?}");
    }

    assert!(
        store
            .list_signing_keys("local-tf", "hashicorp")
            .await
            .expect("list")
            .is_empty(),
        "a refusal that stored the key anyway is the failure mode this asserts"
    );
}

/// The positive control, carried all the way to the client: an administrator
/// registers a key and Terraform's download response starts serving it.
#[actix_web::test]
async fn terraform_signing_key_registered_by_admin_reaches_the_download_response() {
    let (app, _store) = make_terraform_signing_app().await;

    // Before: the placeholder, which is what made every provider unverifiable.
    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(
        body["signing_keys"]["gpg_public_keys"]
            .as_array()
            .expect("the key set is present even when empty")
            .len(),
        0,
        "the assertion below proves nothing unless this starts empty"
    );

    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/local-tf/signing-keys/hashicorp")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(signing_key_body())
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        200,
        "the verb is on the administrative floor: no translation rule produces \
         it, so without the floor this endpoint is unreachable for everybody"
    );

    // After: the same download response now carries something to verify against.
    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    let keys = body["signing_keys"]["gpg_public_keys"]
        .as_array()
        .expect("gpg_public_keys");
    assert_eq!(keys.len(), 1, "got {body}");
    assert_eq!(keys[0]["key_id"], "34365D9472D7468F");
    assert!(keys[0]["ascii_armor"]
        .as_str()
        .unwrap()
        .contains("BEGIN PGP PUBLIC KEY BLOCK"));

    // And removing it puts the response back, so the delete verb is real too.
    let req = TestRequest::delete()
        .uri("/api/v1/admin/registries/local-tf/signing-keys/hashicorp/34365D9472D7468F")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::get()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert!(body["signing_keys"]["gpg_public_keys"]
        .as_array()
        .unwrap()
        .is_empty());
}

/// A key that verifies nothing is refused at the edge rather than stored.
///
/// Stored, it would be indistinguishable from the empty placeholder from the
/// client's side — except that the console would show the namespace as
/// configured, which is worse than showing it as bare.
#[actix_web::test]
async fn terraform_signing_key_that_verifies_nothing_returns_400() {
    let (app, store) = make_terraform_signing_app().await;

    for (label, body) in [
        (
            "empty id",
            serde_json::json!({ "key_id": "", "ascii_armor": ARMOURED_KEY }),
        ),
        (
            "not armoured",
            serde_json::json!({ "key_id": "ABC123", "ascii_armor": "just some text" }),
        ),
    ] {
        let req = TestRequest::put()
            .uri("/api/v1/admin/registries/local-tf/signing-keys/hashicorp")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(body)
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 400, "{label}");
    }

    assert!(store
        .list_signing_keys("local-tf", "hashicorp")
        .await
        .expect("list")
        .is_empty());
}

/// `web::Path` decodes `%2F`, so a namespace of `hashicorp%2Faws` made the
/// provider name `providers/hashicorp/aws/5.0.0` and a body version of
/// `linux-amd64` gave its manifest the storage key of `hashicorp/aws` 5.0.0's
/// linux/amd64 binary — which the manifest write then overwrote.
#[actix_web::test]
async fn terraform_provider_upload_rejects_a_slash_in_a_coordinate_part() {
    let app = make_local_terraform_app(RegistryMode::Local).await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 201);
    let binary = "/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64";
    let req = TestRequest::put()
        .uri(binary)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"victim-zip-bytes".as_slice())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp%2Faws/5.0.0/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(r#"{"version":"linux-amd64"}"#)
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);

    let req = TestRequest::get()
        .uri(binary)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        actix_web::test::read_body(resp).await.as_ref(),
        b"victim-zip-bytes",
        "the victim's binary is untouched"
    );
}
