//! The galaxy client against a mock upstream (RFC 0031 §10).
//!
//! Four things a fixture cannot prove and only a real HTTP exchange can:
//! discovery is *probed* rather than assumed, upstream's pages are assembled
//! into one document, a pagination link that leaves the origin is refused, and
//! a `download_url` on a foreign origin is refused before the request is made.

use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    ports::{DocumentKind, RegistryClient},
};
use serde_json::json;

use super::client::GalaxyRegistryClient;
use crate::registry::http_client::UpstreamHttpOptions;

/// A well-formed sha256, because `integrity::parse_expected` infers the
/// algorithm from the digest's *length* — a short stand-in would be dropped and
/// the assertion would prove nothing.
const REAL_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn client(base: &str) -> GalaxyRegistryClient {
    GalaxyRegistryClient::new(base, &UpstreamHttpOptions::default()).expect("client")
}

fn discovery_body() -> String {
    json!({ "description": "mock", "available_versions": { "v1": "v1/", "v3": "v3/" } }).to_string()
}

fn page(versions: &[&str], next: Option<&str>) -> String {
    json!({
        "meta": { "count": 241 },
        "links": { "first": null, "previous": null, "next": next, "last": null },
        "data": versions.iter().map(|v| json!({ "version": v, "created_at": "2026-01-01T00:00:00Z" })).collect::<Vec<_>>(),
    })
    .to_string()
}

#[tokio::test]
async fn discovery_is_retried_with_api_appended() {
    let mut server = mockito::Server::new_async().await;
    // The operator wrote the host with no `/api/`, exactly as `g_connect`
    // tolerates: the first probe answers something that is not a discovery
    // document, and the second one is the real root.
    let root = server
        .mock("GET", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({ "hello": "world" }).to_string())
        .create_async()
        .await;
    let api = server
        .mock("GET", "/api/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .create_async()
        .await;
    let versions = server
        .mock("GET", "/api/v3/collections/acme/util/versions/?limit=100")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(page(&["1.0.0"], None))
        .create_async()
        .await;

    let c = client(&server.url());
    let doc = c
        .fetch_version_document("acme.util", DocumentKind::Versions)
        .await
        .expect("the listing resolves through the retried root");
    assert_eq!(doc.body.as_json().unwrap()["data"][0]["version"], "1.0.0");
    root.assert_async().await;
    api.assert_async().await;
    versions.assert_async().await;
}

#[tokio::test]
async fn three_upstream_pages_become_one_served_document() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_body(discovery_body())
        .with_header("content-type", "application/json")
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/?limit=100")
        .with_header("content-type", "application/json")
        .with_body(page(
            &["1.0.0"],
            Some("/api/v3/collections/acme/util/versions/?limit=100&offset=1"),
        ))
        .create_async()
        .await;
    server
        .mock(
            "GET",
            "/api/v3/collections/acme/util/versions/?limit=100&offset=1",
        )
        .with_header("content-type", "application/json")
        .with_body(page(
            &["1.1.0"],
            Some("/api/v3/collections/acme/util/versions/?limit=100&offset=2"),
        ))
        .create_async()
        .await;
    server
        .mock(
            "GET",
            "/api/v3/collections/acme/util/versions/?limit=100&offset=2",
        )
        .with_header("content-type", "application/json")
        .with_body(page(&["1.2.0"], None))
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let doc = c
        .fetch_version_document("acme.util", DocumentKind::Versions)
        .await
        .expect("the walk assembles");
    let body = doc.body.as_json().unwrap();
    let served: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["version"].as_str().unwrap())
        .collect();
    assert_eq!(served, ["1.0.0", "1.1.0", "1.2.0"]);
    // The assembled document describes itself, and carries no continuation.
    assert_eq!(body["meta"]["count"], 3);
    assert!(body["links"]["next"].is_null());
}

#[tokio::test]
async fn a_cross_origin_pagination_link_is_refused() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/?limit=100")
        .with_header("content-type", "application/json")
        .with_body(page(
            &["1.0.0"],
            Some("http://169.254.169.254/latest/meta-data/"),
        ))
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let err = c
        .fetch_version_document("acme.util", DocumentKind::Versions)
        .await
        .expect_err("a link off the origin is an SSRF, not a page two");
    assert!(
        matches!(&err, CoreError::Registry(m) if m.contains("cross-origin pagination link")),
        "{err}"
    );
}

