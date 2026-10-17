//! `HelmRegistryClient` against a mocked repository, and the archive reader
//! against an archive built the way `helm package` lays one out.

use futures::TryStreamExt;
use mockito::{Matcher, Server};

use batlehub_core::{
    entities::PackageId, error::CoreError, ports::DocumentKind, ports::RegistryClient,
};

use super::{chart_yaml_from_archive, resolve_reference, HelmRegistryClient};
use crate::registry::http_client::UpstreamHttpOptions;

const DIGEST: &str = "d370858f873b904a966e64d530874e645114a86a3095f7afc43ed65b217afab8";

fn index(base: &str) -> String {
    format!(
        r#"apiVersion: v1
entries:
  cert-manager:
  - name: cert-manager
    version: v1.21.2
    created: "2026-09-01T10:00:00Z"
    digest: {DIGEST}
    urls:
    - charts/cert-manager-v1.21.2.tgz
  ingress-nginx:
  - name: ingress-nginx
    version: 4.15.1
    created: "2026-09-02T10:00:00Z"
    urls:
    - {base}/releases/download/ingress-nginx-4.15.1/ingress-nginx-4.15.1.tgz
  redis:
  - name: redis
    version: 20.0.0
    urls:
    - oci://registry-1.docker.io/bitnamicharts/redis
generated: "2026-09-02T10:00:00Z"
"#
    )
}

async fn upstream() -> mockito::ServerGuard {
    let mut server = Server::new_async().await;
    let body = index(&server.url());
    server
        .mock("GET", "/repo/index.yaml")
        .with_body(body)
        .create_async()
        .await;
    server
}

fn client(server: &mockito::ServerGuard) -> HelmRegistryClient {
    HelmRegistryClient::new(&format!("{}/repo", server.url()), &Default::default()).unwrap()
}

async fn body(client: &HelmRegistryClient, pkg: &PackageId) -> Result<Vec<u8>, CoreError> {
    let fetched = client.fetch_artifact(pkg).await?;
    let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await?;
    Ok(chunks.concat())
}

#[test]
fn resolve_reference_follows_resolve_reference_url() {
    let base = reqwest::Url::parse("https://h/charts?token=abc").unwrap();
    // Relative: the base path gets its trailing slash, and the base's query.
    assert_eq!(
        resolve_reference(&base, "x-1.0.0.tgz").unwrap().as_str(),
        "https://h/charts/x-1.0.0.tgz?token=abc"
    );
    let slashed = reqwest::Url::parse("https://h/charts/").unwrap();
    assert_eq!(
        resolve_reference(&slashed, "sub/x-1.0.0.tgz")
            .unwrap()
            .as_str(),
        "https://h/charts/sub/x-1.0.0.tgz"
    );
    // Absolute: taken as is.
    assert_eq!(
        resolve_reference(&base, "https://o/x.tgz")
            .unwrap()
            .as_str(),
        "https://o/x.tgz"
    );
}

#[tokio::test]
async fn the_index_is_parsed_and_one_chart_can_be_asked_for() {
    let server = upstream().await;
    let c = client(&server);
    let doc = c
        .fetch_version_document("index.yaml", DocumentKind::Versions)
        .await
        .unwrap();
    let json = doc.body.as_json().unwrap();
    assert!(json.pointer("/entries/cert-manager/0/digest").is_some());

    let one = c
        .fetch_version_document("ingress-nginx", DocumentKind::Versions)
        .await
        .unwrap();
    let entries = one.body.as_json().unwrap()["entries"].as_object().unwrap();
    assert_eq!(entries.keys().collect::<Vec<_>>(), ["ingress-nginx"]);
    assert_eq!(c.list_versions("cert-manager").await.unwrap(), ["v1.21.2"]);
}

#[tokio::test]
async fn a_relative_url_is_resolved_against_the_repository_and_carries_the_digest() {
    let mut server = upstream().await;
    let _chart = server
        .mock("GET", "/repo/charts/cert-manager-v1.21.2.tgz")
        .with_body("chart-bytes")
        .create_async()
        .await;
    let c = client(&server);
    let pkg = PackageId::new("h", "cert-manager", "v1.21.2").with_artifact("chart");
    let meta = c.resolve_metadata(&pkg).await.unwrap();
    assert_eq!(meta.checksum.as_deref(), Some(DIGEST));
    assert!(
        meta.published_at.is_some(),
        "published_at comes from created"
    );
    assert!(
        meta.extra["helm"].get("urls").is_none(),
        "upstream URLs are not kept"
    );
    assert_eq!(body(&c, &pkg).await.unwrap(), b"chart-bytes");
}

