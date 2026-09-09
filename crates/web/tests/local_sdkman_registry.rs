//! SDKMAN as a typed registry (RFC 0010, phases 5–6).
//!
//! What `sdk` does, and what this proxy therefore has to get right:
//!
//! - it resolves **every** install through `candidates/validate/{c}/{v}/{plat}`
//!   and stops on anything but `valid` — so a blocked version must answer
//!   `invalid` there, and no download may be attempted;
//! - `sdk install java` with no version reads `candidates/default/java` and
//!   then validates it — so a blocked default must be repaired to a version
//!   the validate will accept;
//! - `sdk list java` prints the rendered `versions/list`, in one of two
//!   layouts — so a blocked version must be gone from both, with the table
//!   still aligned;
//! - a block is on the *candidate*: an admin blocking a JDK means all eight
//!   platforms, not the one whose listing they were reading.
//!
//! The fixture is `FixedRegistry`'s three versions (see `tests/common/mod.rs`);
//! `java` renders the vendor-table layout and every other candidate the grid.
//! The broker's redirect chain is the adapter's job and is tested there with
//! `mockito`; here the artifact route is tested for its coordinate, its cache
//! key and its edge validation.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, TestRequest};
use batlehub_config::schema::RegistryMode;
use batlehub_core::services::proxy::proxy_artifact_key;

const REG: &str = "jvm";

async fn app() -> impl TestService {
    proxy_registry_app(REG, "sdkman").await
}

fn url(path: &str) -> String {
    format!("/proxy/{REG}/sdkman/{path}")
}

// ── the listings ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn versions_all_lists_every_version_as_text() {
    let app = app().await;
    assert_content_type(
        &app,
        &url("candidates/java/linuxx64/versions/all"),
        "text/plain",
    )
    .await;
    assert_eq!(
        get_text(&app, &url("candidates/java/linuxx64/versions/all")).await,
        "1.0.0,1.1.0,2.0.0-beta.1"
    );
}

#[actix_web::test]
async fn versions_all_hides_a_blocked_version() {
    let app = app().await;
    block_version(&app, REG, "java", "1.1.0").await;
    assert_eq!(
        get_text(&app, &url("candidates/java/linuxx64/versions/all")).await,
        "1.0.0,2.0.0-beta.1"
    );
}

/// RFC 0010 decision 8: the listing coordinate carries the platform, the
/// block does not. One block, eight platforms.
#[actix_web::test]
async fn a_block_on_the_candidate_covers_every_platform() {
    let app = app().await;
    block_version(&app, REG, "java", "1.1.0").await;
    for platform in ["linuxx64", "darwinarm64", "windowsx64", "exotic"] {
        let body = get_text(
            &app,
            &url(&format!("candidates/java/{platform}/versions/all")),
        )
        .await;
        assert!(!body.contains("1.1.0"), "{platform}: {body}");
    }
}

/// `candidates/default` names one version and carries no list: when the one
/// it names is blocked it is repaired to the newest survivor, the way npm's
/// `dist-tags.latest` is.
#[actix_web::test]
async fn a_blocked_default_is_repaired_to_the_newest_allowed_version() {
    let app = app().await;
    assert_eq!(
        get_text(&app, &url("candidates/default/java")).await,
        "1.1.0"
    );

    block_version(&app, REG, "java", "1.1.0").await;
    // `1.0.0` is the highest stable survivor; the beta does not win over it.
    assert_eq!(
        get_text(&app, &url("candidates/default/java")).await,
        "1.0.0"
    );
    assert_content_type(&app, &url("candidates/default/java"), "text/plain").await;
}

#[actix_web::test]
async fn an_allowed_default_is_served_as_upstream_sent_it() {
    let app = app().await;
    block_version(&app, REG, "java", "1.0.0").await;
    assert_eq!(
        get_text(&app, &url("candidates/default/java")).await,
        "1.1.0"
    );
}

// ── the chokepoint ───────────────────────────────────────────────────────────

/// A blocked version answers `invalid`, which is where `sdk install` stops on
/// its own *"Stop! … is not a valid java version."* (RFC 0010 §7).
#[actix_web::test]
async fn validate_answers_invalid_for_a_blocked_version_and_valid_otherwise() {
    let app = app().await;
    assert_eq!(
        get_text(&app, &url("candidates/validate/java/1.1.0/linuxx64")).await,
        "valid"
    );

    block_version(&app, REG, "java", "1.1.0").await;
    for platform in ["linuxx64", "darwinarm64"] {
        assert_eq!(
            get_text(
                &app,
                &url(&format!("candidates/validate/java/1.1.0/{platform}"))
            )
            .await,
            "invalid",
            "{platform}"
        );
    }
    assert_eq!(
        get_text(&app, &url("candidates/validate/java/1.0.0/linuxx64")).await,
        "valid",
        "the other versions are untouched"
    );
}

// ── the rendered table ───────────────────────────────────────────────────────

