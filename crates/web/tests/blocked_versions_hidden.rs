//! Blocking a version hides it from version *listings*, not just from downloads.
//!
//! A package manager picks a version by reading a listing — an npm packument —
//! and only then downloads it. Denying the download without editing the listing
//! makes the resolver choose the blocked version and fail, so a block reads as
//! breakage rather than policy. These tests pin the other half: the blocked
//! version is absent from what clients resolve against, `dist-tags.latest`
//! moves back to the newest allowed version, and a direct request for the
//! blocked version still gets the operator's `403` and reason.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body_json, TestRequest};
use serde_json::Value;

use base64::Engine as _;
use batlehub_config::schema::RegistryMode;

async fn app(mode: RegistryMode) -> impl TestService {
    registry_app("local-npm", "npm", mode).await
}

fn npm_publish_payload(name: &str, version: &str) -> serde_json::Value {
    let tarball_b64 = base64::engine::general_purpose::STANDARD.encode(b"fake-tarball-content");
    serde_json::json!({
        "name": name,
        "versions": {
            version: { "name": name, "version": version, "dist": { "tarball": "" } }
        },
        "_attachments": {
            format!("{name}-{version}.tgz"): { "data": tarball_b64 }
        }
    })
}

fn version_keys(doc: &Value) -> Vec<String> {
    doc["versions"]
        .as_object()
        .expect("packument has a versions map")
        .keys()
        .cloned()
        .collect()
}

// ── Proxy mode ───────────────────────────────────────────────────────────────

/// The case from the brief, on a proxied registry: block the version `latest`
/// points at, and `latest` must fall back to the newest allowed stable version
/// rather than continuing to name the blocked one.
#[actix_web::test]
async fn proxy_packument_hides_blocked_version_and_repoints_latest() {
    let app = app(RegistryMode::Proxy).await;

    // Before: upstream's own view, unfiltered. This first read also warms the
    // document cache, which is the point — the block below must take effect on
    // the very next request, not when the cached copy expires. That only holds
    // because what gets cached is the raw upstream document, with filtering
    // applied on the way out.
    let before: Value = get_json(&app, "/proxy/local-npm/lodash").await;
    assert_eq!(before["dist-tags"]["latest"], "1.1.0");
    assert!(version_keys(&before).contains(&"1.1.0".to_owned()));

    block_version(&app, "local-npm", "lodash", "1.1.0").await;

    let after: Value = get_json(&app, "/proxy/local-npm/lodash").await;

    let versions = version_keys(&after);
    assert!(
        !versions.contains(&"1.1.0".to_owned()),
        "blocked version still listed: {versions:?}"
    );
    assert!(versions.contains(&"1.0.0".to_owned()));
    assert_eq!(
        after["dist-tags"]["latest"], "1.0.0",
        "latest must move to the newest allowed stable version"
    );
    // The per-version timestamp goes with it, or the document contradicts itself.
    assert!(after["time"].get("1.1.0").is_none());
    assert!(after["time"].get("created").is_some());
}

/// A packument must be JSON. Before this existed the proxy fall-through streamed
/// `fetch_artifact` for this route, answering npm's metadata request with the
/// `latest` tarball as `application/octet-stream`.
#[actix_web::test]
async fn proxy_packument_is_json_not_a_tarball() {
    let app = app(RegistryMode::Proxy).await;

    assert_content_type(&app, "/proxy/local-npm/lodash", "application/json").await;

    let doc: Value = get_json(&app, "/proxy/local-npm/lodash").await;
    assert_eq!(doc["name"], "lodash");
    assert!(doc["versions"].is_object());
}

/// Tarball URLs must point back at this proxy. Served with the upstream's own
/// URLs, every download would route around the proxy — past its cache, its
/// audit trail, and the download-time block gate.
#[actix_web::test]
async fn proxy_packument_rewrites_tarball_urls_to_this_host() {
    let app = app(RegistryMode::Proxy).await;

    let doc: Value = get_json(&app, "/proxy/local-npm/lodash").await;

    for v in ["1.0.0", "1.1.0"] {
        let url = doc["versions"][v]["dist"]["tarball"]
            .as_str()
            .unwrap_or_default();
        assert!(
            !url.contains("upstream.invalid"),
            "{v} still points upstream: {url}"
        );
        assert!(
            url.ends_with(&format!("/proxy/local-npm/lodash/{v}/tarball")),
            "{v} tarball URL was {url}"
        );
    }
}

