//! `mockito` against both files: the request logic in `client.rs` and the
//! vocabulary in `models.rs`.

use batlehub_core::{
    entities::PackageId,
    error::CoreError,
    ports::{DocumentKind, RegistryClient},
};
use futures::TryStreamExt;
use mockito::Server;

use super::models::{parse_validate_answer, relayed_path_is_allowed};
use super::SdkmanRegistryClient;

/// One mockito server standing in for both hosts: the API under `/2`, the
/// broker at the root. They are one origin, which is also what makes the
/// same-origin redirect below a *trusted* hop.
fn client(server: &Server) -> SdkmanRegistryClient {
    SdkmanRegistryClient::new(
        format!("{}/2", server.url()),
        server.url(),
        &Default::default(),
    )
    .unwrap()
}

fn pkg(candidate: &str, version: &str, platform: &str) -> PackageId {
    PackageId::new("jvm", candidate, version).with_artifact(platform)
}

// ── models ───────────────────────────────────────────────────────────────────

#[test]
fn the_validate_answer_is_one_of_two_words() {
    assert_eq!(parse_validate_answer("valid"), Some(true));
    assert_eq!(parse_validate_answer("invalid\n"), Some(false));
    assert_eq!(parse_validate_answer("<html>"), None);
    assert_eq!(parse_validate_answer(""), None);
}

#[test]
fn only_the_endpoints_the_client_calls_are_relayed() {
    for ok in [
        "candidates/all",
        "candidates/list",
        "candidates/validate/java/21.0.5-tem/linuxx64",
        "hooks/post/java/21.0.5-tem/linuxx64",
        "hooks/pre/java/21.0.5-tem/linuxx64",
        "healthcheck",
        "broker/version/sdkman/script/stable",
        "selfupdate/stable/linuxx64",
    ] {
        assert!(relayed_path_is_allowed(ok), "{ok} is relayed");
    }
    for bad in [
        "candidates/java/linuxx64/versions/all",
        "candidates/default/java",
        "candidates/validate",
        "candidates/validate/",
        "hooks/",
        "hooks/post/../../etc",
        "healthcheck?x=1",
        "/healthcheck",
        "admin",
        "",
    ] {
        assert!(!relayed_path_is_allowed(bad), "{bad} is not relayed");
    }
}

// ── resolve_metadata ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_valid_version_resolves_without_a_date() {
    let mut server = Server::new_async().await;
    let _validate = server
        .mock("GET", "/2/candidates/validate/java/21.0.5-tem/linuxx64")
        .with_body("valid")
        .create_async()
        .await;

    let meta = client(&server)
        .resolve_metadata(&pkg("java", "21.0.5-tem", "linuxx64"))
        .await
        .unwrap();
    assert!(meta.published_at.is_none(), "SDKMAN publishes no dates");
    assert_eq!(meta.extra["platform"], "linuxx64");
    assert_eq!(
        meta.download_url.as_deref(),
        Some(format!("{}/download/java/21.0.5-tem/linuxx64", server.url()).as_str())
    );
}

#[tokio::test]
async fn invalid_is_not_found() {
    let mut server = Server::new_async().await;
    let _validate = server
        .mock("GET", "/2/candidates/validate/java/99.0.0-nope/linuxx64")
        .with_body("invalid")
        .create_async()
        .await;

    let result = client(&server)
        .resolve_metadata(&pkg("java", "99.0.0-nope", "linuxx64"))
        .await;
    assert!(matches!(result, Err(CoreError::NotFound(_))), "{result:?}");
}

/// A captive portal or an HTML error page is not a validate answer.
#[tokio::test]
async fn an_answer_that_is_neither_word_is_an_upstream_error() {
    let mut server = Server::new_async().await;
    let _validate = server
        .mock("GET", "/2/candidates/validate/java/21.0.5-tem/linuxx64")
        .with_body("<html>Not Found</html>")
        .create_async()
        .await;

    let result = client(&server)
        .resolve_metadata(&pkg("java", "21.0.5-tem", "linuxx64"))
        .await;
    assert!(matches!(result, Err(CoreError::Registry(_))), "{result:?}");
}