/// `sdk list java` — the vendor layout: a blocked version is a whole row.
#[actix_web::test]
async fn a_blocked_version_is_absent_from_the_rendered_java_table() {
    let app = app().await;
    let path = "candidates/java/linuxx64/versions/list?current=&installed=";
    let before = get_text(&app, &url(path)).await;
    assert!(before.contains("| 1.1.0"), "{before}");

    block_version(&app, REG, "java", "1.1.0").await;
    let after = get_text(&app, &url(path)).await;
    assert!(!after.contains("| 1.1.0"), "{after}");
    assert!(after.contains("| 1.0.0"), "{after}");
    assert!(
        after.contains(" Fixture        |     | 2.0.0-beta.1       | 2.0.0-beta.1"),
        "the surviving rows are byte-identical: {after}"
    );
}

/// `sdk list maven` — the grid layout: a blocked version is one blanked cell.
#[actix_web::test]
async fn a_blocked_version_is_blanked_in_the_rendered_grid() {
    let app = app().await;
    let path = "candidates/maven/linuxx64/versions/list?current=&installed=";
    block_version(&app, REG, "maven", "1.1.0").await;
    let after = get_text(&app, &url(path)).await;
    assert!(
        after.contains("     2.0.0-beta.1                            1.0.0"),
        "{after}"
    );
    assert!(!after.contains("1.1.0"), "{after}");
}

/// The list is a 400 upstream without its query; the handler forwards what
/// the client sent, and the fixture echoes the package string it received.
#[actix_web::test]
async fn versions_list_forwards_the_query_and_does_not_400() {
    let app = app().await;
    let body = get_text(
        &app,
        &url("candidates/maven/linuxx64/versions/list?current=1.1.0&installed=1.1.0,1.0.0"),
    )
    .await;
    assert!(
        body.starts_with("# maven/linuxx64?current=1.1.0&installed=1.1.0,1.0.0\n"),
        "{body}"
    );
    // The bare form the client never sends still answers rather than 400s.
    let resp = call_service(
        &app,
        admin_get(&url("candidates/maven/linuxx64/versions/list")),
    )
    .await;
    assert_eq!(resp.status(), 200);
}

/// Two clients with different installed sets are two cache entries.
#[actix_web::test]
async fn versions_list_is_cached_per_query() {
    let app = app().await;
    let a = get_text(
        &app,
        &url("candidates/maven/linuxx64/versions/list?current=1.0.0&installed=1.0.0"),
    )
    .await;
    let b = get_text(
        &app,
        &url("candidates/maven/linuxx64/versions/list?current=1.1.0&installed=1.1.0"),
    )
    .await;
    assert_ne!(a, b);
    assert!(a.contains("current=1.0.0"));
    assert!(b.contains("current=1.1.0"));
}

