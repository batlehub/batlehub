use super::*;
use batlehub_core::entities::PackageId;
use batlehub_core::error::CoreError;
use batlehub_core::ports::{DocumentKind, RegistryClient};
use client::{rewrite_simple_html, rewrite_simple_json};
use futures::TryStreamExt;

#[test]
fn name_normalization() {
    assert_eq!(normalize_name("Pillow"), "pillow");
    assert_eq!(normalize_name("my_pkg"), "my-pkg");
    assert_eq!(normalize_name("A.B.C"), "a-b-c");
    assert_eq!(normalize_name("My--Package"), "my-package");
    assert_eq!(normalize_name("requests"), "requests");
}

#[test]
fn rewrite_simple_html_rewrites_cdn_urls() {
    let html = br#"<a href="https://files.pythonhosted.org/packages/ab/cd/requests-2.28.0.tar.gz#sha256=abc">requests-2.28.0.tar.gz</a>"#;
    let out = rewrite_simple_html(html, "http://localhost:8080/proxy/my-pypi");
    let out_str = std::str::from_utf8(&out).unwrap();
    assert!(out_str.contains("/proxy/my-pypi/packages/requests-2.28.0.tar.gz#sha256=abc"));
    assert!(!out_str.contains("files.pythonhosted.org"));
}

#[test]
fn rewrite_simple_html_keeps_relative_hrefs() {
    let html = br#"<a href="/simple/">index</a>"#;
    let out = rewrite_simple_html(html, "http://localhost:8080/proxy/my-pypi");
    let out_str = std::str::from_utf8(&out).unwrap();
    assert!(out_str.contains(r#"href="/simple/""#));
}

#[test]
fn rewrite_simple_json_rewrites_urls() {
    let json = serde_json::json!({
        "files": [
            { "filename": "foo-1.0.whl", "url": "https://files.pythonhosted.org/packages/xx/foo-1.0.whl#sha256=deadbeef" }
        ]
    });
    let body = serde_json::to_vec(&json).unwrap();
    let out = rewrite_simple_json(&body, "http://localhost/proxy/my-pypi");
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let url = parsed["files"][0]["url"].as_str().unwrap();
    assert_eq!(
        url,
        "http://localhost/proxy/my-pypi/packages/foo-1.0.whl#sha256=deadbeef"
    );
}

#[tokio::test]
async fn resolve_metadata_finds_wheel() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/pypi/requests/2.28.0/json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&serde_json::json!({
            "urls": [
                {
                    "filename": "requests-2.28.0-py3-none-any.whl",
                    "url": "https://files.pythonhosted.org/packages/requests-2.28.0-py3-none-any.whl",
                    "digests": { "sha256": "abc123" },
                    "upload_time_iso_8601": "2022-10-26T18:17:01.491020Z"
                }
            ]
        })).unwrap())
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = PypiRegistryClient::new(server.url(), &opts).unwrap();

    let pkg = PackageId::new("my-pypi", "requests", "2.28.0")
        .with_artifact("requests-2.28.0-py3-none-any.whl");
    let meta = client.resolve_metadata(&pkg).await.unwrap();

    assert_eq!(meta.checksum.as_deref(), Some("abc123"));
    assert!(meta.download_url.is_some());
    assert!(meta.published_at.is_some());
    mock.assert_async().await;
}

