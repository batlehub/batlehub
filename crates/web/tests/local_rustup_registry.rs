//! The Rust toolchain tree as a typed registry (RFC 0024, phases 1–2).
//!
//! What rustup does, and what this proxy therefore has to get right:
//!
//! - it resolves **every** install through a channel manifest, so a blocked
//!   release must be absent from the one it asks for — a `404` for a name that
//!   denotes one release, and a different manifest for a name that moves;
//! - it verifies each manifest against the `.sha256` beside it and refuses a
//!   mismatch, so an edited document served with upstream's sidecar installs
//!   nothing: the sidecar here is computed over the bytes actually served;
//! - it rewrites `https://static.rust-lang.org` in every component URL to
//!   `RUSTUP_DIST_SERVER` itself, so the manifest is relayed with its URLs as
//!   written and the tarball fetch still lands here.
//!
//! The fixture tree is in `tests/common/mod.rs`: `stable` is 1.98.1 of
//! 2026-09-03, the release before it is 1.98.0 of 2026-08-20, and there are two
//! nightlies a day apart.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, TestRequest};
use sha2::{Digest, Sha256};

const REG: &str = "rust";

async fn app() -> impl TestService {
    proxy_registry_app(REG, "rustup").await
}

fn manifest_url(name: &str) -> String {
    format!("/proxy/{REG}/rustup/dist/channel-rust-{name}.toml")
}

fn dated_manifest_url(date: &str, name: &str) -> String {
    format!("/proxy/{REG}/rustup/dist/{date}/channel-rust-{name}.toml")
}

