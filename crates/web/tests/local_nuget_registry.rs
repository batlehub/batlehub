//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_config::schema::RegistryMode;

// ══ NuGet local registry tests ════════════════════════════════════════════════

/// Build a minimal in-memory .nupkg (ZIP) containing a .nuspec with the given id/version.
fn make_sample_nupkg(id: &str, version: &str, description: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let nuspec = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{id}</id>
    <version>{version}</version>
    <description>{description}</description>
    <authors>TestAuthor</authors>
    <tags>test</tags>
  </metadata>
</package>"#
    );

    let mut buf = Vec::new();
    let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
    let opts = SimpleFileOptions::default();
    zip.start_file(format!("{id}.nuspec"), opts).unwrap();
    zip.write_all(nuspec.as_bytes()).unwrap();
    zip.finish().unwrap();
    buf
}

/// Wrap a .nupkg in a `multipart/form-data` body and return `(body_bytes, content_type_header)`.
fn make_nuget_publish_body(nupkg: &[u8]) -> (Vec<u8>, String) {
    let boundary = "nugetboundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"package\"; filename=\"package.nupkg\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    ).into_bytes();
    body.extend_from_slice(nupkg);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let ct = format!("multipart/form-data; boundary={boundary}");
    (body, ct)
}

#[actix_web::test]
async fn nuget_service_index_returns_valid_json() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    let body: Value = user_json(&app, "/proxy/local-nuget/nuget/v3/index.json").await;
    assert_eq!(body["version"], "3.0.0");
    assert!(body["resources"]
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false));
}

#[actix_web::test]
async fn nuget_service_index_includes_vulnerabilities_url_resource() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    let body: Value = user_json(&app, "/proxy/local-nuget/nuget/v3/index.json").await;
    let resources = body["resources"]
        .as_array()
        .expect("resources must be an array");
    let vuln_resource = resources
        .iter()
        .find(|r| r["@type"].as_str() == Some("VulnerabilitiesUrl/6.7.0"))
        .expect("service index must contain a VulnerabilitiesUrl/6.7.0 resource");
    let id = vuln_resource["@id"].as_str().expect("@id must be a string");
    assert!(
        id.contains("/proxy/local-nuget/nuget/v3/vulnerabilities/"),
        "@id must point to this server's vulnerability endpoint, got: {id}"
    );
}

/// PUT a freshly packed `.nupkg` to `uri` as the admin, and answer with the
/// status. The URI is a parameter because `dotnet nuget push` appends a
/// trailing slash and the symbol package has a path of its own.
async fn publish_to<S: TestService>(
    app: &S,
    uri: &str,
    id: &str,
    version: &str,
    description: &str,
) -> actix_web::http::StatusCode {
    let nupkg = make_sample_nupkg(id, version, description);
    let (body, ct) = make_nuget_publish_body(&nupkg);
    let req = TestRequest::put()
        .uri(uri)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header(("Content-Type", ct))
        .set_payload(body)
        .to_request();
    call_service(app, req).await.status()
}

/// [`publish_to`] on the unslashed publish path, which is what everything but
/// the two trailing-slash regressions uses.
async fn publish<S: TestService>(
    app: &S,
    id: &str,
    version: &str,
    description: &str,
) -> actix_web::http::StatusCode {
    publish_to(
        app,
        "/proxy/local-nuget/nuget/api/v2/package",
        id,
        version,
        description,
    )
    .await
}

/// A read as an ordinary user — the consumer side of every publish here.
async fn user_json<S: TestService>(app: &S, uri: &str) -> Value {
    let req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "{uri} should be served");
    read_body_json(resp).await
}

#[actix_web::test]
async fn nuget_publish_creates_version() {
    let app = make_local_nuget_app(RegistryMode::Local).await;

    assert_eq!(publish(&app, "MyLib", "1.0.0", "A test library").await, 201);

    // Version should now appear in flat container
    let body2: Value = user_json(&app, "/proxy/local-nuget/nuget/v3/flat/mylib/index.json").await;
    let versions = body2["versions"].as_array().unwrap();
    assert!(versions.iter().any(|v| v == "1.0.0"));
}