#[tokio::test]
async fn resolve_metadata_404_returns_not_found() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/pypi/nonexistent/1.0.0/json")
        .with_status(404)
        .create_async()
        .await;

    // A 404 from the JSON API sends the lookup to the simple page, which is
    // not there either.
    let _page = server
        .mock("GET", "/simple/nonexistent/")
        .with_status(404)
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = PypiRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("reg", "nonexistent", "1.0.0");
    let err = client.resolve_metadata(&pkg).await.unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn fetch_artifact_resolves_then_streams_file() {
    use futures::TryStreamExt;
    let mut server = mockito::Server::new_async().await;
    let file_url = format!("{}/files/requests-2.28.0-py3-none-any.whl", server.url());
    let meta = server
        .mock("GET", "/pypi/requests/2.28.0/json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::to_string(&serde_json::json!({
                "urls": [
                    {
                        "filename": "requests-2.28.0-py3-none-any.whl",
                        "url": file_url,
                        "digests": { "sha256": "abc" }
                    }
                ]
            }))
            .unwrap(),
        )
        .create_async()
        .await;
    let file = server
        .mock("GET", "/files/requests-2.28.0-py3-none-any.whl")
        .with_status(200)
        .with_body("WHEELBYTES")
        .create_async()
        .await;

    let client = PypiRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
    let pkg = PackageId::new("reg", "requests", "2.28.0")
        .with_artifact("requests-2.28.0-py3-none-any.whl");
    let fetched = client.fetch_artifact(&pkg).await.unwrap();
    let body: Vec<u8> = fetched
        .stream
        .try_fold(Vec::new(), |mut acc, c| async move {
            acc.extend_from_slice(&c);
            Ok(acc)
        })
        .await
        .unwrap();
    assert_eq!(body, b"WHEELBYTES");
    meta.assert_async().await;
    file.assert_async().await;
}

#[tokio::test]
async fn fetch_artifact_missing_file_is_not_found() {
    let mut server = mockito::Server::new_async().await;
    let _meta = server
        .mock("GET", "/pypi/requests/2.28.0/json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(serde_json::to_string(&serde_json::json!({ "urls": [] })).unwrap())
        .create_async()
        .await;
    let client = PypiRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
    let pkg = PackageId::new("reg", "requests", "2.28.0").with_artifact("nope.whl");
    match client.fetch_artifact(&pkg).await {
        Err(e) => assert!(matches!(e, CoreError::NotFound(_))),
        Ok(_) => panic!("expected NotFound for a missing file"),
    }
}

#[tokio::test]
async fn list_versions_parses_releases() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/pypi/requests/json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::to_string(&serde_json::json!({
                "releases": {
                    "2.27.0": [],
                    "2.28.0": [],
                    "2.28.1": []
                }
            }))
            .unwrap(),
        )
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = PypiRegistryClient::new(server.url(), &opts).unwrap();
    let versions = client.list_versions("requests").await.unwrap();
    assert_eq!(versions, vec!["2.27.0", "2.28.0", "2.28.1"]);
}

// ── fetch_simple_page ──────────────────────────────────────────────────────

#[tokio::test]
async fn fetch_simple_page_returns_body_and_content_type() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/simple/my-package/")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body("<html><body>index</body></html>")
        .create_async()
        .await;

    let client = reqwest::Client::new();
    let (body, content_type) = fetch_simple_page(&client, &server.url(), "My_Package", None, None)
        .await
        .unwrap();

    assert_eq!(&body[..], b"<html><body>index</body></html>");
    assert_eq!(content_type.as_deref(), Some("text/html"));
    mock.assert_async().await;
}

#[tokio::test]
async fn fetch_simple_page_404_returns_not_found() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/simple/nonexistent/")
        .with_status(404)
        .create_async()
        .await;

    let client = reqwest::Client::new();
    let err = fetch_simple_page(&client, &server.url(), "nonexistent", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)));
}

#[tokio::test]
async fn fetch_simple_page_500_returns_registry_error() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/simple/broken/")
        .with_status(500)
        .create_async()
        .await;

    let client = reqwest::Client::new();
    let err = fetch_simple_page(&client, &server.url(), "broken", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Registry(_)));
}

#[tokio::test]
async fn fetch_simple_page_sends_basic_auth_and_accept_headers() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/simple/private-pkg/")
        .match_header("authorization", mockito::Matcher::Regex("^Basic .*".into()))
        .match_header("accept", "application/vnd.pypi.simple.v1+json")
        .with_status(200)
        .with_header("content-type", "application/vnd.pypi.simple.v1+json")
        .with_body("{}")
        .create_async()
        .await;

    let client = reqwest::Client::new();
    let auth = ("user".to_owned(), "pass".to_owned());
    let (_body, content_type) = fetch_simple_page(
        &client,
        &server.url(),
        "private-pkg",
        Some(&auth),
        Some("application/vnd.pypi.simple.v1+json"),
    )
    .await
    .unwrap();

    assert_eq!(
        content_type.as_deref(),
        Some("application/vnd.pypi.simple.v1+json")
    );
    mock.assert_async().await;
}

