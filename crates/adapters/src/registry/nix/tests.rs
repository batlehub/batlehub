//! Adapter tests for the Nix binary cache, against a mocked upstream.
//!
//! The fixtures are the real thing: the narinfo and `nix-cache-info` bodies
//! were fetched from `cache.nixos.org` (2026-09-17) and are quoted byte for
//! byte, because the properties being checked here — that a relay changes one
//! line, that a signature survives it — are only meaningful against a document
//! a real signer produced.

use batlehub_core::entities::PackageId;
use batlehub_core::error::CoreError;
use batlehub_core::ports::{DocumentKind, RegistryClient};
use batlehub_core::services::nix::{self, NarInfo};

use super::NixBinaryCacheClient;
use crate::registry::http_client::UpstreamHttpOptions;

const HASH: &str = "0001npbf2n4z3pjy6vm2mw8ywkqixxs6";

const NARINFO: &str = "\
StorePath: /nix/store/0001npbf2n4z3pjy6vm2mw8ywkqixxs6-hslua-aeson-2.3.2-doc
URL: nar/075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x.nar.zst
Compression: zstd
FileHash: sha256:10k72lz1iazridh4787xk3mfl6c5akf8x88xz7bnswc03b5gvyqp
FileSize: 46064
NarHash: sha256:075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x
NarSize: 226848
References: ghpayap4j5fqg9ryyzrfdj9ygdi01iw9-aeson-2.2.4.1-doc ibfrnxf4jrihd9gkax1sjlr707gz36jb-scientific-0.3.8.1-doc p6xzjlrry42f3pdcgk1xn53hps56ai8s-lua-2.3.4-doc q9915zjvbv0pi4hijw3hgx0nb8asyjlr-hslua-marshalling-2.3.2-doc r0fajfsqr1xlvr9177gh0jjq9b0axk7n-hslua-core-2.3.2.1-doc
Deriver: y1h1bh5gl539r42jydbnbmp3vyh11sva-hslua-aeson-2.3.2.drv
Sig: cache.nixos.org-1:21qiHy652KfJ7Rsnc+dy5KndgujuIQEU/oudrFh7sWkkLlT9r8F3AxKA//dMvr9xWBA3tITPZA6ZFC7KxxRJBA==
";

const CACHE_INFO: &str = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n";

fn client(base: &str) -> NixBinaryCacheClient {
    NixBinaryCacheClient::new(base, &UpstreamHttpOptions::default()).expect("client builds")
}

fn nar_pkg() -> PackageId {
    PackageId::new("nixcache", "hslua-aeson", "2.3.2-doc").with_artifact(format!(
        "{HASH}/075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x.nar.zst"
    ))
}

#[tokio::test]
async fn resolve_metadata_reads_the_coordinate_out_of_the_store_path() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(NARINFO)
        .create_async()
        .await;

    let meta = client(&server.url())
        .resolve_metadata(&nar_pkg())
        .await
        .expect("resolves");

    // The coordinate comes from `StorePath:`, not from what the caller asked
    // for — a request naming the wrong package still resolves to the truth.
    assert_eq!(meta.id.name, "hslua-aeson");
    assert_eq!(meta.id.version, "2.3.2-doc");
    assert_eq!(meta.published_at, None, "the protocol carries no dates");
    assert_eq!(meta.is_signed, Some(true));
    m.assert_async().await;
}

/// The RFC 0031 defect-1 regression, at the layer that would have shipped it.
#[tokio::test]
async fn the_advertised_digest_is_in_a_shape_the_verifier_can_read() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(NARINFO)
        .create_async()
        .await;

    let meta = client(&server.url())
        .resolve_metadata(&nar_pkg())
        .await
        .unwrap();

    let checksum = meta.checksum.expect("a NAR coordinate advertises a digest");
    let (_, bytes) = batlehub_core::services::integrity::parse_expected(&checksum)
        .expect("the verifier reads it — without this, cache writes go unverified");
    assert_eq!(bytes.len(), 32);
    // And it is `FileHash`, the digest of the bytes this server stores, not
    // `NarHash`, which covers the decompressed stream it never holds.
    let expected =
        nix::nix32_decode("10k72lz1iazridh4787xk3mfl6c5akf8x88xz7bnswc03b5gvyqp", 32).unwrap();
    assert_eq!(bytes, expected);
}

