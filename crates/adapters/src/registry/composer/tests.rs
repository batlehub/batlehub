use super::client::ComposerRegistryClient;
use super::local::parse_composer_zip;
use batlehub_core::{entities::PackageId, error::CoreError, ports::RegistryClient};
use bytes::Bytes;
use futures::TryStreamExt;
use mockito::Server;

fn client(url: &str) -> ComposerRegistryClient {
    ComposerRegistryClient::new(url, &Default::default()).unwrap()
}

/// Build a minimal Packagist v2 JSON response for one package + version.
fn p2_json(package: &str, version: &str, dist_url: &str) -> String {
    serde_json::json!({
        "packages": {
            package: [{
                "version": version,
                "dist": {
                    "type": "zip",
                    "url": dist_url,
                    "shasum": "deadbeef00000000000000000000000000000000"
                },
                "time": "2024-06-01T12:00:00+00:00",
                "description": "A test package"
            }]
        },
        "minified": "composer/2.0"
    })
    .to_string()
}

// ── resolve_metadata ──────────────────────────────────────────────────────────

#[tokio::test]
async fn resolve_metadata_returns_correct_fields() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/p2/symfony/console.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(p2_json(
            "symfony/console",
            "v7.2.0",
            "https://example.com/dist.zip",
        ))
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "symfony/console", "v7.2.0");
    let meta = c.resolve_metadata(&pkg).await.unwrap();

    assert_eq!(meta.id.version, "v7.2.0");
    assert_eq!(
        meta.checksum.as_deref(),
        Some("deadbeef00000000000000000000000000000000")
    );
    assert!(meta.published_at.is_some());
    // No download_url when artifact is None
    assert!(meta.download_url.is_none());
}

#[tokio::test]
async fn resolve_metadata_sets_download_url_for_dist_artifact() {
    let mut server = Server::new_async().await;
    let dist_url = format!("{}/dist/symfony-console.zip", server.url());
    let _mock = server
        .mock("GET", "/p2/symfony/console.json")
        .with_status(200)
        .with_body(p2_json("symfony/console", "v7.2.0", &dist_url))
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "symfony/console", "v7.2.0").with_artifact("dist");
    let meta = c.resolve_metadata(&pkg).await.unwrap();

    assert_eq!(meta.download_url.as_deref(), Some(dist_url.as_str()));
}

#[tokio::test]
async fn resolve_metadata_p2_artifact_returns_url_without_upstream_call() {
    // artifact="p2" must return immediately — no p2 endpoint should be hit.
    let server = Server::new_async().await;
    // No mock registered — any HTTP call would panic.
    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "vendor/package", "_index").with_artifact("p2");
    let meta = c.resolve_metadata(&pkg).await.unwrap();

    assert!(
        meta.download_url
            .as_deref()
            .unwrap_or("")
            .contains("/p2/vendor/package.json"),
        "download_url must point to the p2 endpoint"
    );
}

#[tokio::test]
async fn resolve_metadata_package_not_found_returns_not_found_error() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/p2/missing/pkg.json")
        .with_status(404)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "missing/pkg", "v1.0.0");
    assert!(matches!(
        c.resolve_metadata(&pkg).await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn resolve_metadata_version_not_in_p2_returns_not_found() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/p2/vendor/pkg.json")
        .with_status(200)
        .with_body(p2_json("vendor/pkg", "v1.0.0", "https://example.com/a.zip"))
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "vendor/pkg", "v9.9.9");
    assert!(matches!(
        c.resolve_metadata(&pkg).await,
        Err(CoreError::NotFound(_))
    ));
}

// ── fetch_artifact ────────────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_artifact_p2_streams_raw_json_bytes() {
    let mut server = Server::new_async().await;
    let body = p2_json("vendor/pkg", "v1.0.0", "https://example.com/dist.zip");
    let _mock = server
        .mock("GET", "/p2/vendor/pkg.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.clone())
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "vendor/pkg", "_index").with_artifact("p2");
    let fetched = c.fetch_artifact(&pkg).await.unwrap();
    let chunks: Vec<_> = fetched.stream.try_collect().await.unwrap();
    let content: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
    assert_eq!(content, body.as_bytes());
}