#[actix_web::test]
async fn a_malformed_list_query_is_refused_at_the_edge() {
    let app = app().await;
    let resp = call_service(
        &app,
        admin_get(&url(
            "candidates/maven/linuxx64/versions/list?installed=..%2Fetc",
        )),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

// ── the relayed documents ────────────────────────────────────────────────────

#[actix_web::test]
async fn relayed_documents_cross_byte_exact_as_text() {
    let app = app().await;
    assert_eq!(
        get_text(&app, &url("candidates/all")).await,
        "fixture,java,maven"
    );
    assert_eq!(
        get_text(&app, &url("healthcheck")).await,
        "000000000000000000000000"
    );
    assert_content_type(&app, &url("healthcheck"), "text/plain").await;
    let hook = get_text(&app, &url("hooks/post/java/1.1.0/linuxx64")).await;
    assert_eq!(
        hook,
        "#!/bin/bash\n#Hook: hooks/post/java/1.1.0/linuxx64\nfunction __sdkman_post_installation_hook { :; }\n"
    );
    assert_eq!(
        get_text(&app, &url("broker/version/sdkman/script/stable")).await,
        "relayed:broker/version/sdkman/script/stable"
    );
    assert_eq!(
        get_text(&app, &url("selfupdate/stable/linuxx64")).await,
        "relayed:selfupdate/stable/linuxx64"
    );
}

/// A block does not touch the hook of a blocked version: the download it
/// follows is refused, the script is not rewritten (RFC 0010 §7).
#[actix_web::test]
async fn a_hook_is_relayed_unchanged_for_a_blocked_version() {
    let app = app().await;
    block_version(&app, REG, "java", "1.1.0").await;
    let hook = get_text(&app, &url("hooks/post/java/1.1.0/linuxx64")).await;
    assert!(hook.contains("#Hook: hooks/post/java/1.1.0/linuxx64"));
}

#[actix_web::test]
async fn an_unknown_phase_component_or_channel_is_not_found() {
    let app = app().await;
    for path in [
        "hooks/mid/java/1.1.0/linuxx64",
        "broker/version/sdkman/binary/stable",
        "broker/version/sdkman/script/nightly",
        "selfupdate/nightly/linuxx64",
    ] {
        let resp = call_service(&app, admin_get(&url(path))).await;
        assert_eq!(resp.status(), 404, "{path}");
    }
}

// ── the broker ───────────────────────────────────────────────────────────────

#[actix_web::test]
async fn a_download_is_served_as_bytes_under_its_candidate_version_platform_key() {
    let app = app().await;
    let path = url("broker/download/java/1.1.0/linuxx64");
    assert_content_type(&app, &path, "application/octet-stream").await;
    assert_eq!(
        get_text(&app, &path).await,
        "artifact:sdkman:jvm/java/1.1.0/linuxx64"
    );
}

/// The second build agent to ask for a JDK does not leave the site.
#[actix_web::test]
async fn a_download_is_cached_under_a_stable_key() {
    let parts = local_registry_app_parts(REG, "sdkman", RegistryMode::Proxy, None);
    let storage = Arc::clone(&parts.proxy_svc.storage);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let pkg =
        batlehub_core::entities::PackageId::new(REG, "java", "1.1.0").with_artifact("linuxx64");
    let key = proxy_artifact_key(&pkg);
    assert_eq!(key, "artifact:jvm/java/1.1.0/linuxx64");
    assert!(!storage.exists(&key).await.unwrap());

    get_bytes(&app, &url("broker/download/java/1.1.0/linuxx64")).await;
    assert!(
        storage.exists(&key).await.unwrap(),
        "the first read must cache {key}"
    );
    get_bytes(&app, &url("broker/download/java/1.1.0/linuxx64")).await;
}

/// Hiding governs resolution, not diagnosis: a direct request for a blocked
/// version — from an `.sdkmanrc`, or memory — gets the operator's `403`.
#[actix_web::test]
async fn a_direct_download_of_a_blocked_version_is_denied_on_every_platform() {
    let app = app().await;
    block_version(&app, REG, "java", "1.1.0").await;
    for platform in ["linuxx64", "darwinarm64"] {
        let resp = call_service(
            &app,
            admin_get(&url(&format!("broker/download/java/1.1.0/{platform}"))),
        )
        .await;
        assert_eq!(resp.status(), 403, "{platform}");
    }
    let resp = call_service(&app, admin_get(&url("broker/download/java/1.0.0/linuxx64"))).await;
    assert_eq!(resp.status(), 200);
}

// ── edge validation ──────────────────────────────────────────────────────────

/// Candidate, version and platform all reach a storage key; each is refused
/// at the edge with a clean `400` — the regression every registry kind carries.
#[actix_web::test]
async fn sdkman_download_traversal_version_returns_400() {
    let app = app().await;
    for path in [
        "broker/download/java/..%2Fx/linuxx64",
        "broker/download/..%2Fx/1.1.0/linuxx64",
        "broker/download/java/1.1.0/..%2Fx",
        "broker/download/java/1.1.0/plan9",
        "candidates/validate/java/..%2Fx/linuxx64",
        "candidates/validate/java/1.1.0/plan9",
        "candidates/java/plan9/versions/all",
        "candidates/..%2Fx/linuxx64/versions/all",
        "hooks/post/java/..%2Fx/linuxx64",
        "selfupdate/stable/plan9",
        // Double-encoded: `%252e%252e` reaches the handler as `%2e%2e`, which
        // a byte comparison does not read as a dot segment but a URL parser
        // does.
        "broker/download/java/%252e%252e/linuxx64",
        "broker/download/java/1.1.0/%252e%252e",
        "candidates/validate/java/..%252Fx/linuxx64",
    ] {
        let resp = call_service(&app, admin_get(&url(path))).await;
        assert_eq!(resp.status(), 400, "{path}");
    }
    for path in [
        "broker/download/../1.1.0/linuxx64",
        "broker/download/%2e%2e/1.1.0/linuxx64",
    ] {
        let resp = call_service(&app, admin_get(&url(path))).await;
        assert!(
            matches!(resp.status().as_u16(), 400 | 404),
            "{path} answered {}",
            resp.status()
        );
    }
}

#[actix_web::test]
async fn the_routes_refuse_a_registry_of_another_kind() {
    let app = proxy_registry_app("npm-reg", "npm").await;
    for path in [
        "candidates/all",
        "healthcheck",
        "broker/download/java/1.1.0/linuxx64",
    ] {
        let resp = call_service(&app, admin_get(&format!("/proxy/npm-reg/sdkman/{path}"))).await;
        assert_eq!(resp.status(), 404, "{path}");
    }
}

/// The literal routes register before the `{candidate}/…` ones, so a
/// candidate that happens to be called `default` or `validate` cannot shadow
/// them — and the reverse: `candidates/default/java` is the default document,
/// not a listing for a candidate called `default`.
#[actix_web::test]
async fn literal_routes_win_over_a_candidate_of_the_same_name() {
    let app = app().await;
    let req = TestRequest::get()
        .uri(&url("candidates/default/java"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.request().match_pattern().as_deref(),
        Some("/proxy/{registry}/sdkman/candidates/default/{candidate}")
    );
}
