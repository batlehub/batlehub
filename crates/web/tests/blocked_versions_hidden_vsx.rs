//! Blocking an extension version hides it from the gallery and from the
//! OpenVSX API.
//!
//! The coverage table generated from `RegistryKind::listing_filter()` now says
//! "yes" for both extension kinds, so this is what makes that true. Both client
//! protocols render from one entry list and the filter sits on that list, so a
//! version hidden from one is hidden from the other by construction — these
//! tests pin that rather than assume it.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use std::io::Write;

use actix_web::test::{call_service, read_body_json, TestRequest};
use batlehub_config::schema::RegistryMode;
use serde_json::{json, Value};

const EXT: &str = "acme.tool";

fn vsix(version: &str) -> Vec<u8> {
    let manifest = json!({
        "publisher": "acme", "name": "tool", "version": version,
        "displayName": "Acme Tool", "engines": { "vscode": "^1.85.0" }
    })
    .to_string();
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("extension/package.json", opts).unwrap();
        w.write_all(manifest.as_bytes()).unwrap();
        w.finish().unwrap();
    }
    buf
}

/// A registry holding two versions of one extension.
async fn app() -> impl TestService {
    let app = registry_app("local-vsx", "openvsx", RegistryMode::Local).await;

    for v in ["1.0.0", "1.1.0"] {
        let req = TestRequest::put()
            .uri(&format!("/proxy/local-vsx/{EXT}/{v}/vsix"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_payload(vsix(v))
            .to_request();
        let resp = call_service(&app, req).await;
        assert!(resp.status().is_success(), "publish {v}: {}", resp.status());
    }
    app
}

async fn gallery_versions<S: TestService>(app: &S) -> Vec<String> {
    let req = TestRequest::post()
        .uri("/proxy/local-vsx/vscode/gallery/extensionquery")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(json!({
            "filters": [{ "criteria": [{ "filterType": 7, "value": EXT }] }],
            "flags": 0x1
        }))
        .to_request();
    let doc: Value = read_body_json(call_service(app, req).await).await;
    doc["results"][0]["extensions"][0]["versions"]
        .as_array()
        .map(|vs| {
            vs.iter()
                .map(|v| v["version"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn api_versions<S: TestService>(app: &S) -> Vec<String> {
    let doc: Value = get_json(app, "/proxy/local-vsx/api/acme/tool").await;
    let mut vs: Vec<String> = doc["allVersions"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    vs.sort();
    vs
}

#[actix_web::test]
async fn a_blocked_version_leaves_the_gallery() {
    let app = app().await;
    assert_eq!(gallery_versions(&app).await, ["1.1.0", "1.0.0"]);

    block_version(&app, "local-vsx", EXT, "1.1.0").await;

    assert_eq!(
        gallery_versions(&app).await,
        ["1.0.0"],
        "the editor must not be offered a version it will then be refused"
    );
}

/// Both protocols render from one entry list, so neither can disagree with the
/// other about what exists.
#[actix_web::test]
async fn the_openvsx_api_hides_the_same_version() {
    let app = app().await;
    assert_eq!(api_versions(&app).await, ["1.0.0", "1.1.0"]);

    block_version(&app, "local-vsx", EXT, "1.1.0").await;

    assert_eq!(api_versions(&app).await, ["1.0.0"]);
}

/// The newest *allowed* version becomes the one the API reports, so an
/// `ovsx get` with no version pinned resolves to something installable.
#[actix_web::test]
async fn the_reported_newest_version_moves_to_an_allowed_one() {
    let app = app().await;

    block_version(&app, "local-vsx", EXT, "1.1.0").await;

    let doc: Value = get_json(&app, "/proxy/local-vsx/api/acme/tool").await;
    assert_eq!(doc["version"], "1.0.0");
}

/// Hiding governs resolution, not diagnosis: a client that names the blocked
/// version still gets the operator's `403` and reason.
#[actix_web::test]
async fn a_direct_download_of_a_blocked_version_is_still_denied() {
    let app = app().await;

    block_version(&app, "local-vsx", EXT, "1.1.0").await;

    for uri in [
        format!("/proxy/local-vsx/{EXT}/1.1.0/vsix"),
        "/proxy/local-vsx/vscode/gallery/publishers/acme/vsextensions/tool/1.1.0/vspackage"
            .to_owned(),
        "/proxy/local-vsx/vscode/asset/acme/tool/1.1.0/Microsoft.VisualStudio.Services.VSIXPackage"
            .to_owned(),
    ] {
        let req = admin_get(&uri);
        assert_eq!(
            call_service(&app, req).await.status(),
            403,
            "{uri} should be refused with the operator's reason"
        );
    }
}

#[actix_web::test]
async fn blocking_every_version_leaves_no_installable_extension() {
    let app = app().await;

    for v in ["1.0.0", "1.1.0"] {
        block_version(&app, "local-vsx", EXT, v).await;
    }

    assert!(gallery_versions(&app).await.is_empty());

    let req = admin_get("/proxy/local-vsx/api/acme/tool");
    let status = call_service(&app, req).await.status();
    assert!(
        status == 403 || status == 404,
        "an extension with nothing installable must not resolve; got {status}"
    );
}

#[actix_web::test]
async fn another_extension_is_untouched() {
    let app = app().await;

    let req = TestRequest::put()
        .uri("/proxy/local-vsx/acme.other/1.1.0/vsix")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_payload(vsix("1.1.0"))
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());

    block_version(&app, "local-vsx", EXT, "1.1.0").await;

    let doc: Value = get_json(&app, "/proxy/local-vsx/api/acme/other").await;
    assert_eq!(doc["version"], "1.1.0");
}
