//! mise's versions host as a registry kind (RFC 0043).
//!
//! The real `MiseVersionsRegistryClient` against a mock jdx.dev, because the
//! document is TOML the client relays verbatim and every claim below is about
//! what reaches mise: the version list without blocked or too-young entries,
//! the beacon answered here and sent nowhere, the GitHub mirror refused with
//! the one status mise retries elsewhere, and an air-gapped listing that is a
//! `404` rather than a recorded miss.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, read_body, TestRequest};
use batlehub_adapters::registry::MiseVersionsRegistryClient;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    ports::RegistryClient,
    rules::{BlockListRule, ReleaseAgeGateRule},
    services::RegistryPolicy,
};

const REG: &str = "mv";

const JQ: &str = "[versions]\n\
\"1.8.1\" = { created_at = 2025-07-01T11:50:06.000Z, release_url = \"https://github.com/jqlang/jq/releases/tag/jq-1.8.1\" }\n\
\"1.8.2\" = { created_at = 2099-01-01T00:00:00.000Z, release_url = \"https://github.com/jqlang/jq/releases/tag/jq-1.8.2\" }\n";

/// The app, its mock upstream, and how many requests reached the upstream.
struct Lab<S> {
    app: S,
    upstream: mockito::ServerGuard,
}

async fn lab(age_gate: Option<Duration>, air_gap: bool) -> Lab<impl TestService> {
    lab_with(age_gate, air_gap, false).await
}

async fn lab_with(
    age_gate: Option<Duration>,
    air_gap: bool,
    closed: bool,
) -> Lab<impl TestService> {
    let mut upstream = mockito::Server::new_async().await;
    upstream
        .mock("GET", "/data/jq.toml")
        .with_header("content-type", "application/toml")
        .with_body(JQ)
        .create_async()
        .await;

    let parts = local_registry_app_parts(REG, "mise-versions", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(MiseVersionsRegistryClient::new(upstream.url(), &Default::default()).unwrap())
                as Arc<dyn RegistryClient>,
        );
        let mut rules: Vec<Box<dyn batlehub_core::rules::Rule>> = vec![Box::new(
            BlockListRule::new(Arc::clone(&parts.proxy_svc.repo)),
        )];
        if let Some(min_age) = age_gate {
            rules.push(Box::new(ReleaseAgeGateRule::new(min_age, vec![])));
        }
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules,
            }),
        );
        hot.air_gap.enabled = air_gap;
        if closed {
            hot.grants = [(
                REG.to_owned(),
                Arc::new(batlehub_core::entities::RegistryGrants::empty(
                    REG,
                    batlehub_core::entities::RegistryKind::MiseVersions,
                )),
            )]
            .into();
        }
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    Lab { app, upstream }
}

fn versions_in(toml_text: &str) -> Vec<String> {
    let t: toml::Table = toml::from_str(toml_text).expect("the served list parses as TOML");
    t["versions"].as_table().unwrap().keys().cloned().collect()
}

// ── the version list ─────────────────────────────────────────────────────────

#[actix_web::test]
async fn the_version_list_is_relayed_as_toml() {
    let lab = lab(None, false).await;
    let resp = call_service(
        &lab.app,
        admin_get(&format!("/proxy/{REG}/mise-versions/data/jq.toml")),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/toml"
    );
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert_eq!(body, JQ, "nothing blocked, no gate: byte-exact");
}

#[actix_web::test]
async fn a_blocked_version_is_absent_from_both_spellings_of_the_list() {
    let lab = lab(None, false).await;
    block_version(&lab.app, REG, "jq", "1.8.2").await;
    for path in ["data", "tools"] {
        let body = get_text(
            &lab.app,
            &format!("/proxy/{REG}/mise-versions/{path}/jq.toml"),
        )
        .await;
        assert_eq!(versions_in(&body), ["1.8.1"], "{path}: {body}");
    }
}

/// RFC 0043 decision 2: the one listing an age gate filters.
#[actix_web::test]
async fn an_age_gate_hides_the_versions_younger_than_it() {
    let lab = lab(Some(Duration::from_secs(86_400)), false).await;
    let body = get_text(
        &lab.app,
        &format!("/proxy/{REG}/mise-versions/data/jq.toml"),
    )
    .await;
    assert_eq!(versions_in(&body), ["1.8.1"], "1.8.2 is dated 2099: {body}");
}

#[actix_web::test]
async fn an_unsafe_tool_name_is_refused_and_an_unknown_file_is_not_found() {
    let lab = lab(None, false).await;
    let resp = call_service(
        &lab.app,
        admin_get(&format!("/proxy/{REG}/mise-versions/data/..toml")),
    )
    .await;
    assert_eq!(resp.status(), 400);
    let resp = call_service(
        &lab.app,
        admin_get(&format!("/proxy/{REG}/mise-versions/data/jq.json")),
    )
    .await;
    assert_eq!(resp.status(), 404);
    let resp = call_service(
        &lab.app,
        admin_get(&format!("/proxy/{REG}/mise-versions/jq")),
    )
    .await;
    assert_eq!(
        resp.status(),
        404,
        "mise ≤ 2025.12's path is the scope's 404"
    );
}

// ── the beacon ───────────────────────────────────────────────────────────────