/// The platform axis is a closed set: an unknown one is refused before any
/// request is made, not forwarded upstream as a path segment.
#[tokio::test]
async fn an_unknown_platform_is_refused_before_any_request() {
    let mut server = Server::new_async().await;
    let validate = server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let c = client(&server);

    let result = c
        .resolve_metadata(&pkg("java", "21.0.5-tem", "plan9"))
        .await;
    assert!(
        matches!(result, Err(CoreError::InvalidInput(_))),
        "{result:?}"
    );
    let result = c.fetch_artifact(&pkg("java", "21.0.5-tem", "../x")).await;
    assert!(
        matches!(result, Err(CoreError::InvalidInput(_))),
        "an unknown platform is refused before any request"
    );
    let result = c
        .resolve_metadata(&PackageId::new("jvm", "java", "21.0.5-tem"))
        .await;
    assert!(
        matches!(result, Err(CoreError::NotFound(_))),
        "no platform at all: {result:?}"
    );
    validate.assert_async().await;
}

// ── fetch_artifact ───────────────────────────────────────────────────────────

/// The broker answers `302` and the bytes live elsewhere; the chain is
/// followed here and the final body streamed.
#[tokio::test]
async fn the_brokers_redirect_is_followed_and_the_final_body_streamed() {
    let mut server = Server::new_async().await;
    let broker = server
        .mock("GET", "/download/maven/3.9.9/linuxx64")
        .with_status(302)
        .with_header("location", "/cdn/apache-maven-3.9.9-bin.zip")
        .with_header("x-sdkman-archivetype", "zip")
        .create_async()
        .await;
    let cdn = server
        .mock("GET", "/cdn/apache-maven-3.9.9-bin.zip")
        .with_header("cache-control", "max-age=3600")
        .with_body("PK\x03\x04maven")
        .create_async()
        .await;

    let fetched = client(&server)
        .fetch_artifact(&pkg("maven", "3.9.9", "linuxx64"))
        .await
        .unwrap();
    assert_eq!(fetched.cache_control.as_deref(), Some("max-age=3600"));
    let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
    let body: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
    assert_eq!(body, b"PK\x03\x04maven");
    broker.assert_async().await;
    cdn.assert_async().await;
}

/// A redirect off the configured origins to a private address is refused by
/// the SSRF guard: the broker names the download host, and that host is not
/// SDKMAN (RFC 0010 §7).
#[tokio::test]
async fn a_redirect_to_a_private_address_is_refused() {
    let mut server = Server::new_async().await;
    let _broker = server
        .mock("GET", "/download/java/21.0.5-tem/linuxx64")
        .with_status(302)
        .with_header("location", "http://169.254.169.254/latest/meta-data/")
        .create_async()
        .await;

    let result = client(&server)
        .fetch_artifact(&pkg("java", "21.0.5-tem", "linuxx64"))
        .await;
    let err = match result {
        Err(CoreError::Registry(msg)) => msg,
        Err(other) => panic!("expected the SSRF guard to refuse: {other}"),
        Ok(_) => panic!("expected the SSRF guard to refuse; the fetch succeeded"),
    };
    assert!(err.contains("SSRF guard"), "{err}");
}

#[tokio::test]
async fn a_broker_404_is_not_found() {
    let mut server = Server::new_async().await;
    let _broker = server
        .mock("GET", "/download/java/21.0.5-tem/linuxx64")
        .with_status(404)
        .create_async()
        .await;
    let result = client(&server)
        .fetch_artifact(&pkg("java", "21.0.5-tem", "linuxx64"))
        .await;
    assert!(
        matches!(result, Err(CoreError::NotFound(_))),
        "a broker 404 is NotFound"
    );
}

// ── fetch_version_document ───────────────────────────────────────────────────

#[tokio::test]
async fn versions_all_is_text_and_round_trips() {
    let mut server = Server::new_async().await;
    let _all = server
        .mock("GET", "/2/candidates/maven/linuxx64/versions/all")
        .with_header("content-type", "text/plain; charset=UTF-8")
        .with_body("3.9.8,3.9.9,4.0.0-rc-6")
        .create_async()
        .await;
    let c = client(&server);

    let doc = c
        .fetch_version_document("maven/linuxx64", DocumentKind::Versions)
        .await
        .unwrap();
    assert_eq!(doc.content_type, "text/plain; charset=utf-8");
    assert_eq!(doc.body.as_text(), Some("3.9.8,3.9.9,4.0.0-rc-6"));

    assert_eq!(
        c.list_versions("maven/linuxx64").await.unwrap(),
        ["3.9.8", "3.9.9", "4.0.0-rc-6"]
    );
}