#[actix_web::test]
async fn nuget_publish_requires_auth() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    let nupkg = make_sample_nupkg("MyLib", "1.0.0", "Test");
    let (body, ct) = make_nuget_publish_body(&nupkg);

    let req = TestRequest::put()
        .uri("/proxy/local-nuget/nuget/api/v2/package")
        .insert_header(("Content-Type", ct))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn nuget_publish_duplicate_returns_409() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    assert_eq!(publish(&app, "MyLib", "1.0.0", "Test").await, 201);
    assert_eq!(
        publish(&app, "MyLib", "1.0.0", "Test").await,
        409,
        "the second publish of the same coordinate is a conflict"
    );
}

#[actix_web::test]
async fn nuget_xnuget_apikey_header_authenticates() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    let nupkg = make_sample_nupkg("KeyLib", "0.1.0", "Test ApiKey auth");
    let (body, ct) = make_nuget_publish_body(&nupkg);

    // Use X-NuGet-ApiKey instead of Authorization: Bearer
    let req = TestRequest::put()
        .uri("/proxy/local-nuget/nuget/api/v2/package")
        .insert_header(("X-NuGet-ApiKey", ADMIN_TOKEN))
        .insert_header(("Content-Type", ct))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        201,
        "X-NuGet-ApiKey should authenticate like Bearer"
    );
}

#[actix_web::test]
async fn nuget_yank_removes_from_versions() {
    let app = make_local_nuget_app(RegistryMode::Local).await;

    // Publish first
    assert_eq!(publish(&app, "YankLib", "2.0.0", "Yank test").await, 201);

    // Yank it
    let req_yank = TestRequest::delete()
        .uri("/proxy/local-nuget/nuget/v2/package/yanklib/2.0.0")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req_yank).await.status(), 204);

    // Versions list should be empty (yanked packages are excluded)
    let body_list: Value =
        user_json(&app, "/proxy/local-nuget/nuget/v3/flat/yanklib/index.json").await;
    let versions = body_list["versions"].as_array().unwrap();
    assert!(
        versions.is_empty(),
        "yanked version should not appear in flat container versions list"
    );
}

#[actix_web::test]
async fn nuget_registration_local_has_catalog_entry() {
    let app = make_local_nuget_app(RegistryMode::Local).await;

    assert_eq!(
        publish(&app, "RegLib", "1.0.0", "Registration test").await,
        201
    );

    let body2: Value = user_json(
        &app,
        "/proxy/local-nuget/nuget/v3/registration5/reglib/index.json",
    )
    .await;
    assert!(body2["count"].as_u64().unwrap_or(0) >= 1);
    let items = body2["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let leaf_items = items[0]["items"].as_array().unwrap();
    assert!(!leaf_items.is_empty());
    let entry = &leaf_items[0]["catalogEntry"];
    assert_eq!(entry["version"], "1.0.0");
}

#[actix_web::test]
async fn nuget_publish_proxy_mode_returns_404() {
    let app = make_local_nuget_app(RegistryMode::Proxy).await;

    assert_eq!(publish(&app, "PxLib", "1.0.0", "test").await, 404);
}

#[actix_web::test]
async fn nuget_publish_traversal_id_returns_400() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    // A traversal sequence in the package ID (inside the .nuspec) must be rejected
    // by validate_package_name before reaching the storage layer — clean 400.
    assert_eq!(
        publish(&app, "../../etc/x", "1.0.0", "traversal test").await,
        400
    );
}

#[actix_web::test]
async fn nuget_publish_traversal_version_returns_400() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    assert_eq!(
        publish(&app, "SafeLib", "../../etc/x", "traversal test").await,
        400
    );
}

