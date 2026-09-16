//! The RubyGems compact index, served from a local registry.
//!
//! RFC 0009 §12.15. `/versions`, `/info/{gem}` and `/names` are what Bundler
//! resolves from — the JSON APIs are a fallback it reaches for only when the
//! compact index is absent. All three shipped proxy-only, so a gem published to
//! a **local** registry was invisible to `bundle install`, and a local registry
//! answered `/versions` with rubygems.org's index.
//!
//! Measured with Bundler 4.0.17 against a real server: `Could not find gem
//! 'e2eprobe'` immediately after publishing it, and `Bundle complete!`
//! afterwards.
//!
//! Covers all three modes' generation, the hybrid merge, and the conditional
//! and partial answers of §13.24 — the last through the routes, because
//! `range.rs`'s own tests prove the helper and not its use.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, TestRequest};

use batlehub_config::schema::RegistryMode;

/// A `dependencies:` block as a gemspec writes it.
fn runtime_dependency(name: &str, op: &str, version: &str) -> String {
    format!(
        "dependencies:\n\
         - !ruby/object:Gem::Dependency\n  \
           name: {name}\n  \
           requirement: !ruby/object:Gem::Requirement\n    \
             requirements:\n    \
             - - \"{op}\"\n      \
               - !ruby/object:Gem::Version\n        \
                 version: '{version}'\n  \
           type: :runtime\n"
    )
}

async fn local_gems_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-gems", "rubygems", RegistryMode::Local, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

async fn hybrid_gems_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-gems", "rubygems", RegistryMode::Hybrid, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

async fn publish<S>(app: &S, gem: Vec<u8>)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::post()
        .uri("/proxy/local-gems/api/v1/gems")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(gem)
        .to_request();
    assert_eq!(call_service(app, req).await.status(), 200);
}

async fn text<S>(app: &S, uri: &str) -> String
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "{uri}");
    String::from_utf8(read_body(resp).await.to_vec()).unwrap()
}

/// The document Bundler fetches first must name the gems this registry holds.
#[actix_web::test]
async fn compact_versions_lists_locally_published_gems() {
    let app = local_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;
    publish(&app, make_gem_with_deps("mygem", "1.1.0", "")).await;

    let versions = text(&app, "/proxy/local-gems/versions").await;
    assert!(
        versions.starts_with("created_at: "),
        "compact index needs its header: {versions}"
    );
    let line = versions
        .lines()
        .find(|l| l.starts_with("mygem "))
        .unwrap_or_else(|| panic!("no line for mygem in:\n{versions}"));
    let mut parts = line.split(' ');
    assert_eq!(parts.next(), Some("mygem"));
    assert_eq!(parts.next(), Some("1.0.0,1.1.0"), "both versions, in order");
    assert_eq!(
        parts.next().map(str::len),
        Some(32),
        "each line ends with the MD5 of that gem's info document"
    );
}

/// That trailing checksum is how Bundler decides whether its cached copy of the
/// info document is current, so it has to be the MD5 of *that* document.
#[actix_web::test]
async fn the_versions_checksum_is_the_md5_of_the_info_document() {
    let app = local_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;

    let versions = text(&app, "/proxy/local-gems/versions").await;
    let advertised = versions
        .lines()
        .find(|l| l.starts_with("mygem "))
        .and_then(|l| l.split(' ').nth(2))
        .expect("checksum on the versions line")
        .to_owned();

    let info = text(&app, "/proxy/local-gems/info/mygem").await;
    let actual = {
        use md5::{Digest as _, Md5};
        hex::encode(Md5::digest(info.as_bytes()))
    };
    assert_eq!(advertised, actual);
}

/// Dependencies are carried inline by the compact index, and a resolver handed
/// an empty list installs a gem without the gems it needs.
#[actix_web::test]
async fn compact_info_carries_runtime_dependencies() {
    let app = local_gems_app().await;
    publish(
        &app,
        make_gem_with_deps("mygem", "1.0.0", &runtime_dependency("rake", "~>", "13.0")),
    )
    .await;

    let info = text(&app, "/proxy/local-gems/info/mygem").await;
    assert!(
        info.starts_with("---\n"),
        "info needs its separator: {info}"
    );
    let line = info.lines().nth(1).expect("one version line");
    assert!(
        line.starts_with("1.0.0 rake:~> 13.0|checksum:"),
        "expected version, deps and checksum; got {line}"
    );
}