/// A non-`latest` dist-tag naming a blocked version is dropped rather than
/// silently repointed: a tag labels one specific release, and moving it would
/// misrepresent what the publisher tagged.
#[actix_web::test]
async fn proxy_packument_drops_other_tags_pointing_at_a_blocked_version() {
    let app = app(RegistryMode::Proxy).await;

    block_version(&app, "local-npm", "lodash", "2.0.0-beta.1").await;

    let doc: Value = get_json(&app, "/proxy/local-npm/lodash").await;

    assert!(doc["dist-tags"].get("next").is_none(), "stale tag survived");
    assert_eq!(
        doc["dist-tags"]["latest"], "1.1.0",
        "latest was not pointing at the blocked version, so it should not move"
    );
}

/// Blocking one package must not disturb another's listing.
#[actix_web::test]
async fn proxy_packument_of_another_package_is_untouched() {
    let app = app(RegistryMode::Proxy).await;

    block_version(&app, "local-npm", "lodash", "1.1.0").await;

    let doc: Value = get_json(&app, "/proxy/local-npm/express").await;

    assert_eq!(doc["dist-tags"]["latest"], "1.1.0");
    assert!(version_keys(&doc).contains(&"1.1.0".to_owned()));
}

/// Hiding governs resolution, not diagnosis: an explicit request for the blocked
/// version still gets 403 and the reason the operator recorded.
#[actix_web::test]
async fn proxy_direct_request_for_a_blocked_version_is_still_denied() {
    let app = app(RegistryMode::Proxy).await;

    block_version(&app, "local-npm", "lodash", "1.1.0").await;

    let req = admin_get("/proxy/local-npm/lodash/1.1.0/tarball");
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

// ── Local mode ───────────────────────────────────────────────────────────────

/// The same guarantee for a privately hosted package: the block store is shared,
/// so a locally published version disappears from its own packument too.
#[actix_web::test]
async fn local_packument_hides_blocked_version_and_repoints_latest() {
    let app = app(RegistryMode::Local).await;

    for v in ["1.0.0", "1.1.0"] {
        let req = TestRequest::put()
            .uri("/proxy/local-npm/inhouse")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(npm_publish_payload("inhouse", v))
            .to_request();
        let resp = call_service(&app, req).await;
        assert!(resp.status().is_success(), "publish {v}: {}", resp.status());
    }

    let before: Value = get_json(&app, "/proxy/local-npm/inhouse").await;
    assert_eq!(before["dist-tags"]["latest"], "1.1.0");

    block_version(&app, "local-npm", "inhouse", "1.1.0").await;

    let after: Value = get_json(&app, "/proxy/local-npm/inhouse").await;

    let versions = version_keys(&after);
    assert!(
        !versions.contains(&"1.1.0".to_owned()),
        "blocked version still listed: {versions:?}"
    );
    assert_eq!(after["dist-tags"]["latest"], "1.0.0");
}

/// Blocking every version leaves the package with nothing to resolve to. The
/// listing endpoint refuses it rather than serving an empty document that a
/// client would read as "exists, but broken".
///
/// `403`, not `404`, and the difference is load-bearing — see
/// `hybrid_packument_does_not_fall_through_to_upstream_when_every_version_is_blocked`
/// below. It also matches what a direct request for a blocked version already
/// answers, so the two halves of a block agree.
#[actix_web::test]
async fn local_packument_is_denied_when_every_version_is_blocked() {
    let app = app(RegistryMode::Local).await;

    let req = TestRequest::put()
        .uri("/proxy/local-npm/inhouse")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(npm_publish_payload("inhouse", "1.0.0"))
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());

    block_version(&app, "local-npm", "inhouse", "1.0.0").await;

    let req = admin_get("/proxy/local-npm/inhouse");
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

/// A package that was never published here is still `404`, so genuine hybrid
/// fall-through to upstream is untouched by the rule above.
#[actix_web::test]
async fn local_packument_is_not_found_when_never_published() {
    let app = app(RegistryMode::Local).await;

    let req = admin_get("/proxy/local-npm/never-published");
    assert_eq!(call_service(&app, req).await.status(), 404);
}

/// SECURITY: on a Hybrid registry, "we hold nothing for this name" falls through
/// to upstream. If an all-blocked package reported *that*, blocking every
/// version of an internal `lodash` would make this proxy answer with the public
/// npmjs `lodash` in its place — the substitution a block exists to prevent, and
/// a dependency-confusion vector built out of an operator's own control.
///
/// It must refuse instead, and must not serve the upstream document.
#[actix_web::test]
async fn hybrid_packument_does_not_fall_through_to_upstream_when_every_version_is_blocked() {
    let app = app(RegistryMode::Hybrid).await;

    // `lodash` is a name the upstream fixture also serves — that overlap is the
    // whole point of the test.
    let req = TestRequest::put()
        .uri("/proxy/local-npm/lodash")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(npm_publish_payload("lodash", "9.9.9"))
        .to_request();
    assert!(call_service(&app, req).await.status().is_success());

    block_version(&app, "local-npm", "lodash", "9.9.9").await;

    let req = admin_get("/proxy/local-npm/lodash");
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        403,
        "an all-blocked local package must be refused, never replaced by upstream's"
    );
}