#[tokio::test]
async fn a_download_url_on_a_foreign_origin_is_refused() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/1.0.0/")
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "version": "1.0.0",
                "created_at": "2026-01-01T00:00:00Z",
                "artifact": { "sha256": "00", "size": 1 },
                "download_url": "http://169.254.169.254/latest/meta-data/",
            })
            .to_string(),
        )
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    let Err(err) = c.fetch_artifact(&pkg).await else {
        panic!("the document is upstream's, and a URL in it can point anywhere");
    };
    assert!(
        matches!(&err, CoreError::Registry(m) if m.contains("not an origin this registry serves")),
        "{err}"
    );
}

#[tokio::test]
async fn the_version_document_supplies_published_at_and_the_digest() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/1.0.0/")
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "version": "1.0.0",
                "created_at": "2026-01-02T03:04:05Z",
                "requires_ansible": ">=2.15.0",
                "artifact": { "filename": "acme-util-1.0.0.tar.gz", "sha256": REAL_SHA256, "size": 7 },
                "download_url": format!("{}/api/v3/artifacts/collections/acme-util-1.0.0.tar.gz", server.url()),
            })
            .to_string(),
        )
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    let meta = c.resolve_metadata(&pkg).await.expect("resolves");
    assert_eq!(
        meta.published_at.map(|d| d.to_rfc3339()),
        Some("2026-01-02T03:04:05+00:00".to_owned()),
        "created_at is an RFC 3339 instant, so the age gate needs no midnight rule"
    );
    // Bare hex, not `sha256:<hex>`: `integrity::parse_expected` accepts an SRI
    // token or a bare hex digest and nothing else, so a prefixed value makes
    // the cache-write verification skip itself with only a log line to say so.
    assert_eq!(meta.checksum.as_deref(), Some(REAL_SHA256));
    assert_eq!(meta.extra["requires_ansible"], ">=2.15.0");
}

#[tokio::test]
async fn an_unknown_collection_is_not_found_rather_than_an_error() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/9.9.9/")
        .with_status(404)
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "9.9.9").with_artifact("tarball");
    let err = c.resolve_metadata(&pkg).await.expect_err("404");
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn an_upstream_with_no_v1_refuses_the_role_surface_by_name() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(json!({ "available_versions": { "v3": "v3/" } }).to_string())
        .expect_at_least(1)
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let err = c
        .role_search("geerlingguy", "docker")
        .await
        .expect_err("no v1 upstream");
    assert!(
        matches!(&err, CoreError::NotSupported(m) if m.contains("advertises no 'v1' API")),
        "{err}"
    );
}

#[tokio::test]
async fn a_role_id_resolves_to_the_name_a_block_is_written_on() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v1/roles/4567/")
        .with_header("content-type", "application/json")
        .with_body(
            json!({ "id": 4567, "github_user": "geerlingguy", "name": "docker" }).to_string(),
        )
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let alias = PackageId::new("g", "roles/#4567", "");
    let resolved = c
        .canonical_coordinate(&alias)
        .await
        .expect("resolves")
        .expect("an alias resolves to a name");
    assert_eq!(resolved.name, "roles/geerlingguy.docker");
}

#[tokio::test]
async fn a_collection_coordinate_is_already_canonical() {
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "1.0.0");
    assert!(
        c.canonical_coordinate(&pkg).await.unwrap().is_none(),
        "the common case costs no I/O at all"
    );
}

#[tokio::test]
async fn a_publish_tarball_yields_its_two_documents() {
    use std::io::Write;
    let manifest = json!({
        "collection_info": { "namespace": "acme", "name": "util", "version": "1.0.0" },
        "format": 1,
    });
    let mut tar = tar::Builder::new(Vec::new());
    for (path, body) in [
        ("MANIFEST.json", manifest.to_string()),
        ("FILES.json", json!({ "files": [] }).to_string()),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_path(path).unwrap();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append(&h, body.as_bytes()).unwrap();
    }
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&raw).unwrap();
    let bytes = gz.finish().unwrap();

    let read = super::publish::read_collection_tarball(&bytes).expect("reads");
    assert_eq!(read.manifest.package().unwrap(), "acme.util");
    assert_eq!(read.manifest.collection_info.version, "1.0.0");
    assert!(
        read.files_json.is_some(),
        "FILES.json travels as it arrived"
    );
}