/// The console asks about a candidate, not a candidate on a platform: the
/// default platform's list answers.
#[tokio::test]
async fn a_bare_candidate_reads_the_default_platforms_list() {
    let mut server = Server::new_async().await;
    let all = server
        .mock("GET", "/2/candidates/maven/linuxx64/versions/all")
        .with_body("3.9.9")
        .create_async()
        .await;
    assert_eq!(
        client(&server).list_versions("maven").await.unwrap(),
        ["3.9.9"]
    );
    all.assert_async().await;
}

#[tokio::test]
async fn the_default_document_is_read_for_the_candidate_alone() {
    let mut server = Server::new_async().await;
    let _default = server
        .mock("GET", "/2/candidates/default/java")
        .with_body("25.0.4-tem")
        .create_async()
        .await;
    let doc = client(&server)
        .fetch_version_document("java", DocumentKind::SDKMAN_DEFAULT)
        .await
        .unwrap();
    assert_eq!(doc.body.as_text(), Some("25.0.4-tem"));
}

/// The rendered list is a 400 upstream without its two parameters; the
/// client's query is forwarded as sent, and an absent one becomes the empty
/// pair.
#[tokio::test]
async fn the_rendered_list_forwards_the_clients_query() {
    let mut server = Server::new_async().await;
    let with_query = server
        .mock("GET", "/2/candidates/maven/linuxx64/versions/list")
        .match_query(mockito::Matcher::AllOf(vec![
            mockito::Matcher::UrlEncoded("current".into(), "3.9.9".into()),
            mockito::Matcher::UrlEncoded("installed".into(), "3.9.9,3.8.4".into()),
        ]))
        .with_body("rendered with query")
        .create_async()
        .await;
    let empty_pair = server
        .mock("GET", "/2/candidates/gradle/linuxx64/versions/list")
        .match_query(mockito::Matcher::AllOf(vec![
            mockito::Matcher::UrlEncoded("current".into(), "".into()),
            mockito::Matcher::UrlEncoded("installed".into(), "".into()),
        ]))
        .with_body("rendered bare")
        .create_async()
        .await;
    let c = client(&server);

    let doc = c
        .fetch_version_document(
            "maven/linuxx64?current=3.9.9&installed=3.9.9,3.8.4",
            DocumentKind::SDKMAN_VERSIONS_LIST,
        )
        .await
        .unwrap();
    assert_eq!(doc.body.as_text(), Some("rendered with query"));
    let doc = c
        .fetch_version_document("gradle/linuxx64", DocumentKind::SDKMAN_VERSIONS_LIST)
        .await
        .unwrap();
    assert_eq!(doc.body.as_text(), Some("rendered bare"));
    with_query.assert_async().await;
    empty_pair.assert_async().await;
}

#[tokio::test]
async fn a_relayed_document_is_fetched_from_the_api_as_is() {
    let mut server = Server::new_async().await;
    let _hook = server
        .mock("GET", "/2/hooks/post/java/21.0.5-tem/linuxx64")
        .with_body("#!/bin/bash\n#Post Hook: linux-java-tarball\n")
        .create_async()
        .await;
    let c = client(&server);

    let doc = c
        .fetch_version_document("hooks/post/java/21.0.5-tem/linuxx64", DocumentKind::RELAYED)
        .await
        .unwrap();
    assert_eq!(
        doc.body.as_text(),
        Some("#!/bin/bash\n#Post Hook: linux-java-tarball\n")
    );

    let result = c
        .fetch_version_document("admin/secrets", DocumentKind::RELAYED)
        .await;
    assert!(
        matches!(result, Err(CoreError::InvalidInput(_))),
        "{result:?}"
    );
}

#[tokio::test]
async fn an_unknown_platform_in_a_listing_is_refused_and_other_kinds_are_not_supported() {
    let server = Server::new_async().await;
    let c = client(&server);
    let result = c
        .fetch_version_document("java/plan9", DocumentKind::Versions)
        .await;
    assert!(
        matches!(result, Err(CoreError::InvalidInput(_))),
        "{result:?}"
    );
    let result = c.fetch_version_document("java", DocumentKind::LATEST).await;
    assert!(
        matches!(result, Err(CoreError::NotSupported(_))),
        "{result:?}"
    );
}