// ── rewrite_file_url (via rewrite_simple_json) ────────────────────────────

#[test]
fn rewrite_simple_json_leaves_slashless_url_unchanged() {
    let json = serde_json::json!({
        "files": [
            { "filename": "foo", "url": "no-slash-url" }
        ]
    });
    let body = serde_json::to_vec(&json).unwrap();
    let out = rewrite_simple_page(
        &body,
        Some("application/vnd.pypi.simple.v1+json"),
        "http://localhost/proxy/my-pypi",
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed["files"][0]["url"].as_str().unwrap(), "no-slash-url");
}

// ── README capture (RFC 0007 §2.1) ────────────────────────────────────────

/// `info.description` is the long description and `info.description_content_type`
/// names its markup. Both are per-version by construction — a wheel's `METADATA`
/// ships inside the wheel — so this is this version's own account of itself.
#[tokio::test]
async fn the_long_description_reaches_the_extra_channel_with_its_declared_markup() {
    for (declared, expected) in [
        (
            "text/markdown",
            batlehub_core::entities::ReadmeFormat::Markdown,
        ),
        ("text/x-rst", batlehub_core::entities::ReadmeFormat::Rst),
        ("text/plain", batlehub_core::entities::ReadmeFormat::Plain),
        // A parameterised type is still that type.
        (
            "text/markdown; charset=UTF-8; variant=GFM",
            batlehub_core::entities::ReadmeFormat::Markdown,
        ),
    ] {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/pypi/requests/2.28.0/json")
            .with_status(200)
            .with_body(
                serde_json::to_string(&serde_json::json!({
                    "info": {
                        "description": "Requests is an elegant HTTP library.",
                        "description_content_type": declared,
                    },
                    "urls": [{
                        "filename": "requests-2.28.0-py3-none-any.whl",
                        "url": "https://files.pythonhosted.org/packages/requests-2.28.0.whl",
                        "digests": { "sha256": "abc123" }
                    }]
                }))
                .unwrap(),
            )
            .create_async()
            .await;

        let client =
            PypiRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let meta = client
            .resolve_metadata(&PackageId::new("my-pypi", "requests", "2.28.0"))
            .await
            .unwrap();

        let found = batlehub_core::entities::MetadataReadme::from_extra(&meta.extra)
            .expect("description captured");
        assert_eq!(
            found.content.as_deref(),
            Some("Requests is an elegant HTTP library.")
        );
        assert_eq!(found.format, expected, "declared {declared}");
        assert!(!found.package_level);
    }
}

/// PEP 566 says an absent `Description-Content-Type` means plain text. It must
/// not be guessed into a renderer.
#[tokio::test]
async fn an_undeclared_content_type_is_plain_not_markdown() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/pypi/requests/2.28.0/json")
        .with_status(200)
        .with_body(
            serde_json::to_string(&serde_json::json!({
                "info": { "description": "plain prose" },
                "urls": []
            }))
            .unwrap(),
        )
        .create_async()
        .await;

    let client = PypiRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
    let meta = client
        .resolve_metadata(&PackageId::new("my-pypi", "requests", "2.28.0"))
        .await
        .unwrap();
    assert_eq!(
        batlehub_core::entities::MetadataReadme::from_extra(&meta.extra)
            .unwrap()
            .format,
        batlehub_core::entities::ReadmeFormat::Plain
    );
}