#[tokio::test]
async fn a_tarball_with_no_manifest_is_refused() {
    use std::io::Write;
    let mut tar = tar::Builder::new(Vec::new());
    let body = b"nothing";
    let mut h = tar::Header::new_gnu();
    h.set_path("README.md").unwrap();
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    tar.append(&h, &body[..]).unwrap();
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&raw).unwrap();
    let bytes = gz.finish().unwrap();

    let err = super::publish::read_collection_tarball(&bytes).expect_err("no MANIFEST.json");
    assert!(matches!(err, CoreError::InvalidInput(_)), "{err}");
}

#[tokio::test]
async fn an_absolute_path_download_url_resolves_against_the_upstream() {
    // galaxy_ng behind a reverse proxy emits the path form, and
    // `_download_file` accepts it — so an origin check written against a bare
    // `Url::parse` would refuse every self-hosted upstream.
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/1.0.0/")
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "version": "1.0.0",
                "artifact": { "sha256": "abc", "size": 3 },
                "download_url": "/api/v3/plugin/ansible/content/published/collections/artifacts/acme-util-1.0.0.tar.gz",
            })
            .to_string(),
        )
        .create_async()
        .await;
    let bytes = server
        .mock(
            "GET",
            "/api/v3/plugin/ansible/content/published/collections/artifacts/acme-util-1.0.0.tar.gz",
        )
        .with_body("gz!")
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    c.fetch_artifact(&pkg)
        .await
        .map(|_| ())
        .expect("an absolute-path download_url is followed");
    bytes.assert_async().await;
}

#[tokio::test]
async fn a_download_url_that_is_neither_absolute_nor_a_path_is_refused() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/1.0.0/")
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "version": "1.0.0",
                "artifact": { "sha256": "abc" },
                "download_url": "artifacts/acme-util-1.0.0.tar.gz",
            })
            .to_string(),
        )
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    let Err(err) = c.fetch_artifact(&pkg).await else {
        panic!("the client itself refuses this with 'Invalid non absolute download_url'");
    };
    assert!(
        matches!(&err, CoreError::Registry(m) if m.contains("neither absolute nor absolute-path")),
        "{err}"
    );
}

/// The digest the version document advertises must reach the cache's verifier
/// in a shape it can read.
///
/// `integrity::parse_expected` accepts an SRI `<algo>-<base64>` token or a bare
/// hex digest whose algorithm it infers from the length. A `sha256:<hex>` value
/// parses as neither, and the failure is **silent**: the artifact is served, the
/// client's own check passes, and only a `WARN` says the server verified
/// nothing. That is what shipped until `tests/heavy/galaxy.sh` ran.
#[tokio::test]
async fn the_advertised_digest_is_in_a_shape_the_verifier_can_read() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/1.0.0/")
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "version": "1.0.0",
                "artifact": { "sha256": REAL_SHA256, "size": 1 },
                "download_url": format!("{}/api/x.tar.gz", server.url()),
            })
            .to_string(),
        )
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    let meta = c.resolve_metadata(&pkg).await.expect("resolves");
    let advertised = meta.checksum.expect("a digest is advertised");
    assert!(
        batlehub_core::services::integrity::parse_expected(&advertised).is_some(),
        "the cache would skip verification for {advertised:?}"
    );
}

/// An upstream digest this instance cannot vouch for is dropped rather than
/// passed on: an unparseable value reaches the verifier as "skipping", which
/// reads in the log like a defect in this server rather than in the document.
#[tokio::test]
async fn a_malformed_upstream_digest_is_dropped_rather_than_relayed() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/1.0.0/")
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "version": "1.0.0",
                "artifact": { "sha256": "not-a-digest" },
                "download_url": format!("{}/api/x.tar.gz", server.url()),
            })
            .to_string(),
        )
        .create_async()
        .await;

    let c = client(&format!("{}/api", server.url()));
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    let meta = c.resolve_metadata(&pkg).await.expect("resolves");
    assert!(meta.checksum.is_none());
    // The document's own value still travels for anything that wants to relay
    // it — only the *verifier's* field is cleared.
    assert_eq!(meta.extra["sha256"], "not-a-digest");
}