#[actix_web::test]
async fn nuget_flat_download_local_returns_nupkg() {
    let app = make_local_nuget_app(RegistryMode::Local).await;

    assert_eq!(publish(&app, "DlLib", "1.0.0", "Download test").await, 201);

    let req_dl = TestRequest::get()
        .uri("/proxy/local-nuget/nuget/v3/flat/dllib/1.0.0/dllib.1.0.0.nupkg")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp_dl = call_service(&app, req_dl).await;
    assert_eq!(resp_dl.status(), 200);
    let bytes = read_body(resp_dl).await;
    assert!(
        !bytes.is_empty(),
        "nupkg download should return artifact bytes"
    );
}

#[actix_web::test]
async fn nuget_flat_download_local_returns_nuspec() {
    let app = make_local_nuget_app(RegistryMode::Local).await;

    assert_eq!(
        publish(&app, "NuspecLib", "2.0.0", "Nuspec extract test").await,
        201
    );

    let req_nuspec = TestRequest::get()
        .uri("/proxy/local-nuget/nuget/v3/flat/nuspeclib/2.0.0/nuspeclib.2.0.0.nuspec")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp_nuspec = call_service(&app, req_nuspec).await;
    assert_eq!(resp_nuspec.status(), 200);
    let body_bytes = read_body(resp_nuspec).await;
    let xml = std::str::from_utf8(&body_bytes).unwrap();
    assert!(
        xml.contains("<id>NuspecLib</id>"),
        "nuspec should contain the package id"
    );
    assert!(
        xml.contains("<version>2.0.0</version>"),
        "nuspec should contain the version"
    );
}

#[actix_web::test]
async fn nuget_search_local_returns_packages() {
    let app = make_local_nuget_app(RegistryMode::Local).await;

    assert_eq!(publish(&app, "SearchMe", "1.0.0", "Search test").await, 201);

    let body2: Value = user_json(&app, "/proxy/local-nuget/nuget/v3/query?q=search").await;
    assert!(
        body2["totalHits"].as_u64().unwrap_or(0) >= 1,
        "search should return at least one hit"
    );
}

#[actix_web::test]
async fn nuget_flat_download_missing_returns_404() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-nuget/nuget/v3/flat/ghost/9.9.9/ghost.9.9.9.nupkg")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

/// `dotnet nuget push` sends the publish URL with a trailing slash — RFC 0009 §12.16.
///
/// The client reads `PackagePublish/2.0.0` out of the service index and appends
/// `/` before PUTting, so what arrives is `…/api/v2/package/`. Every test above
/// uses the unslashed path, the service index advertises the unslashed path,
/// and the route was registered only for that — so `dotnet nuget push` got a
/// `404` from a registry whose publish endpoint works perfectly when curl'd.
/// Found by `tests/heavy/nuget.sh`.
#[actix_web::test]
async fn nuget_publish_accepts_the_trailing_slash_dotnet_sends() {
    let app = make_local_nuget_app(RegistryMode::Local).await;

    assert_eq!(
        publish_to(
            &app,
            "/proxy/local-nuget/nuget/api/v2/package/",
            "SlashLib",
            "1.0.0",
            "A test library"
        )
        .await,
        201
    );

    let doc: Value = user_json(&app, "/proxy/local-nuget/nuget/v3/flat/slashlib/index.json").await;
    assert!(doc["versions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "1.0.0"));
}

/// The same for symbol packages: `dotnet nuget push` of a `.snupkg` appends the
/// same slash to `SymbolPackagePublish/4.9.0`.
#[actix_web::test]
async fn nuget_symbol_publish_accepts_the_trailing_slash() {
    let app = make_local_nuget_app(RegistryMode::Local).await;
    assert_eq!(
        publish(&app, "SymLib", "1.0.0", "A test library").await,
        201
    );

    let status = publish_to(
        &app,
        "/proxy/local-nuget/nuget/api/v2/symbolpackage/",
        "SymLib",
        "1.0.0",
        "Symbols",
    )
    .await;
    assert_ne!(
        status, 404,
        "the slashed symbol-publish path must reach the handler, not the route table"
    );
}