#[tokio::test]
async fn fetch_artifact_dist_streams_zip_bytes() {
    let mut server = Server::new_async().await;
    let dist_path = "/archives/vendor-pkg-v1.0.0.zip";
    let dist_url = format!("{}{}", server.url(), dist_path);
    let zip_bytes: &[u8] = b"PK\x03\x04fake-zip-content";

    let _p2_mock = server
        .mock("GET", "/p2/vendor/pkg.json")
        .with_status(200)
        .with_body(p2_json("vendor/pkg", "v1.0.0", &dist_url))
        .create_async()
        .await;

    let _zip_mock = server
        .mock("GET", dist_path)
        .with_status(200)
        .with_header("content-type", "application/zip")
        .with_body(zip_bytes)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "vendor/pkg", "v1.0.0").with_artifact("dist");
    let fetched = c.fetch_artifact(&pkg).await.unwrap();
    let chunks: Vec<_> = fetched.stream.try_collect().await.unwrap();
    let content: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
    assert_eq!(content, zip_bytes);
}

#[tokio::test]
async fn fetch_artifact_not_found_returns_not_found_error() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/p2/missing/pkg.json")
        .with_status(404)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "missing/pkg", "v1.0.0").with_artifact("dist");
    assert!(matches!(
        c.fetch_artifact(&pkg).await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn fetch_artifact_dist_propagates_cache_control() {
    let mut server = Server::new_async().await;
    let dist_path = "/dist/pkg.zip";
    let dist_url = format!("{}{}", server.url(), dist_path);

    let _p2_mock = server
        .mock("GET", "/p2/vendor/pkg.json")
        .with_status(200)
        .with_body(p2_json("vendor/pkg", "v1.0.0", &dist_url))
        .create_async()
        .await;

    let _zip_mock = server
        .mock("GET", dist_path)
        .with_status(200)
        .with_header("cache-control", "max-age=86400")
        .with_body(b"data".as_slice())
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("pkgist", "vendor/pkg", "v1.0.0").with_artifact("dist");
    let fetched = c.fetch_artifact(&pkg).await.unwrap();
    assert_eq!(fetched.cache_control.as_deref(), Some("max-age=86400"));
}

// ── list_versions ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_versions_returns_all_versions() {
    let mut server = Server::new_async().await;
    let body = serde_json::json!({
        "packages": {
            "symfony/console": [
                {"version": "v7.2.0"},
                {"version": "v7.1.0"},
                {"version": "v6.4.0"}
            ]
        }
    })
    .to_string();

    let _mock = server
        .mock("GET", "/p2/symfony/console.json")
        .with_status(200)
        .with_body(body)
        .create_async()
        .await;

    let c = client(&server.url());
    let versions = c.list_versions("symfony/console").await.unwrap();
    assert_eq!(versions.len(), 3);
    assert!(versions.contains(&"v7.2.0".to_owned()));
    assert!(versions.contains(&"v6.4.0".to_owned()));
}

#[tokio::test]
async fn list_versions_not_found_returns_error() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("GET", "/p2/unknown/pkg.json")
        .with_status(404)
        .create_async()
        .await;

    let c = client(&server.url());
    assert!(matches!(
        c.list_versions("unknown/pkg").await,
        Err(CoreError::NotFound(_))
    ));
}

// ── parse_composer_zip ────────────────────────────────────────────────────────

fn make_zip_with_file(filename: &str, content: &[u8]) -> Bytes {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file(filename, opts).unwrap();
        writer.write_all(content).unwrap();
        writer.finish().unwrap();
    }
    Bytes::from(buf.into_inner())
}

fn cjson(name: &str, version: &str) -> Vec<u8> {
    serde_json::json!({ "name": name, "version": version, "description": "test" })
        .to_string()
        .into_bytes()
}

#[test]
fn parse_composer_zip_refuses_a_decompression_bomb() {
    // 11 MiB of spaces deflates to a few KiB: over the 10 MiB composer.json cap.
    let data = make_zip_with_file("composer.json", &vec![b' '; 11 * 1024 * 1024]);
    assert!(data.len() < 1024 * 1024, "the fixture must be a bomb");
    let err = parse_composer_zip(&data, None).err().expect("refused");
    assert!(err.to_string().contains("byte limit"), "{err}");
}

#[test]
fn parse_composer_zip_root_level() {
    let data = make_zip_with_file("composer.json", &cjson("vendor/mypkg", "v1.0.0"));
    let meta = parse_composer_zip(&data, None).unwrap();
    assert_eq!(meta.name, "vendor/mypkg");
    assert_eq!(meta.version, "v1.0.0");
}

#[test]
fn parse_composer_zip_github_style_nested() {
    // GitHub zipball layout: <vendor>-<pkg>-<sha>/composer.json
    let data = make_zip_with_file(
        "vendor-mypkg-abc1234/composer.json",
        &cjson("vendor/mypkg", "v2.3.0"),
    );
    let meta = parse_composer_zip(&data, None).unwrap();
    assert_eq!(meta.name, "vendor/mypkg");
    assert_eq!(meta.version, "v2.3.0");
}