// --- RFC 0041 §6.6: pagination, the role surface and upstream errors -------

/// A mock upstream whose `/api/` answers the discovery document, and a client
/// rooted at it.
async fn upstream() -> (mockito::ServerGuard, GalaxyRegistryClient) {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/api/")
        .with_header("content-type", "application/json")
        .with_body(discovery_body())
        .expect_at_least(1)
        .create_async()
        .await;
    let c = client(&format!("{}/api", server.url()));
    (server, c)
}

fn json_mock(
    server: &mut mockito::ServerGuard,
    path: &str,
    body: serde_json::Value,
) -> mockito::Mock {
    server
        .mock("GET", path)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
}

const VERSIONS: &str = "/api/v3/collections/acme/util/versions/?limit=100";
const ROLE_SEARCH: &str = "/api/v1/roles/?owner__username=geerlingguy&name=docker";
const ROLE_VERSIONS: &str = "/api/v1/roles/4567/versions/?page_size=100";

#[tokio::test]
async fn list_versions_walks_every_page_and_answers_oldest_first() {
    let (mut server, c) = upstream().await;
    let next = format!("{VERSIONS}&offset=2");
    server
        .mock("GET", VERSIONS)
        .with_header("content-type", "application/json")
        .with_body(page(&["2.0.0", "1.10.0"], Some(&next)))
        .create_async()
        .await;
    server
        .mock("GET", next.as_str())
        .with_header("content-type", "application/json")
        .with_body(page(&["1.2.0"], None))
        .create_async()
        .await;

    let versions = c.list_versions("acme.util").await.expect("lists");
    assert_eq!(versions, ["1.2.0", "1.10.0", "2.0.0"]);
}

#[tokio::test]
async fn list_versions_of_a_role_is_empty_without_any_request() {
    // No mock at all: a request would 501 and the call would fail.
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url()));
    assert!(c
        .list_versions("roles/geerlingguy.docker")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn an_upstream_error_on_a_later_page_fails_the_whole_listing() {
    let (mut server, c) = upstream().await;
    let next = format!("{VERSIONS}&offset=1");
    server
        .mock("GET", VERSIONS)
        .with_header("content-type", "application/json")
        .with_body(page(&["1.0.0"], Some(&next)))
        .create_async()
        .await;
    server
        .mock("GET", next.as_str())
        .with_status(503)
        .create_async()
        .await;

    // A truncated listing served as complete would hide versions; an error does not.
    let err = c
        .list_versions("acme.util")
        .await
        .expect_err("503 on page two");
    assert!(matches!(err, CoreError::Registry(_)), "{err}");
}

#[tokio::test]
async fn an_unknown_collection_listing_is_not_found() {
    let (mut server, c) = upstream().await;
    server
        .mock("GET", VERSIONS)
        .with_status(404)
        .create_async()
        .await;
    let err = c.list_versions("acme.util").await.expect_err("404");
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn a_listing_page_that_is_not_json_is_a_registry_error() {
    let (mut server, c) = upstream().await;
    server
        .mock("GET", VERSIONS)
        .with_body("<html>maintenance</html>")
        .create_async()
        .await;
    let err = c.list_versions("acme.util").await.expect_err("not JSON");
    assert!(
        matches!(&err, CoreError::Registry(m) if m.contains("parsing listing page")),
        "{err}"
    );
}

#[tokio::test]
async fn an_upstream_with_no_discovery_document_is_an_error_naming_the_last_probe() {
    let mut server = mockito::Server::new_async().await;
    // Both probes — the configured root and it with `/api` appended — fail.
    let probes = server
        .mock("GET", mockito::Matcher::Any)
        .with_status(500)
        .expect(2)
        .create_async()
        .await;
    let c = client(&server.url());
    let err = c
        .list_versions("acme.util")
        .await
        .expect_err("no discovery");
    assert!(matches!(err, CoreError::Registry(_)), "{err}");
    probes.assert_async().await;
}

#[tokio::test]
async fn a_root_that_is_not_a_discovery_document_is_refused_by_name() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", mockito::Matcher::Any)
        .with_header("content-type", "application/json")
        .with_body(json!({ "hello": "world" }).to_string())
        .create_async()
        .await;
    let c = client(&server.url());
    let err = c
        .list_versions("acme.util")
        .await
        .expect_err("not discovery");
    assert!(
        matches!(&err, CoreError::Registry(m) if m.contains("is not a galaxy discovery document")),
        "{err}"
    );
}

#[tokio::test]
async fn discovery_is_probed_once_and_then_answered_from_cache() {
    let mut server = mockito::Server::new_async().await;
    let discovery = json_mock(
        &mut server,
        "/api/",
        serde_json::from_str(&discovery_body()).unwrap(),
    )
    .expect(1)
    .create_async()
    .await;
    json_mock(
        &mut server,
        VERSIONS,
        serde_json::from_str(&page(&["1.0.0"], None)).unwrap(),
    )
    .expect(2)
    .create_async()
    .await;
    let c = client(&format!("{}/api", server.url()));
    c.list_versions("acme.util").await.expect("first");
    c.list_versions("acme.util").await.expect("second");
    discovery.assert_async().await;
}

#[tokio::test]
async fn the_collection_document_is_relayed_as_served() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        "/api/v3/collections/acme/util/",
        json!({ "namespace": "acme", "name": "util", "updated_at": "2026-02-02T00:00:00Z" }),
    )
    .create_async()
    .await;
    let doc = c
        .fetch_version_document("acme.util", DocumentKind::COLLECTION)
        .await
        .expect("relays");
    assert_eq!(
        doc.body.as_json().unwrap()["updated_at"],
        "2026-02-02T00:00:00Z"
    );
}