/// An absent or empty `description` captures nothing: an empty panel and a
/// panel showing whitespace look identical to a reader, and only one of them is
/// honest about there being no README.
#[tokio::test]
async fn an_empty_or_absent_description_captures_nothing() {
    for info in [
        serde_json::json!({}),
        serde_json::json!({ "description": "" }),
        serde_json::json!({ "description": "   \n  " }),
        serde_json::json!({ "description": null }),
    ] {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("GET", "/pypi/requests/2.28.0/json")
            .with_status(200)
            .with_body(
                serde_json::to_string(&serde_json::json!({ "info": info, "urls": [] })).unwrap(),
            )
            .create_async()
            .await;

        let client =
            PypiRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let meta = client
            .resolve_metadata(&PackageId::new("my-pypi", "requests", "2.28.0"))
            .await
            .unwrap();
        assert_eq!(
            batlehub_core::entities::MetadataReadme::from_extra(&meta.extra),
            None
        );
    }
}

// ── fetch_version_document: PEP 691 asked, PEP 503 answered ─────────────────

/// An index that only speaks PEP 503 answers `text/html` to a JSON `Accept`.
/// pip lists HTML in its own `Accept` for exactly that case, so the page is
/// served as HTML — not a `502` for a body that is not JSON, which is what
/// `tests/heavy/backends.sh` measured against a served directory.
#[tokio::test]
async fn simple_json_kind_falls_back_to_html_when_the_upstream_only_speaks_html() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/simple/only-html/")
        .match_header(
            "accept",
            mockito::Matcher::Regex("application/vnd\\.pypi\\.simple\\.v1\\+json.*text/html".into()),
        )
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body("<html><body><a href=\"only_html-1.0.0.tar.gz\">only_html-1.0.0.tar.gz</a></body></html>")
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = PypiRegistryClient::new(server.url(), &opts).unwrap();
    let doc = client
        .fetch_version_document("only-html", DocumentKind::SIMPLE_JSON)
        .await
        .unwrap();

    assert!(
        doc.content_type.starts_with("text/html"),
        "{}",
        doc.content_type
    );
    assert!(
        doc.body
            .as_text()
            .is_some_and(|t| t.contains("only_html-1.0.0.tar.gz")),
        "the HTML page is served as text: {:?}",
        doc.body
    );
    mock.assert_async().await;
}

#[tokio::test]
async fn simple_json_kind_is_json_when_the_upstream_answers_json() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/simple/speaks-json/")
        .with_status(200)
        .with_header("content-type", "application/vnd.pypi.simple.v1+json")
        .with_body(r#"{"name":"speaks-json","files":[]}"#)
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = PypiRegistryClient::new(server.url(), &opts).unwrap();
    let doc = client
        .fetch_version_document("speaks-json", DocumentKind::SIMPLE_JSON)
        .await
        .unwrap();

    assert_eq!(doc.content_type, SIMPLE_JSON_ACCEPT);
    assert!(doc.body.as_json().is_some());
}

// ── fetch_artifact: a PEP 503-only upstream ─────────────────────────────────

#[test]
fn files_in_simple_page_resolves_relative_html_hrefs_by_filename() {
    let html = b"<html><body>\n<a href=\"../../packages/x_y-1.0.0-py3-none-any.whl#sha256=abc\">x_y-1.0.0-py3-none-any.whl</a><br/>\n<a href=\"http://cdn.example/other-2.0.tar.gz\">other-2.0.tar.gz</a></body></html>";
    let files =
        client::files_in_simple_page("http://idx.example/simple/x-y/", html, Some("text/html"));
    assert_eq!(
        files,
        vec![
            (
                "x_y-1.0.0-py3-none-any.whl".to_owned(),
                "http://idx.example/packages/x_y-1.0.0-py3-none-any.whl#sha256=abc".to_owned()
            ),
            (
                "other-2.0.tar.gz".to_owned(),
                "http://cdn.example/other-2.0.tar.gz".to_owned()
            ),
        ]
    );
}