#[actix_web::test]
async fn every_beacon_shape_is_absorbed_and_nothing_reaches_upstream() {
    let mut lab = lab(None, false).await;
    let forwarded = lab
        .upstream
        .mock("ANY", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let auth = ("Authorization", bearer(ADMIN_TOKEN));
    for req in [
        TestRequest::get()
            .uri(&format!(
                "/proxy/{REG}/mise-versions/t?tool=jq&full=aqua%3Ajqlang%2Fjq&version=1.8.1&os=linux&arch=x64"
            ))
            .insert_header(auth.clone()),
        TestRequest::post()
            .uri(&format!("/proxy/{REG}/mise-versions/api/tools/jq"))
            .insert_header(auth.clone())
            .set_json(serde_json::json!({ "version": "1.8.1" })),
        TestRequest::post()
            .uri(&format!("/proxy/{REG}/mise-versions/api/track"))
            .insert_header(auth.clone())
            .set_json(serde_json::json!({ "tool": "jq" })),
    ] {
        let resp = call_service(&lab.app, req.to_request()).await;
        assert_eq!(resp.status(), 204);
        assert!(read_body(resp).await.is_empty());
    }
    forwarded.assert_async().await;
}

// ── the GitHub mirror ────────────────────────────────────────────────────────

/// RFC 0043 decision 5: `503`, because mise trusts a `404` as "no such release".
#[actix_web::test]
async fn the_github_mirror_is_a_503_never_a_404() {
    let lab = lab(None, false).await;
    for path in [
        "api/github/repos/jqlang/jq/releases/jq-1.8.1",
        "api/github/repos/jqlang/jq/releases?page=1",
        "data/github/jqlang/jq/releases.json",
    ] {
        let resp = call_service(
            &lab.app,
            admin_get(&format!("/proxy/{REG}/mise-versions/{path}")),
        )
        .await;
        assert_eq!(resp.status(), 503, "{path}");
    }
}

// ── the python index ─────────────────────────────────────────────────────────

#[actix_web::test]
async fn the_python_index_is_relayed_byte_exact() {
    // The index is a passthrough on the registry's configured upstream, so this
    // app's upstream map names the mock — `lab` leaves it at the default host.
    let mut upstream = mockito::Server::new_async().await;
    let index = upstream
        .mock(
            "GET",
            "/data/python-precompiled-x86_64-unknown-linux-gnu.gz",
        )
        .with_header("content-type", "application/gzip")
        .with_body([0x1f, 0x8b, 0x08, 0x00])
        .expect(1)
        .create_async()
        .await;
    let app = upstream_forwarding_app(REG, "mise-versions", upstream.url(), rbac_policy).await;
    let resp = call_service(
        &app,
        admin_get(&format!(
            "/proxy/{REG}/mise-versions/data/python-precompiled-x86_64-unknown-linux-gnu.gz"
        )),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await.as_ref(), [0x1f, 0x8b, 0x08, 0x00]);
    // A second read is the passthrough cache's, not the upstream's.
    let again = call_service(
        &app,
        admin_get(&format!(
            "/proxy/{REG}/mise-versions/data/python-precompiled-x86_64-unknown-linux-gnu.gz"
        )),
    )
    .await;
    assert_eq!(again.status(), 200);
    index.assert_async().await;
}

// ── the air gap ──────────────────────────────────────────────────────────────

/// RFC 0043 §5.3: mise reads `404` as "use the backend's own list".
#[actix_web::test]
async fn an_air_gapped_listing_is_a_404_and_never_dials_out() {
    let mut lab = lab(None, true).await;
    let forwarded = lab
        .upstream
        .mock("ANY", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    for path in ["data/jq.toml", "tools/jq.toml"] {
        let resp = call_service(
            &lab.app,
            admin_get(&format!("/proxy/{REG}/mise-versions/{path}")),
        )
        .await;
        assert_eq!(resp.status(), 404, "{path}");
    }
    let resp = call_service(
        &lab.app,
        admin_get(&format!(
            "/proxy/{REG}/mise-versions/data/python-precompiled-x86_64-unknown-linux-gnu.gz"
        )),
    )
    .await;
    assert!(
        resp.status().is_server_error(),
        "unheld index: {}",
        resp.status()
    );
    forwarded.assert_async().await;
}

#[actix_web::test]
async fn the_routes_refuse_a_registry_of_another_kind() {
    let app = proxy_registry_app("npm-reg", "npm").await;
    let resp = call_service(&app, admin_get("/proxy/npm-reg/mise-versions/data/jq.toml")).await;
    assert_ne!(
        resp.request().match_pattern().as_deref(),
        Some("/proxy/{registry}/mise-versions/data/{file}")
    );
}

/// A closed registry stays closed on every route of this kind, the beacon and
/// the mirror included: `RegistryGrants::empty()` grants nobody anything.
#[actix_web::test]
async fn a_closed_registry_refuses_every_route() {
    let lab = lab_with(None, false, true).await;
    let app = lab.app;
    for (method, path) in [
        ("GET", "data/jq.toml"),
        ("GET", "tools/jq.toml"),
        ("GET", "t?tool=jq"),
        ("POST", "api/tools/jq"),
        ("POST", "api/track"),
        ("GET", "api/github/repos/a/b/releases/v1"),
        ("GET", "data/python-precompiled-x86_64-unknown-linux-gnu.gz"),
    ] {
        let req = match method {
            "POST" => TestRequest::post(),
            _ => TestRequest::get(),
        }
        .uri(&format!("/proxy/{REG}/mise-versions/{path}"))
        .insert_header(("Authorization", bearer(USER_TOKEN)));
        let resp = call_service(&app, req.to_request()).await;
        assert_eq!(resp.status(), 403, "{method} {path}");
    }
}