#[test]
fn parse_composer_zip_version_override_wins() {
    let data = make_zip_with_file("composer.json", &cjson("vendor/mypkg", "v1.0.0"));
    let meta = parse_composer_zip(&data, Some("v99.0.0")).unwrap();
    assert_eq!(meta.version, "v99.0.0");
}

#[test]
fn parse_composer_zip_no_version_field_without_override_returns_error() {
    let json = serde_json::json!({ "name": "vendor/pkg", "description": "no version" })
        .to_string()
        .into_bytes();
    let data = make_zip_with_file("composer.json", &json);
    assert!(parse_composer_zip(&data, None).is_err());
}

#[test]
fn parse_composer_zip_no_composer_json_returns_error() {
    let data = make_zip_with_file("README.md", b"hello world");
    assert!(parse_composer_zip(&data, None).is_err());
}

#[test]
fn parse_composer_zip_invalid_name_no_slash_returns_error() {
    let json = serde_json::json!({ "name": "noslash", "version": "v1.0.0" })
        .to_string()
        .into_bytes();
    let data = make_zip_with_file("composer.json", &json);
    assert!(parse_composer_zip(&data, None).is_err());
}

#[test]
fn parse_composer_zip_preserves_full_json() {
    let data = make_zip_with_file("composer.json", &cjson("vendor/mypkg", "v1.0.0"));
    let meta = parse_composer_zip(&data, None).unwrap();
    assert_eq!(meta.composer_json["name"], "vendor/mypkg");
    assert_eq!(meta.composer_json["description"], "test");
}

#[test]
fn parse_composer_zip_path_traversal_name_rejected() {
    // Names with '..' components must be rejected to prevent storage path traversal.
    for bad_name in &[
        "vendor/../../etc/shadow",
        "a/../b/c",
        "../vendor/pkg",
        "vendor/pkg/extra",
    ] {
        let json = serde_json::json!({ "name": bad_name, "version": "v1.0.0" })
            .to_string()
            .into_bytes();
        let data = make_zip_with_file("composer.json", &json);
        assert!(
            parse_composer_zip(&data, None).is_err(),
            "expected error for name '{bad_name}'"
        );
    }
}

#[test]
fn parse_composer_zip_multi_root_zip_rejected() {
    // A ZIP with two top-level directories each containing composer.json is ambiguous
    // and must be rejected rather than silently picking one.
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("dir-a/composer.json", opts).unwrap();
        writer.write_all(&cjson("vendor/a", "v1.0.0")).unwrap();
        writer.start_file("dir-b/composer.json", opts).unwrap();
        writer.write_all(&cjson("vendor/b", "v2.0.0")).unwrap();
        writer.finish().unwrap();
    }
    let data = Bytes::from(buf.into_inner());
    assert!(
        parse_composer_zip(&data, None).is_err(),
        "ambiguous multi-root ZIP must fail"
    );
}

#[tokio::test]
async fn resolve_metadata_p2_passthrough_needs_no_http() {
    // p2 / p2~dev artifacts return synthetic metadata pointing at the upstream
    // URL without any request — an unreachable base proves it never connects.
    let c = client("http://127.0.0.1:1");
    let pkg = PackageId::new("composer", "monolog/monolog", "_").with_artifact("p2");
    let md = c.resolve_metadata(&pkg).await.unwrap();
    assert_eq!(
        md.download_url.as_deref(),
        Some("http://127.0.0.1:1/p2/monolog/monolog.json")
    );

    let dev = PackageId::new("composer", "monolog/monolog", "_").with_artifact("p2~dev");
    let md_dev = c.resolve_metadata(&dev).await.unwrap();
    assert_eq!(
        md_dev.download_url.as_deref(),
        Some("http://127.0.0.1:1/p2/monolog/monolog~dev.json")
    );
    assert_eq!(c.registry_type(), "composer");
}

#[tokio::test]
async fn list_versions_returns_p2_versions() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/p2/monolog/monolog.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(p2_json("monolog/monolog", "2.0.0", "https://x/d.zip"))
        .create_async()
        .await;
    let c = client(&server.url());
    assert_eq!(
        c.list_versions("monolog/monolog").await.unwrap(),
        vec!["2.0.0"]
    );
}

#[tokio::test]
async fn search_packages_maps_results() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"^/search\.json".to_string()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "results": [{"name": "monolog/monolog", "description": "logging"}]
            })
            .to_string(),
        )
        .create_async()
        .await;
    let c = client(&server.url());
    let results = c.search_packages("monolog", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "monolog/monolog");
    assert_eq!(results[0].latest_version, "latest");
}