#[tokio::test]
async fn a_narinfo_is_relayed_with_every_byte_upstream_sent() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(NARINFO)
        .create_async()
        .await;

    let doc = client(&server.url())
        .fetch_version_document(HASH, DocumentKind::NARINFO)
        .await
        .expect("fetches");
    assert_eq!(doc.content_type, "text/x-nix-narinfo");
    assert_eq!(
        doc.body.as_text().map(str::to_owned),
        Some(NARINFO.to_owned()),
        "the adapter relays; the rewrite is the handler's, and it is one line"
    );
}

/// `getFile` maps all three to absence, and a build depends on the difference:
/// an upstream that refuses anonymous reads must make Nix try the next
/// substituter, not fail the build with an upstream error.
#[tokio::test]
async fn upstream_403_404_and_410_are_all_not_found() {
    for status in [403usize, 404, 410] {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", format!("/{HASH}.narinfo").as_str())
            .with_status(status)
            .create_async()
            .await;

        let err = client(&server.url())
            .fetch_version_document(HASH, DocumentKind::NARINFO)
            .await
            .expect_err("must be absence");
        assert!(
            matches!(err, CoreError::NotFound(_)),
            "{status} must be NotFound, got {err:?}"
        );
    }
}

#[tokio::test]
async fn cache_info_is_relayed_as_its_own_content_type() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/nix-cache-info")
        .with_status(200)
        .with_body(CACHE_INFO)
        .create_async()
        .await;

    let doc = client(&server.url())
        .fetch_version_document("", DocumentKind::CACHE_INFO)
        .await
        .unwrap();
    assert_eq!(doc.content_type, "text/x-nix-cache-info");
    assert_eq!(doc.body.as_text(), Some(CACHE_INFO));
}

#[tokio::test]
async fn there_is_no_listing_document_and_no_version_list() {
    let c = client("https://example.invalid");
    assert!(matches!(
        c.fetch_version_document(HASH, DocumentKind::Versions).await,
        Err(CoreError::NotSupported(_))
    ));
    assert!(matches!(
        c.list_versions("hello").await,
        Err(CoreError::NotSupported(_))
    ));
}

#[tokio::test]
async fn a_hash_outside_the_alphabet_never_reaches_the_upstream() {
    let mut server = mockito::Server::new_async().await;
    // Registered but never matched: the guard is at the edge.
    let m = server
        .mock("GET", mockito::Matcher::Any)
        .with_status(200)
        .expect(0)
        .create_async()
        .await;

    let c = client(&server.url());
    for bad in [
        "eeee1npbf2n4z3pjy6vm2mw8ywkqixxs", // `e` is not in Nix32
        "tooshort",
        "../../etc/passwd",
    ] {
        assert!(c.narinfo(bad).await.is_err(), "'{bad}' must be refused");
        assert!(c
            .fetch_version_document(bad, DocumentKind::NARINFO)
            .await
            .is_err());
    }
    m.assert_async().await;
}

#[tokio::test]
async fn a_nar_is_streamed_from_the_url_the_narinfo_recorded() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(NARINFO)
        .create_async()
        .await;
    let nar = server
        .mock(
            "GET",
            "/nar/075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x.nar.zst",
        )
        .with_status(200)
        .with_body(b"NAR-BYTES")
        .create_async()
        .await;

    let fetched = client(&server.url())
        .fetch_artifact(&nar_pkg())
        .await
        .expect("streams");
    let body = collect(fetched).await;
    assert_eq!(body, b"NAR-BYTES");
    nar.assert_async().await;
}

