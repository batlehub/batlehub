//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_config::schema::RegistryMode;

// ── Local / Hybrid private Go module proxy ─────────────────────────────────

/// Build a minimal Go module zip with the given module path and version.
/// The zip contains `{module}@{version}/go.mod` and a stub source file.
fn make_go_module_zip(module: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        let mod_path = format!("{module}@{version}/go.mod");
        writer.start_file(mod_path, options).unwrap();
        writer
            .write_all(format!("module {module}\n\ngo 1.21\n").as_bytes())
            .unwrap();
        let src_path = format!("{module}@{version}/main.go");
        writer.start_file(src_path, options).unwrap();
        writer.write_all(b"package main\n").unwrap();
        writer.finish().unwrap();
    }
    buf.into_inner()
}

/// Build a test app with a single goproxy registry in the given mode.
/// Registry name is `"local-go"`, type `"goproxy"`.
async fn make_local_go_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-go", "goproxy", mode, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

#[actix_web::test]
async fn go_publish_user_can_publish() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let zip = make_go_module_zip("example.com/mymod", "v1.0.0");
    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/mymod/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["ok"], true);
}

/// An upper-case path is stored under the case encoding Go clients ask for,
/// whether it was uploaded raw or already encoded.
#[actix_web::test]
async fn go_publish_upper_case_module_is_served_under_its_escaped_path() {
    let app = make_local_go_app(RegistryMode::Local).await;
    for (uri, version) in [
        (
            "/proxy/local-go/github.com/Acme/Lib/@v/v1.0.0.zip",
            "v1.0.0",
        ),
        (
            "/proxy/local-go/github.com/!acme/!lib/@v/v1.1.0.zip",
            "v1.1.0",
        ),
    ] {
        let req = TestRequest::put()
            .uri(uri)
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_go_module_zip("github.com/Acme/Lib", version))
            .to_request();
        assert!(call_service(&app, req).await.status().is_success(), "{uri}");
    }
    let req = TestRequest::get()
        .uri("/proxy/local-go/github.com/!acme/!lib/@v/list")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    let list = std::str::from_utf8(&body).unwrap();
    assert!(list.contains("v1.0.0") && list.contains("v1.1.0"), "{list}");

    let req = TestRequest::get()
        .uri("/proxy/local-go/github.com/!acme/!lib/@v/v1.0.0.mod")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = read_body(call_service(&app, req).await).await;
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        "module github.com/Acme/Lib\n\ngo 1.21\n"
    );
}

#[actix_web::test]
async fn go_publish_duplicate_version_returns_409() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let zip = make_go_module_zip("example.com/dup", "v1.0.0");

    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/dup/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip.clone())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/dup/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 409);
}

#[actix_web::test]
async fn go_publish_anonymous_returns_401() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let zip = make_go_module_zip("example.com/mymod", "v1.0.0");
    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/mymod/@v/v1.0.0.zip")
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn go_publish_proxy_mode_returns_404() {
    let app = make_app(InMemoryRepo::new()).await;
    let zip = make_go_module_zip("example.com/mymod", "v1.0.0");
    let req = TestRequest::put()
        .uri("/proxy/go/example.com/mymod/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn go_version_list_returns_published_version() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let zip = make_go_module_zip("example.com/mymod", "v1.0.0");

    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/mymod/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-go/example.com/mymod/@v/list")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    let list = std::str::from_utf8(&body).unwrap();
    assert!(
        list.contains("v1.0.0"),
        "version list must include published version"
    );
}

#[actix_web::test]
async fn go_info_returns_version_metadata() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let zip = make_go_module_zip("example.com/infomod", "v2.0.0");

    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/infomod/@v/v2.0.0.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri("/proxy/local-go/example.com/infomod/@v/v2.0.0.info")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["Version"], "v2.0.0");
    assert!(
        body["Time"].as_str().is_some(),
        "Time field must be present"
    );
}

#[actix_web::test]
async fn go_mod_returns_extracted_go_mod() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let module = "example.com/modfile";
    let version = "v0.1.0";
    let zip = make_go_module_zip(module, version);

    let req = TestRequest::put()
        .uri(&format!("/proxy/local-go/{module}/@v/{version}.zip"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip)
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri(&format!("/proxy/local-go/{module}/@v/{version}.mod"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    let content = std::str::from_utf8(&body).unwrap();
    assert!(
        content.contains(module),
        "go.mod must contain the module path"
    );
}

#[actix_web::test]
async fn go_zip_download_returns_artifact() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let module = "example.com/dlmod";
    let version = "v1.1.0";
    let zip_bytes = make_go_module_zip(module, version);

    let req = TestRequest::put()
        .uri(&format!("/proxy/local-go/{module}/@v/{version}.zip"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(zip_bytes.clone())
        .to_request();
    call_service(&app, req).await;

    let req = TestRequest::get()
        .uri(&format!("/proxy/local-go/{module}/@v/{version}.zip"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = read_body(resp).await;
    assert_eq!(body.as_ref(), zip_bytes.as_slice());
}

#[actix_web::test]
async fn go_latest_returns_most_recent_version() {
    let app = make_local_go_app(RegistryMode::Local).await;

    for v in ["v1.0.0", "v1.1.0", "v2.0.0"] {
        let zip = make_go_module_zip("example.com/latestmod", v);
        let req = TestRequest::put()
            .uri(&format!("/proxy/local-go/example.com/latestmod/@v/{v}.zip"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .insert_header(("Content-Type", "application/zip"))
            .set_payload(zip)
            .to_request();
        call_service(&app, req).await;
    }

    let req = TestRequest::get()
        .uri("/proxy/local-go/example.com/latestmod/@latest")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["Version"], "v2.0.0");
}

#[actix_web::test]
async fn go_info_unknown_returns_404() {
    let app = make_local_go_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-go/example.com/nomod/@v/v9.9.9.info")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}