#[tokio::test]
async fn the_version_detail_document_is_addressed_by_its_version() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        "/api/v3/collections/acme/util/versions/1.0.0/",
        json!({ "version": "1.0.0", "download_url": "x" }),
    )
    .create_async()
    .await;
    let doc = c
        .fetch_version_document("acme.util@1.0.0", DocumentKind::VERSION_DETAIL)
        .await
        .expect("relays");
    assert_eq!(doc.body.as_json().unwrap()["version"], "1.0.0");
}

#[tokio::test]
async fn a_version_detail_request_with_no_version_is_not_supported() {
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url()));
    let err = c
        .fetch_version_document("acme.util", DocumentKind::VERSION_DETAIL)
        .await
        .expect_err("no @version");
    assert!(matches!(err, CoreError::NotSupported(_)), "{err}");
}

#[tokio::test]
async fn a_role_versions_listing_is_assembled_from_v1_pages() {
    let (mut server, c) = upstream().await;
    // v1 spells its continuation `next_link` and its entries `results`.
    let next = "/api/v1/roles/4567/versions/?page=2&page_size=100";
    json_mock(
        &mut server,
        ROLE_VERSIONS,
        json!({ "count": 2, "next_link": next, "results": [{ "name": "1.0.0" }] }),
    )
    .create_async()
    .await;
    json_mock(
        &mut server,
        next,
        json!({ "count": 2, "next_link": null, "results": [{ "name": "2.0.0" }] }),
    )
    .create_async()
    .await;

    let doc = c
        .fetch_version_document("roles/geerlingguy.docker@4567", DocumentKind::ROLE_VERSIONS)
        .await
        .expect("assembles");
    let names: Vec<&str> = doc.body.as_json().unwrap()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["1.0.0", "2.0.0"]);
}

#[tokio::test]
async fn a_role_versions_request_with_no_id_is_not_supported() {
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url()));
    let err = c
        .fetch_version_document("roles/geerlingguy.docker", DocumentKind::ROLE_VERSIONS)
        .await
        .expect_err("no @id");
    assert!(matches!(err, CoreError::NotSupported(_)), "{err}");
}

#[tokio::test]
async fn the_role_document_is_the_v1_search() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        ROLE_SEARCH,
        json!({ "count": 1, "results": [{ "id": 4567 }] }),
    )
    .create_async()
    .await;
    let doc = c
        .fetch_version_document("roles/geerlingguy.docker", DocumentKind::ROLE)
        .await
        .expect("search");
    assert_eq!(doc.body.as_json().unwrap()["results"][0]["id"], 4567);
}