/// `/names` is the third compact document, and it lists what exists.
#[actix_web::test]
async fn compact_names_lists_locally_published_gems() {
    let app = local_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;

    let names = text(&app, "/proxy/local-gems/names").await;
    assert_eq!(names, "---\nmygem\n");
}

/// A local registry answers from its own database and asks nobody.
///
/// The upstream client in this harness answers every request, so a handler that
/// reached for it would return that instead — which is exactly what these three
/// routes used to do, in every mode.
#[actix_web::test]
async fn a_local_registry_does_not_serve_the_upstream_index() {
    let app = local_gems_app().await;

    let versions = text(&app, "/proxy/local-gems/versions").await;
    assert_eq!(
        versions.lines().filter(|l| !l.is_empty()).count(),
        2,
        "an empty local registry has a header and a separator and nothing else: {versions}"
    );

    let names = text(&app, "/proxy/local-gems/names").await;
    assert_eq!(names, "---\n");
}

// ── Hybrid mode ──────────────────────────────────────────────────────────────
//
// Hybrid appends this registry's gems to the upstream document. Nothing
// exercised that merge: §12.15 was measured in local mode, and the claim about
// hybrid was made in the RFC and the registry page with no test under it —
// which is the shape of gap this whole RFC is about.

/// A hybrid registry serves both, in one document.
///
/// The header and separator belong to the upstream document; the local lines
/// follow it. A second `created_at:` or `---` in the middle would end the
/// document as far as a compact-index parser is concerned.
#[actix_web::test]
async fn hybrid_versions_carries_upstream_and_local_gems() {
    let app = hybrid_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;

    let versions = text(&app, "/proxy/local-gems/versions").await;
    for expected in [
        "rails 1.0.0,1.1.0,2.0.0-beta.1",
        "rack 1.0.0",
        "mygem 1.0.0",
    ] {
        assert!(
            versions.lines().any(|l| l.starts_with(expected)),
            "expected a line for {expected} in:\n{versions}"
        );
    }
    assert_eq!(
        versions.matches("created_at: ").count(),
        1,
        "one header, upstream's: {versions}"
    );
    assert_eq!(
        versions.lines().filter(|l| *l == "---").count(),
        1,
        "one separator: {versions}"
    );
}

/// `/names` merges the same way.
#[actix_web::test]
async fn hybrid_names_carries_upstream_and_local_gems() {
    let app = hybrid_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;

    let names = text(&app, "/proxy/local-gems/names").await;
    let listed: Vec<&str> = names.lines().filter(|l| *l != "---").collect();
    assert!(
        listed.contains(&"rack") && listed.contains(&"rails"),
        "{names}"
    );
    assert!(listed.contains(&"mygem"), "{names}");
    assert_eq!(names.lines().filter(|l| *l == "---").count(), 1, "{names}");
}

/// `/info/{gem}` is per-gem, so it answers from whichever side hosts it —
/// locally published first, upstream for anything this registry does not have.
#[actix_web::test]
async fn hybrid_info_prefers_local_and_falls_through_for_the_rest() {
    let app = hybrid_gems_app().await;
    publish(
        &app,
        make_gem_with_deps("mygem", "1.0.0", &runtime_dependency("rake", "~>", "13.0")),
    )
    .await;

    let local = text(&app, "/proxy/local-gems/info/mygem").await;
    assert!(
        local
            .lines()
            .nth(1)
            .is_some_and(|l| l.starts_with("1.0.0 rake:~> 13.0|checksum:")),
        "the local gem's own info, not upstream's: {local}"
    );

    let upstream = text(&app, "/proxy/local-gems/info/rails").await;
    assert!(
        upstream.contains("2.0.0-beta.1"),
        "a gem this registry does not host comes from upstream: {upstream}"
    );
}