#[tokio::test]
async fn an_absolute_url_is_fetched_through_a_redirect_and_prov_is_its_sibling() {
    let mut server = upstream().await;
    let _r = server
        .mock(
            "GET",
            "/releases/download/ingress-nginx-4.15.1/ingress-nginx-4.15.1.tgz",
        )
        .with_status(302)
        .with_header("location", "/assets/signed?sig=1")
        .create_async()
        .await;
    let _a = server
        .mock("GET", "/assets/signed")
        .match_query(Matcher::Any)
        .with_body("nginx-bytes")
        .create_async()
        .await;
    let _p = server
        .mock(
            "GET",
            "/releases/download/ingress-nginx-4.15.1/ingress-nginx-4.15.1.tgz.prov",
        )
        .with_status(404)
        .create_async()
        .await;
    let c = client(&server);
    let chart = PackageId::new("h", "ingress-nginx", "4.15.1").with_artifact("chart");
    assert_eq!(body(&c, &chart).await.unwrap(), b"nginx-bytes");
    let meta = c.resolve_metadata(&chart).await.unwrap();
    assert!(
        meta.checksum.is_none(),
        "no digest in the entry, none invented"
    );

    let prov = PackageId::new("h", "ingress-nginx", "4.15.1").with_artifact("prov");
    assert!(matches!(body(&c, &prov).await, Err(CoreError::NotFound(_))));
}

#[tokio::test]
async fn an_unlisted_coordinate_and_an_oci_entry_are_not_found() {
    let server = upstream().await;
    let c = client(&server);
    for (name, version) in [
        ("cert-manager", "v9.9.9"),
        ("nope", "1.0.0"),
        ("redis", "20.0.0"),
    ] {
        let pkg = PackageId::new("h", name, version).with_artifact("chart");
        assert!(
            matches!(c.resolve_metadata(&pkg).await, Err(CoreError::NotFound(_))),
            "{name} {version}"
        );
    }
}

#[tokio::test]
async fn an_absolute_url_naming_a_private_host_is_refused() {
    let mut server = Server::new_async().await;
    server
        .mock("GET", "/repo/index.yaml")
        .with_body(
            "apiVersion: v1\nentries:\n  x:\n  - name: x\n    version: 1.0.0\n    urls: [\"http://169.254.169.254/latest/meta-data\"]\n",
        )
        .create_async()
        .await;
    let c = client(&server);
    let pkg = PackageId::new("h", "x", "1.0.0").with_artifact("chart");
    // The refusal itself, not any error: an unroutable host errors too.
    let err = body(&c, &pkg).await.unwrap_err().to_string();
    assert!(err.contains("SSRF guard"), "{err}");
}

fn chart_archive(files: &[(&str, &str)]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (path, contents) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, path, contents.as_bytes())
            .unwrap();
    }
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &raw).unwrap();
    gz.finish().unwrap()
}

#[test]
fn chart_yaml_is_read_from_the_root_only() {
    let bytes = chart_archive(&[
        (
            "mychart/charts/dep/Chart.yaml",
            "name: dep\nversion: 9.9.9\n",
        ),
        (
            "mychart/Chart.yaml",
            "apiVersion: v2\nname: mychart\nversion: 0.1.0\nappVersion: \"1.10\"\n",
        ),
        ("mychart/README.md", "# mychart\n"),
    ]);
    let chart = chart_yaml_from_archive(&bytes).unwrap();
    assert_eq!(chart["name"], "mychart");
    assert_eq!(chart["version"], "0.1.0");
    assert_eq!(chart["appVersion"], "1.10");

    let none = chart_archive(&[("mychart/values.yaml", "a: 1\n")]);
    assert!(chart_yaml_from_archive(&none).is_err());
    assert!(chart_yaml_from_archive(b"not gzip").is_err());
}

#[test]
fn a_dot_slash_archive_is_read_as_helm_reads_it() {
    let bytes = chart_archive(&[("./mychart/Chart.yaml", "name: mychart\nversion: 0.1.0\n")]);
    assert_eq!(chart_yaml_from_archive(&bytes).unwrap()["name"], "mychart");
}

#[test]
fn decompression_stops_at_helms_chart_size_limit() {
    // 101 MiB of zeros ahead of Chart.yaml: a few hundred KB on the wire.
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(101 * 1024 * 1024);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(
        &mut header,
        "mychart/big",
        std::io::Read::take(std::io::repeat(0), 101 * 1024 * 1024),
    )
    .unwrap();
    let body = "name: mychart\nversion: 0.1.0\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "mychart/Chart.yaml", body.as_bytes())
        .unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &tar.into_inner().unwrap()).unwrap();
    assert!(chart_yaml_from_archive(&gz.finish().unwrap()).is_err());
}

#[test]
fn options_are_accepted() {
    let opts = UpstreamHttpOptions {
        basic_auth: Some(("u".into(), "p".into())),
        ..Default::default()
    };
    assert!(HelmRegistryClient::new("https://charts.example/repo", &opts).is_ok());
    assert!(HelmRegistryClient::new("not a url", &opts).is_err());
}