#[tokio::test]
async fn a_role_name_with_no_dot_is_not_supported() {
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url()));
    let err = c
        .fetch_version_document("roles/nodot", DocumentKind::ROLE)
        .await
        .expect_err("not user.role");
    assert!(matches!(err, CoreError::NotSupported(_)), "{err}");
}

#[tokio::test]
async fn a_document_kind_galaxy_does_not_have_is_not_supported() {
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url()));
    let err = c
        .fetch_version_document("acme.util", DocumentKind::MANIFEST)
        .await
        .expect_err("rustup's kind");
    assert!(
        matches!(&err, CoreError::NotSupported(m) if m.contains("no 'manifest' listing")),
        "{err}"
    );
}

#[tokio::test]
async fn roles_off_refuses_the_v1_surface_without_asking_upstream() {
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url())).with_roles(false, false);
    let err = c
        .role_search("geerlingguy", "docker")
        .await
        .expect_err("roles = off");
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn an_unknown_role_id_is_not_found() {
    let (mut server, c) = upstream().await;
    server
        .mock("GET", "/api/v1/roles/999/")
        .with_status(404)
        .create_async()
        .await;
    let err = c.role_name_for_id("999").await.expect_err("404");
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn a_role_id_answered_as_a_one_entry_search_still_resolves() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        "/api/v1/roles/4567/",
        json!({ "results": [{ "id": 4567, "github_user": "geerlingguy", "name": "docker" }] }),
    )
    .create_async()
    .await;
    assert_eq!(
        c.role_name_for_id("4567").await.expect("resolves"),
        "roles/geerlingguy.docker"
    );
}

#[tokio::test]
async fn a_role_id_whose_search_is_empty_names_nothing() {
    let (mut server, c) = upstream().await;
    json_mock(&mut server, "/api/v1/roles/4567/", json!({ "results": [] }))
        .create_async()
        .await;
    let err = c.role_name_for_id("4567").await.expect_err("empty");
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn a_role_with_no_github_user_cannot_be_named() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        "/api/v1/roles/4567/",
        json!({ "id": 4567, "name": "docker" }),
    )
    .create_async()
    .await;
    let err = c.role_name_for_id("4567").await.expect_err("no user");
    assert!(
        matches!(&err, CoreError::Registry(m) if m.contains("no github_user/name pair")),
        "{err}"
    );
}

#[tokio::test]
async fn a_role_version_resolves_to_its_listing_entry_download_url() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        ROLE_SEARCH,
        json!({ "results": [{ "id": 4567 }] }),
    )
    .create_async()
    .await;
    json_mock(
        &mut server,
        ROLE_VERSIONS,
        json!({ "results": [
            { "name": "6.0.0", "download_url": "https://github.com/geerlingguy/ansible-role-docker/archive/6.0.0.tar.gz" },
            { "name": "7.0.0", "release_date": "2026-03-01T00:00:00Z",
              "download_url": "https://github.com/geerlingguy/ansible-role-docker/archive/7.0.0.tar.gz" },
        ] }),
    )
    .create_async()
    .await;

    let pkg = PackageId::new("g", "roles/geerlingguy.docker", "7.0.0");
    let meta = c.resolve_metadata(&pkg).await.expect("resolves");
    assert_eq!(
        meta.download_url.as_deref(),
        Some("https://github.com/geerlingguy/ansible-role-docker/archive/7.0.0.tar.gz")
    );
}

#[tokio::test]
async fn a_role_version_absent_from_the_listing_is_not_found() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        ROLE_SEARCH,
        json!({ "results": [{ "id": "4567" }] }),
    )
    .create_async()
    .await;
    json_mock(
        &mut server,
        ROLE_VERSIONS,
        json!({ "results": [{ "name": "6.0.0" }] }),
    )
    .create_async()
    .await;
    let pkg = PackageId::new("g", "roles/geerlingguy.docker", "9.9.9");
    let err = c.resolve_metadata(&pkg).await.expect_err("no 9.9.9");
    assert!(
        matches!(&err, CoreError::NotFound(m) if m.contains("has no version 9.9.9")),
        "{err}"
    );
}