// ── Incremental fetch ────────────────────────────────────────────────────────
//
// `handlers/proxy/rubygems/range.rs` decides what a conditional or partial
// request is answered with, and its unit tests cover that decision. What they
// cannot see is whether the route is wired to it — the half of the problem §5.1
// exists for, and the half that was wrong every previous time in this RFC.

/// Fetch a compact document with arbitrary request headers.
async fn conditional<S>(
    app: &S,
    uri: &str,
    headers: &[(&str, String)],
) -> actix_web::dev::ServiceResponse<actix_web::body::BoxBody>
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let mut req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)));
    for (name, value) in headers {
        req = req.insert_header((*name, value.clone()));
    }
    call_service(app, req.to_request()).await
}

/// The algorithm of the compact-index `ETag` — SHA-256, and opaque to the
/// client, which reads back whatever the server sent (RFC 0009 §13.24).
fn etag_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// A client that already holds the document is told so, rather than being sent
/// it again — which is the cheap half of what the `ETag` is for.
#[actix_web::test]
async fn a_current_copy_of_the_versions_document_is_not_modified() {
    let app = local_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;

    let resp = conditional(&app, "/proxy/local-gems/versions", &[]).await;
    assert_eq!(resp.status(), 200);
    let etag = resp
        .headers()
        .get("etag")
        .expect("every answer carries a validator")
        .to_str()
        .unwrap()
        .to_owned();

    let resp = conditional(
        &app,
        "/proxy/local-gems/versions",
        &[("If-None-Match", etag)],
    )
    .await;
    assert_eq!(resp.status(), 304);
}

/// The expensive half: a client holding our prefix gets only the tail, and the
/// digest that lets Bundler append it.
///
/// Without `Repr-Digest` Bundler discards a `206` and re-fetches the whole
/// document, so a partial answer that lacks it costs more than never sending
/// one (RFC 0009 §13.24).
#[actix_web::test]
async fn a_client_holding_our_prefix_is_sent_only_the_tail() {
    let app = local_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;

    let document = text(&app, "/proxy/local-gems/versions").await;
    let held = document.len() / 2;
    let prefix_tag = format!("\"{}\"", etag_hex(&document.as_bytes()[..held]));

    let resp = conditional(
        &app,
        "/proxy/local-gems/versions",
        &[
            ("If-None-Match", prefix_tag),
            ("Range", format!("bytes={held}-")),
        ],
    )
    .await;
    assert_eq!(resp.status(), 206);
    assert_eq!(
        resp.headers().get("content-range").unwrap(),
        format!("bytes {held}-{}/{}", document.len() - 1, document.len()).as_str()
    );

    let expected_digest = {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use sha2::{Digest as _, Sha256};
        format!(
            "sha-256=:{}:",
            STANDARD.encode(Sha256::digest(document.as_bytes()))
        )
    };
    assert_eq!(
        resp.headers().get("repr-digest").unwrap(),
        expected_digest.as_str(),
        "the digest describes the whole document, not the slice"
    );

    let tail = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert_eq!(
        format!("{}{tail}", &document[..held]),
        document,
        "prefix plus tail must reassemble the document Bundler then verifies"
    );
}

/// A client holding something else entirely gets the whole document, not a
/// `206` it would have to detect as corrupt and fetch again.
#[actix_web::test]
async fn a_client_holding_a_different_document_gets_the_whole_one() {
    let app = local_gems_app().await;
    publish(&app, make_gem_with_deps("mygem", "1.0.0", "")).await;
    let document = text(&app, "/proxy/local-gems/versions").await;

    let resp = conditional(
        &app,
        "/proxy/local-gems/versions",
        &[
            (
                "If-None-Match",
                "\"0123456789abcdef0123456789abcdef\"".to_owned(),
            ),
            ("Range", "bytes=8-".to_owned()),
        ],
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        String::from_utf8(read_body(resp).await.to_vec()).unwrap(),
        document
    );
}

/// A gem this registry does not have is absent, not proxied.
#[actix_web::test]
async fn compact_info_for_an_unknown_gem_is_404_in_local_mode() {
    let app = local_gems_app().await;
    let req = TestRequest::get()
        .uri("/proxy/local-gems/info/nosuchgem")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}
