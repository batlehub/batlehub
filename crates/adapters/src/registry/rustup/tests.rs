//! The read client against a mocked dist tree (RFC 0024 §6.4).
//!
//! Standalone rather than inline, because the cases that matter span the
//! client and the core parser: recovering a stable release's date from
//! `manifests.txt` is a `ManifestsTxt` question answered with an HTTP fetch,
//! and the point of the tests is that the two agree.

use batlehub_core::{
    entities::PackageId,
    ports::{DocumentKind, RegistryClient, VersionDocument},
};
use futures::TryStreamExt;
use mockito::Server;

use super::client::RustupRegistryClient;

fn client(url: &str) -> RustupRegistryClient {
    RustupRegistryClient::new(url, &Default::default()).unwrap()
}

const MANIFESTS: &str = "static.rust-lang.org/dist/2026-08-20/channel-rust-1.98.0.toml\n\
    static.rust-lang.org/dist/2026-09-03/channel-rust-1.98.1.toml\n\
    static.rust-lang.org/dist/2026-09-05/channel-rust-nightly.toml\n";

fn text_of(doc: &VersionDocument) -> String {
    doc.body
        .as_text()
        .unwrap_or_else(|| panic!("expected a text document, got {doc:?}"))
        .to_owned()
}

#[tokio::test]
async fn a_stable_release_finds_its_dated_directory_through_manifests_txt() {
    let mut server = Server::new_async().await;
    let manifests = server
        .mock("GET", "/manifests.txt")
        .with_status(200)
        .with_body(MANIFESTS)
        .expect(1)
        .create_async()
        .await;
    let head = server
        .mock(
            "HEAD",
            "/dist/2026-09-03/rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz",
        )
        .with_status(200)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("r", "rust", "1.98.1")
        .with_artifact("rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz");
    let meta = c.resolve_metadata(&pkg).await.unwrap();

    assert_eq!(
        meta.published_at.unwrap().to_rfc3339(),
        "2026-09-03T00:00:00+00:00",
        "the release day is its earliest instant, so the age gate never ages a release early"
    );
    manifests.assert_async().await;
    head.assert_async().await;
}

#[tokio::test]
async fn a_dated_channel_needs_no_lookup_at_all() {
    let mut server = Server::new_async().await;
    // No `manifests.txt` mock: asking for it would be a failed request, and
    // that is the assertion — the date is in the version.
    let head = server
        .mock(
            "HEAD",
            "/dist/2026-09-05/rustc-nightly-x86_64-unknown-linux-gnu.tar.xz",
        )
        .with_status(200)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("r", "rust", "nightly-2026-09-05")
        .with_artifact("rustc-nightly-x86_64-unknown-linux-gnu.tar.xz");
    let meta = c.resolve_metadata(&pkg).await.unwrap();
    assert_eq!(
        meta.published_at.unwrap().to_rfc3339(),
        "2026-09-05T00:00:00+00:00"
    );
    head.assert_async().await;
}

#[tokio::test]
async fn the_manifests_file_is_read_once_for_many_coordinates() {
    let mut server = Server::new_async().await;
    let manifests = server
        .mock("GET", "/manifests.txt")
        .with_status(200)
        .with_body(MANIFESTS)
        .expect(1)
        .create_async()
        .await;
    let _heads = server
        .mock("HEAD", mockito::Matcher::Any)
        .with_status(200)
        .expect_at_least(2)
        .create_async()
        .await;

    let c = client(&server.url());
    for file in ["rustc-1.98.1-x.tar.xz", "cargo-1.98.1-x.tar.xz"] {
        let pkg = PackageId::new("r", "rust", "1.98.1").with_artifact(file);
        c.resolve_metadata(&pkg).await.unwrap();
    }
    manifests.assert_async().await;
}

#[tokio::test]
async fn a_release_manifests_txt_does_not_name_is_not_found() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/manifests.txt")
        .with_status(200)
        .with_body(MANIFESTS)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("r", "rust", "1.97.0").with_artifact("rustc-1.97.0-x.tar.xz");
    let err = c.resolve_metadata(&pkg).await.unwrap_err();
    assert!(
        err.to_string().contains("manifests.txt"),
        "the error says which document was consulted: {err}"
    );
}