/// The other side of the rule above, and the one it is easiest to break: blocks
/// are recorded per registry + name + version and cover **proxied** versions
/// too, so "this name has a blocked version" is not "this name is ours and is
/// entirely blocked".
///
/// Blocking one bad version of a purely upstream package — nothing of it ever
/// published here — must still serve upstream's document with that version
/// stripped. Refusing it would turn a routine CVE block on a popular proxied
/// package into a `403` for the whole package, and every `npm install` naming
/// any version of it would fail.
#[actix_web::test]
async fn hybrid_packument_falls_through_when_only_a_proxied_version_is_blocked() {
    let app = app(RegistryMode::Hybrid).await;

    // Nothing is published locally: `lodash` exists only upstream.
    block_version(&app, "local-npm", "lodash", "1.1.0").await;

    let req = admin_get("/proxy/local-npm/lodash");
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        200,
        "a purely proxied package must still resolve when one of its versions is blocked"
    );

    let doc: Value = read_body_json(resp).await;
    let versions = version_keys(&doc);
    assert!(
        !versions.contains(&"1.1.0".to_owned()),
        "the blocked version must be hidden: {versions:?}"
    );
    assert!(
        versions.contains(&"1.0.0".to_owned()),
        "the allowed versions must survive: {versions:?}"
    );
}

/// The package page draws a blocked local version as blocked.
///
/// The block is stored on the version's admin summary, which the detail merge
/// skipped for a version also published here: the page called the version
/// clear while its download answered `403` and the catalogue said blocked.
#[actix_web::test]
async fn explore_detail_shows_a_blocked_local_version_as_blocked() {
    let app = app(RegistryMode::Local).await;
    for v in ["1.0.0", "1.1.0"] {
        let req = TestRequest::put()
            .uri("/proxy/local-npm/inhouse")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(npm_publish_payload("inhouse", v))
            .to_request();
        assert!(call_service(&app, req).await.status().is_success());
    }
    block_version(&app, "local-npm", "inhouse", "1.0.0").await;

    let req = TestRequest::get()
        .uri("/api/v1/explore/packages/local-npm/inhouse")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let detail: Value = read_body_json(resp).await;
    let status = |v: &str| {
        detail["versions"]
            .as_array()
            .expect("versions")
            .iter()
            .find(|row| row["version"] == v)
            .unwrap_or_else(|| panic!("no row for {v}: {detail}"))["firewall"]["status"]
            .clone()
    };
    assert_eq!(status("1.0.0"), "blocked");
    assert_eq!(status("1.1.0"), "clear");
}