/// The trait's own obligation, and the shape of two shipped defects (Open VSX,
/// Terraform): the narinfo is an *upstream* document, so a `URL:` in it can be
/// absolute and point anywhere.
#[tokio::test]
async fn a_narinfo_cannot_point_the_nar_fetch_at_another_host() {
    let mut server = mockito::Server::new_async().await;
    let hostile = NARINFO.replace(
        "URL: nar/075lhsj33mkk02xn3lf59xn9glvh02wkw9xislbcj1jgjlpcn79x.nar.zst",
        "URL: https://attacker.example/evil.nar.zst",
    );
    server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(hostile)
        .create_async()
        .await;

    match client(&server.url()).fetch_artifact(&nar_pkg()).await {
        Ok(_) => panic!("a cross-host NAR URL must be refused, not fetched"),
        // A refusal, not an absence: `NotFound` would make a hybrid registry
        // fall through to the upstream and a client read it as "not here".
        Err(CoreError::NotFound(e)) => panic!("must not read as absence: {e}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn resolved_fetch_refuses_a_cross_host_download_url() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(NARINFO)
        .create_async()
        .await;
    let c = client(&server.url());
    let mut meta = c.resolve_metadata(&nar_pkg()).await.unwrap();
    meta.download_url = Some("https://attacker.example/evil.nar.zst".to_owned());

    assert!(
        c.fetch_artifact_resolved(&nar_pkg(), &meta).await.is_err(),
        "a download_url outside the cache root must be refused even when it \
         arrived through our own metadata"
    );
}

#[tokio::test]
async fn the_relayed_signature_still_verifies_after_the_rewrite() {
    const NIXOS_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(NARINFO)
        .create_async()
        .await;

    let doc = client(&server.url())
        .fetch_version_document(HASH, DocumentKind::NARINFO)
        .await
        .unwrap();
    let mut info = NarInfo::parse(doc.body.as_text().unwrap()).unwrap();
    nix::rewrite_url(&mut info, HASH).unwrap();

    let fp = nix::fingerprint(&info).unwrap();
    assert!(
        nix::verify_signature(&fp, info.get("Sig").unwrap(), &[NIXOS_KEY.to_owned()]),
        "a client verifies the relayed document with the upstream's key, \
         exactly as it would against the upstream"
    );
}

async fn collect(fetched: batlehub_core::ports::FetchedArtifact) -> Vec<u8> {
    use futures::StreamExt;
    let mut stream = fetched.stream;
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("chunk"));
    }
    out
}

/// **`checksum` is the NAR's, so it may only travel with a NAR coordinate.**
///
/// `download_url` is gated on the artifact shape and `checksum` was not, so a
/// `{hash}.ls` or a bare narinfo coordinate carried `FileHash` — a digest of
/// bytes the integrity check is not looking at. The verifier then refused a
/// document the cache had served correctly, and `nix store ls` failed on
/// every path in proxy mode.
#[tokio::test]
async fn only_a_nar_coordinate_carries_the_nar_checksum() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("GET", format!("/{HASH}.narinfo").as_str())
        .with_status(200)
        .with_body(NARINFO)
        .expect_at_least(1)
        .create_async()
        .await;
    let c = client(&server.url());

    // The NAR itself: verified against `FileHash`, in the SRI spelling
    // `integrity::parse_expected` can actually read.
    let nar = c.resolve_metadata(&nar_pkg()).await.expect("resolves");
    let sri = nar.checksum.expect("a NAR coordinate is verifiable");
    assert!(
        sri.starts_with("sha256-"),
        "the checksum must reach the verifier as SRI, got {sri}"
    );

    // A listing coordinate is the document, not the NAR: no checksum, and no
    // download URL either — the two travel together.
    for artifact in [format!("{HASH}.ls"), format!("{HASH}.narinfo")] {
        let pkg = PackageId::new("nixcache", "hslua-aeson", "2.3.2-doc").with_artifact(&artifact);
        let meta = c.resolve_metadata(&pkg).await.expect("resolves");
        assert!(
            meta.checksum.is_none(),
            "{artifact} is not the NAR; a NAR digest here fails every download"
        );
        assert!(meta.download_url.is_none(), "{artifact}");
    }
}