#[tokio::test]
async fn a_manifest_is_addressed_by_the_channel_in_its_package_string() {
    let mut server = Server::new_async().await;
    let undated = server
        .mock("GET", "/dist/channel-rust-stable.toml")
        .with_status(200)
        .with_body("date = \"2026-09-03\"\n")
        .create_async()
        .await;
    let dated = server
        .mock("GET", "/dist/2026-09-05/channel-rust-nightly.toml")
        .with_status(200)
        .with_body("date = \"2026-09-05\"\n")
        .create_async()
        .await;

    let c = client(&server.url());
    let doc = c
        .fetch_version_document("rust/stable", DocumentKind::MANIFEST)
        .await
        .unwrap();
    assert!(text_of(&doc).contains("2026-09-03"));

    let doc = c
        .fetch_version_document("rust/2026-09-05/nightly", DocumentKind::MANIFEST)
        .await
        .unwrap();
    assert!(text_of(&doc).contains("2026-09-05"));

    undated.assert_async().await;
    dated.assert_async().await;
}

#[tokio::test]
async fn a_package_string_with_no_channel_is_refused_rather_than_guessed_at() {
    let server = Server::new_async().await;
    let c = client(&server.url());
    let err = c
        .fetch_version_document("rust", DocumentKind::MANIFEST)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("names no channel"), "{err}");
}

#[tokio::test]
async fn the_three_other_documents_are_the_paths_the_tree_serves() {
    let mut server = Server::new_async().await;
    let m1 = server
        .mock("GET", "/manifests.txt")
        .with_status(200)
        .with_body(MANIFESTS)
        .create_async()
        .await;
    let m2 = server
        .mock("GET", "/dist/channel-rust-stable-date.txt")
        .with_status(200)
        .with_body("2026-09-03")
        .create_async()
        .await;
    let m3 = server
        .mock("GET", "/rustup/release-stable.toml")
        .with_status(200)
        // Upstream's own bytes: the release tooling single-quotes both
        // values. A fixture that double-quoted them let every route test
        // pass against a reader that only handled `"`, while the real tree
        // answered 404 (found by tests/heavy/rustup.sh §7).
        .with_body("schema-version = '1'\nversion = '1.29.1'\n")
        .create_async()
        .await;

    let c = client(&server.url());
    assert!(text_of(
        &c.fetch_version_document("rust", DocumentKind::Versions)
            .await
            .unwrap()
    )
    .contains("channel-rust-1.98.1.toml"));
    assert_eq!(
        text_of(
            &c.fetch_version_document("rust", DocumentKind::STABLE_DATE)
                .await
                .unwrap()
        ),
        "2026-09-03"
    );
    assert!(text_of(
        &c.fetch_version_document("rustup", DocumentKind::RUSTUP_RELEASE)
            .await
            .unwrap()
    )
    .contains("1.29.1"));

    m1.assert_async().await;
    m2.assert_async().await;
    m3.assert_async().await;
}

#[tokio::test]
async fn the_installer_is_its_own_package_with_no_date() {
    let mut server = Server::new_async().await;
    let head = server
        .mock(
            "HEAD",
            "/rustup/archive/1.29.1/x86_64-unknown-linux-gnu/rustup-init",
        )
        .with_status(200)
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("r", "rustup", "1.29.1")
        .with_artifact("x86_64-unknown-linux-gnu/rustup-init");
    let meta = c.resolve_metadata(&pkg).await.unwrap();
    assert!(
        meta.published_at.is_none(),
        "the installer's tree publishes no dates — the case deny_missing_timestamp decides"
    );
    head.assert_async().await;
}

#[tokio::test]
async fn an_artifact_is_streamed_from_its_dated_directory() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/manifests.txt")
        .with_status(200)
        .with_body(MANIFESTS)
        .create_async()
        .await;
    let file = server
        .mock("GET", "/dist/2026-09-03/rustc-1.98.1-x.tar.xz")
        .with_status(200)
        .with_body("tarball bytes")
        .create_async()
        .await;

    let c = client(&server.url());
    let pkg = PackageId::new("r", "rust", "1.98.1").with_artifact("rustc-1.98.1-x.tar.xz");
    let fetched = c.fetch_artifact(&pkg).await.unwrap();
    let bytes: Vec<u8> = fetched
        .stream
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
        .unwrap();
    assert_eq!(bytes, b"tarball bytes");
    file.assert_async().await;
}

#[tokio::test]
async fn versions_are_every_release_manifests_txt_names_and_the_installer_has_one() {
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/manifests.txt")
        .with_status(200)
        .with_body(MANIFESTS)
        .create_async()
        .await;
    let _r = server
        .mock("GET", "/rustup/release-stable.toml")
        .with_status(200)
        .with_body("version = '1.29.1'\n")
        .create_async()
        .await;

    let c = client(&server.url());
    assert_eq!(
        c.list_versions("rust").await.unwrap(),
        ["1.98.0", "1.98.1", "nightly-2026-09-05"]
    );
    assert_eq!(c.list_versions("rustup").await.unwrap(), ["1.29.1"]);
}