#[tokio::test]
async fn a_role_the_search_does_not_find_is_not_found() {
    let (mut server, c) = upstream().await;
    json_mock(&mut server, ROLE_SEARCH, json!({ "results": [] }))
        .create_async()
        .await;
    let pkg = PackageId::new("g", "roles/geerlingguy.docker", "7.0.0");
    let err = c.resolve_metadata(&pkg).await.expect_err("no role");
    assert!(matches!(err, CoreError::NotFound(_)), "{err}");
}

#[tokio::test]
async fn a_role_with_a_non_scalar_id_is_refused() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        ROLE_SEARCH,
        json!({ "results": [{ "id": [1] }] }),
    )
    .create_async()
    .await;
    let pkg = PackageId::new("g", "roles/geerlingguy.docker", "7.0.0");
    let err = c.resolve_metadata(&pkg).await.expect_err("array id");
    assert!(
        matches!(&err, CoreError::Registry(m) if m.contains("non-scalar id")),
        "{err}"
    );
}

#[tokio::test]
async fn roles_index_mode_refuses_role_bytes() {
    let server = mockito::Server::new_async().await;
    let c = client(&format!("{}/api", server.url())).with_roles(true, false);
    let pkg = PackageId::new("g", "roles/geerlingguy.docker", "7.0.0");
    let meta = batlehub_core::entities::PackageMetadata {
        id: pkg.clone(),
        published_at: None,
        download_url: Some("https://github.com/x/y/archive/7.0.0.tar.gz".to_owned()),
        checksum: None,
        is_signed: None,
        extra: json!({}),
        cache_control: None,
    };
    let Err(err) = c.fetch_artifact_resolved(&pkg, &meta).await else {
        panic!("roles = index serves metadata only");
    };
    assert!(matches!(err, CoreError::NotSupported(_)), "{err}");
}

#[tokio::test]
async fn a_version_with_no_download_url_has_nothing_to_fetch() {
    let (mut server, c) = upstream().await;
    json_mock(
        &mut server,
        "/api/v3/collections/acme/util/versions/1.0.0/",
        json!({ "version": "1.0.0" }),
    )
    .create_async()
    .await;
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    let Err(err) = c.fetch_artifact(&pkg).await else {
        panic!("no download_url");
    };
    assert!(
        matches!(&err, CoreError::NotFound(m) if m.contains("carries no download_url")),
        "{err}"
    );
}

#[tokio::test]
async fn a_version_document_upstream_error_is_a_registry_error() {
    let (mut server, c) = upstream().await;
    server
        .mock("GET", "/api/v3/collections/acme/util/versions/1.0.0/")
        .with_status(502)
        .create_async()
        .await;
    let pkg = PackageId::new("g", "acme.util", "1.0.0");
    let err = c.resolve_metadata(&pkg).await.expect_err("502");
    assert!(matches!(err, CoreError::Registry(_)), "{err}");
}

#[tokio::test]
async fn the_collection_tarball_bytes_are_streamed_through() {
    use futures::TryStreamExt;
    let (mut server, c) = upstream().await;
    let download_url = format!("{}/api/x.tar.gz", server.url());
    json_mock(
        &mut server,
        "/api/v3/collections/acme/util/versions/1.0.0/",
        json!({ "version": "1.0.0", "download_url": download_url }),
    )
    .create_async()
    .await;
    server
        .mock("GET", "/api/x.tar.gz")
        .with_body("tarball-bytes")
        .create_async()
        .await;
    let pkg = PackageId::new("g", "acme.util", "1.0.0").with_artifact("tarball");
    let Ok(fetched) = c.fetch_artifact(&pkg).await else {
        panic!("fetches");
    };
    let chunks: Vec<_> = fetched.stream.try_collect().await.expect("streams");
    assert_eq!(chunks.concat(), b"tarball-bytes");
}

#[test]
fn the_artifact_name_is_the_upstream_tarball_filename() {
    assert_eq!(
        GalaxyRegistryClient::artifact_name("acme.util", "1.0.0").unwrap(),
        "acme-util-1.0.0.tar.gz"
    );
}

#[tokio::test]
async fn search_is_answered_locally_not_upstream() {
    let c = client("http://127.0.0.1:9");
    assert_eq!(c.registry_type(), "galaxy");
    assert_eq!(c.base_url(), "http://127.0.0.1:9");
    assert!(c.search_packages("docker", 10).await.unwrap().is_empty());
}