#[test]
fn files_in_simple_page_reads_a_pep691_document() {
    let json = br#"{"name":"x-y","files":[{"filename":"x_y-1.0.0-py3-none-any.whl","url":"/files/x_y-1.0.0-py3-none-any.whl"}]}"#;
    assert_eq!(
        client::files_in_simple_page(
            "http://idx.example/simple/x-y/",
            json,
            Some("application/vnd.pypi.simple.v1+json")
        ),
        vec![(
            "x_y-1.0.0-py3-none-any.whl".to_owned(),
            "http://idx.example/files/x_y-1.0.0-py3-none-any.whl".to_owned()
        )]
    );
}

/// No `/pypi/{name}/{version}/json` upstream — a static mirror, devpi, a
/// Nexus — so the file is found on the simple page, which is what pip would
/// have read. Measured by tests/heavy/backends.sh: the page came through and
/// the wheel was a `404`.
#[tokio::test]
async fn fetch_artifact_falls_back_to_the_simple_page_when_there_is_no_json_api() {
    let mut server = mockito::Server::new_async().await;
    let _api = server
        .mock("GET", "/pypi/only-html/1.0.0/json")
        .with_status(404)
        .create_async()
        .await;
    let page = server
        .mock("GET", "/simple/only-html/")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body("<html><body><a href=\"../../files/only_html-1.0.0-py3-none-any.whl#sha256=ab\">only_html-1.0.0-py3-none-any.whl</a></body></html>")
        .create_async()
        .await;
    let file = server
        .mock("GET", "/files/only_html-1.0.0-py3-none-any.whl")
        .with_status(200)
        .with_body("WHEEL-BYTES")
        .create_async()
        .await;

    let opts = UpstreamHttpOptions::default();
    let client = PypiRegistryClient::new(server.url(), &opts).unwrap();
    let pkg = PackageId::new("pypi", "only-html", "1.0.0")
        .with_artifact("only_html-1.0.0-py3-none-any.whl");
    let fetched = client.fetch_artifact(&pkg).await.unwrap();
    let bytes: Vec<u8> = fetched
        .stream
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .unwrap();
    assert_eq!(bytes, b"WHEEL-BYTES");
    page.assert_async().await;
    file.assert_async().await;
}

/// The same upstream, asked for metadata: what the page says — the URL and
/// the sha256 in the link — and no more.
#[tokio::test]
async fn resolve_metadata_falls_back_to_the_simple_page_when_there_is_no_json_api() {
    let mut server = mockito::Server::new_async().await;
    let _api = server
        .mock("GET", "/pypi/only-html/1.0.0/json")
        .with_status(404)
        .create_async()
        .await;
    let _page = server
        .mock("GET", "/simple/only-html/")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(format!(
            "<html><body><a href=\"../../files/only_html-1.0.0-py3-none-any.whl#sha256={}\">only_html-1.0.0-py3-none-any.whl</a></body></html>",
            "a".repeat(64)
        ))
        .create_async()
        .await;

    let client = PypiRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
    let by_file = PackageId::new("pypi", "only-html", "1.0.0")
        .with_artifact("only_html-1.0.0-py3-none-any.whl");
    let meta = client.resolve_metadata(&by_file).await.unwrap();
    assert_eq!(
        meta.download_url.as_deref(),
        Some(
            format!(
                "{}/files/only_html-1.0.0-py3-none-any.whl#sha256={}",
                server.url(),
                "a".repeat(64)
            )
            .as_str()
        )
    );
    assert_eq!(meta.checksum.as_deref(), Some("a".repeat(64).as_str()));
    assert!(meta.published_at.is_none());

    // No file named: any file of this version will do.
    let by_version = PackageId::new("pypi", "only-html", "1.0.0");
    assert!(client.resolve_metadata(&by_version).await.is_ok());
    // A version the page does not carry is not found.
    let _api_absent = server
        .mock("GET", "/pypi/only-html/9.9.9/json")
        .with_status(404)
        .create_async()
        .await;
    let absent = PackageId::new("pypi", "only-html", "9.9.9");
    assert!(matches!(
        client.resolve_metadata(&absent).await,
        Err(CoreError::NotFound(_))
    ));
}
