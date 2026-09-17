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