/// The `X-BatleHub-Manifest` value of a response, which says which of the three
/// cases §4.4 names this body is.
async fn manifest_state<S: TestService>(app: &S, url: &str) -> String {
    let req = TestRequest::get()
        .uri(url)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert!(resp.status().is_success(), "{url}: {}", resp.status());
    resp.headers()
        .get("X-BatleHub-Manifest")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

async fn header_of<S: TestService>(app: &S, url: &str, header: &str) -> Option<String> {
    let req = TestRequest::get()
        .uri(url)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    resp.headers()
        .get(header)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

async fn status_of<S: TestService>(app: &S, url: &str) -> u16 {
    let req = TestRequest::get()
        .uri(url)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    call_service(app, req).await.status().as_u16()
}

// ── the manifest is the enforcement point ────────────────────────────────────

#[actix_web::test]
async fn a_channel_manifest_is_relayed_with_its_urls_untouched() {
    let app = app().await;
    let body = get_text(&app, &manifest_url("stable")).await;

    assert!(body.contains("date = \"2026-09-03\""));
    assert!(
        body.contains("https://static.rust-lang.org/dist/2026-09-03/rust-1.98.1"),
        "rustup rewrites the canonical host itself, so the document keeps it: {body}"
    );
    assert_eq!(
        manifest_state(&app, &manifest_url("stable")).await,
        "upstream"
    );
}

/// The sidecar is the hash of what this instance serves, never upstream's file:
/// rustup refuses a manifest whose digest does not match, so any other answer
/// installs nothing.
#[actix_web::test]
async fn the_sidecar_is_the_digest_of_the_served_manifest() {
    let app = app().await;
    let body = get_text(&app, &manifest_url("stable")).await;
    let line = get_text(&app, &format!("{}.sha256", manifest_url("stable"))).await;

    let expected = hex::encode(Sha256::digest(body.as_bytes()));
    assert_eq!(
        line,
        format!("{expected}  channel-rust-stable.toml\n"),
        "the coreutils line, over the bytes the manifest route answered with"
    );
    // rustup reads the first 64 characters as the hash and keeps the first 20
    // as the toolchain's update-hash.
    assert_eq!(&line[..64], expected);
}

/// An exact name denotes one release, so a blocked one is refused rather than
/// silently replaced: `rustup toolchain install 1.98.1` gets its own
/// "nonexistent rust version".
#[actix_web::test]
async fn a_blocked_exact_release_is_a_404_on_all_three_documents() {
    let app = app().await;
    block_version(&app, REG, "rust", "1.98.1").await;

    for url in [
        manifest_url("1.98.1"),
        format!("{}.sha256", manifest_url("1.98.1")),
        format!("{}.asc", manifest_url("1.98.1")),
    ] {
        assert_eq!(status_of(&app, &url).await, 404, "{url}");
    }
}

/// A dated name is exact whatever the channel says: the directory pins the
/// release, so `2026-09-05/channel-rust-nightly.toml` is refused rather than
/// repaired.
#[actix_web::test]
async fn a_dated_channel_is_exact_and_is_never_repaired() {
    let app = app().await;
    block_version(&app, REG, "rust", "nightly-2026-09-05").await;

    assert_eq!(
        status_of(&app, &dated_manifest_url("2026-09-05", "nightly")).await,
        404
    );
}

/// An alias moves, so a block on the release it currently names moves it: the
/// body is the previous release's, and the header says so. For `stable` that is
/// a downgrade, which is the block doing its job.
#[actix_web::test]
async fn a_blocked_alias_is_repaired_to_the_newest_allowed_release() {
    let app = app().await;
    block_version(&app, REG, "rust", "1.98.1").await;

    let body = get_text(&app, &manifest_url("stable")).await;
    assert!(body.contains("date = \"2026-08-20\""), "{body}");
    assert!(body.contains("1.98.0"), "{body}");

    assert_eq!(
        manifest_state(&app, &manifest_url("stable")).await,
        "repaired"
    );
    assert_eq!(
        header_of(&app, &manifest_url("stable"), "X-BatleHub-Version").await,
        Some("1.98.0".to_owned()),
        "the response names the release it actually served"
    );
}

/// The dated channel walks back a day, on the same rule.
#[actix_web::test]
async fn a_blocked_nightly_moves_the_undated_alias_back_one_day() {
    let app = app().await;
    block_version(&app, REG, "rust", "nightly-2026-09-05").await;

    let body = get_text(&app, &manifest_url("nightly")).await;
    assert!(body.contains("date = \"2026-09-04\""), "{body}");
    assert_eq!(
        header_of(&app, &manifest_url("nightly"), "X-BatleHub-Version").await,
        Some("nightly-2026-09-04".to_owned())
    );
}

/// The sidecar of a repaired manifest hashes the repaired body — the property
/// that makes the repair installable at all.
#[actix_web::test]
async fn the_sidecar_follows_the_repair() {
    let app = app().await;
    block_version(&app, REG, "rust", "1.98.1").await;

    let body = get_text(&app, &manifest_url("stable")).await;
    let line = get_text(&app, &format!("{}.sha256", manifest_url("stable"))).await;
    assert_eq!(
        line,
        format!(
            "{}  channel-rust-stable.toml\n",
            hex::encode(Sha256::digest(body.as_bytes()))
        ),
        "and the file name is still the one rustup asked for"
    );
}

/// With every release it could denote blocked, an alias has nothing to serve
/// and answers the not-found rustup would have got from upstream.
#[actix_web::test]
async fn an_alias_with_nothing_left_is_a_404() {
    let app = app().await;
    block_version(&app, REG, "rust", "1.98.1").await;
    block_version(&app, REG, "rust", "1.98.0").await;

    assert_eq!(status_of(&app, &manifest_url("stable")).await, 404);
}

// ── the listing ──────────────────────────────────────────────────────────────

/// `manifests.txt` is read by people and scripts rather than by rustup, and is
/// filtered anyway: a second document answering the same question unfiltered is
/// the gap RFC 0010 §4.4 names.
#[actix_web::test]
async fn manifests_txt_loses_the_blocked_release_and_keeps_the_rest() {
    let app = app().await;
    let url = format!("/proxy/{REG}/rustup/manifests.txt");

    let before = get_text(&app, &url).await;
    assert_eq!(before.lines().count(), 5);

    block_version(&app, REG, "rust", "1.98.1").await;

    let after = get_text(&app, &url).await;
    assert!(!after.contains("channel-rust-1.98.1.toml"), "{after}");
    assert!(after.contains("channel-rust-1.98.0.toml"));
    assert!(
        after.contains("2026-09-03/channel-rust-stable.toml"),
        "a dated stable snapshot names no version and is kept: {after}"
    );
}

// ── the files ────────────────────────────────────────────────────────────────

/// A component archive is cached under the release, whatever dated directory
/// served it, and the coordinate is read off the path with no manifest fetch.
#[actix_web::test]
async fn a_component_archive_is_cached_under_its_release() {
    let app = app().await;
    let url = format!(
        "/proxy/{REG}/rustup/dist/2026-09-03/rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz"
    );
    let body = get_text(&app, &url).await;
    assert!(
        body.contains("rust/1.98.1/rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz"),
        "the fixture echoes the cache key it was asked for: {body}"
    );
}

/// Hiding governs resolution; it does not replace diagnosis. A client holding a
/// manifest from before the block still gets a refusal at the file.
#[actix_web::test]
async fn a_blocked_releases_archive_is_refused_at_the_download_gate() {
    let app = app().await;
    block_version(&app, REG, "rust", "1.98.1").await;

    let url = format!(
        "/proxy/{REG}/rustup/dist/2026-09-03/rust-std-1.98.1-x86_64-unknown-linux-gnu.tar.xz"
    );
    assert_eq!(status_of(&app, &url).await, 403);
}

/// A nightly's archive takes its coordinate from the directory, so a block on
/// the dated release reaches the file too.
#[actix_web::test]
async fn a_nightly_archive_is_refused_by_its_dated_coordinate() {
    let app = app().await;
    block_version(&app, REG, "rust", "nightly-2026-09-05").await;

    let url = format!(
        "/proxy/{REG}/rustup/dist/2026-09-05/rustc-nightly-x86_64-unknown-linux-gnu.tar.xz"
    );
    assert_eq!(status_of(&app, &url).await, 403);
}

// ── the installer's own tree ─────────────────────────────────────────────────

/// `rustup-init.sh` fetches a path that names no version; the handler resolves
/// one so the cache holds a release rather than a moving name.
#[actix_web::test]
async fn the_bootstrap_path_is_served_under_the_version_release_stable_names() {
    let app = app().await;
    let url = format!("/proxy/{REG}/rustup/rustup/dist/x86_64-unknown-linux-gnu/rustup-init");
    let body = get_text(&app, &url).await;
    assert!(
        body.contains("rustup/1.29.1/x86_64-unknown-linux-gnu/rustup-init"),
        "the cache key names the release, not the moving path: {body}"
    );
}

#[actix_web::test]
async fn the_archive_path_serves_the_version_it_names() {
    let app = app().await;
    let url =
        format!("/proxy/{REG}/rustup/rustup/archive/1.29.0/x86_64-unknown-linux-gnu/rustup-init");
    let body = get_text(&app, &url).await;
    assert!(body.contains("rustup/1.29.0/x86_64-unknown-linux-gnu/rustup-init"));
}

/// The installer blocks independently of the toolchains: two packages, two
/// block lists.
#[actix_web::test]
async fn blocking_a_toolchain_does_not_block_the_installer() {
    let app = app().await;
    block_version(&app, REG, "rust", "1.98.1").await;

    let url =
        format!("/proxy/{REG}/rustup/rustup/archive/1.29.1/x86_64-unknown-linux-gnu/rustup-init");
    assert_eq!(status_of(&app, &url).await, 200);

    block_version(&app, REG, "rustup", "1.29.1").await;
    assert_eq!(status_of(&app, &url).await, 403);
}

// ── the edge ─────────────────────────────────────────────────────────────────

/// Every segment reaches a cache or storage key, so a name no release could
/// have is a `400` rather than a lookup.
#[actix_web::test]
async fn a_name_no_release_could_have_is_refused_at_the_edge() {
    let app = app().await;
    for name in ["latest", "1", "1.2.3.4"] {
        assert_eq!(status_of(&app, &manifest_url(name)).await, 400, "{name}");
    }
}

#[actix_web::test]
async fn a_dist_file_that_names_no_release_is_refused() {
    let app = app().await;
    let url = format!("/proxy/{REG}/rustup/dist/2026-09-03/rustc.tar.xz");
    assert_eq!(status_of(&app, &url).await, 400);
}

/// rustup's v1 fallback — the extensionless name — is answered the way upstream
/// answers it, so the message a client sees is the one it would have got.
#[actix_web::test]
async fn the_v1_fallback_name_is_a_404() {
    let app = app().await;
    let url = format!("/proxy/{REG}/rustup/dist/channel-rust-stable");
    assert_eq!(status_of(&app, &url).await, 404);
}

/// The two documents that answer "what is stable" agree, and the second one is
/// served as text rather than as bytes.
#[actix_web::test]
async fn the_stable_date_file_is_served_as_text() {
    let app = app().await;
    let url = format!("/proxy/{REG}/rustup/dist/channel-rust-stable-date.txt");
    assert_eq!(get_text(&app, &url).await, "2026-09-03");
    assert_content_type(&app, &url, "text/plain").await;
}

#[actix_web::test]
async fn the_signature_is_relayed_and_typed_as_one() {
    let app = app().await;
    let url = format!("{}.asc", manifest_url("stable"));
    let body = get_text(&app, &url).await;
    assert!(body.starts_with("-----BEGIN PGP SIGNATURE-----"), "{body}");
    assert_content_type(&app, &url, "application/pgp-signature").await;
    assert_eq!(
        header_of(&app, &url, "X-BatleHub-Manifest")
            .await
            .as_deref(),
        Some("upstream"),
        "the header says whether the signature still verifies what is served"
    );
}
