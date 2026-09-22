//! Devfile registries (RFC 0035), against the real `DevfileRegistryClient`
//! pointed at a mock `registry.devfile.io`.
//!
//! What the two clients do, and what this proxy therefore has to get right:
//!
//! - Che's dashboard reads `index/all` and follows each tile to
//!   `devfiles/{stack}/{version}` — so a blocked version has to be gone from
//!   the legacy index, and its devfile refused;
//! - `registry-library` reads `v2index`, then `HEAD`s the manifest by tag and
//!   fetches every layer by digest, checking each against the manifest — so
//!   the manifest is byte-exact, its `HEAD` carries the real digest and length,
//!   and a digest of a blocked version is not reachable at all (§5.2).
//!
//! Blocks are placed before the first index read in each test: the index is a
//! registry-wide document, filtered against a blocked-set snapshot with a
//! 30-second lifetime, exactly as conda's `repodata.json` is.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::http::Method;
use actix_web::test::{call_service, read_body, TestRequest};
use batlehub_adapters::registry::DevfileRegistryClient;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    ports::RegistryClient,
    rules::{BlockListRule, DenyLatestRule, Rule},
    services::RegistryPolicy,
};
use sha2::{Digest, Sha256};

const REG: &str = "devfile";

const DEVFILE_221: &[u8] = b"schemaVersion: 2.2.2\nmetadata:\n  name: nodejs\n  version: 2.2.1\n";
const DEVFILE_220: &[u8] = b"schemaVersion: 2.1.0\nmetadata:\n  name: nodejs\n  version: 2.2.0\n";
const ARCHIVE: &[u8] = b"a tar archive, as far as this test cares";
const STARTER: &[u8] = b"PK\x03\x04 a zip";

fn sha(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn manifest(devfile: &[u8]) -> String {
    serde_json::json!({
        "schemaVersion": 2,
        "config": {"mediaType": "application/vnd.devfileio.devfile.config.v2+json",
                   "digest": sha(b"{}"), "size": 2},
        "layers": [
            {"mediaType": "application/x-tar", "digest": sha(ARCHIVE), "size": ARCHIVE.len(),
             "annotations": {"org.opencontainers.image.title": "archive.tar"}},
            {"mediaType": "application/vnd.devfileio.devfile.layer.v1", "digest": sha(devfile),
             "size": devfile.len(), "annotations": {"org.opencontainers.image.title": "devfile.yaml"}}
        ]
    })
    .to_string()
}

fn v2index() -> serde_json::Value {
    serde_json::json!([
        {"name": "nodejs", "type": "stack", "versions": [
            {"version": "2.2.1", "default": true, "links": {"self": "devfile-catalog/nodejs:2.2.1"},
             "starterProjects": ["nodejs-starter"]},
            {"version": "2.2.0", "links": {"self": "devfile-catalog/nodejs:2.2.0"},
             "starterProjects": ["nodejs-starter"]}
        ]},
        {"name": "nodejs-basic", "type": "sample", "git": {"remotes": {"origin": "https://example.invalid/x.git"}}}
    ])
}

fn legacy_index() -> serde_json::Value {
    serde_json::json!([
        {"name": "nodejs", "type": "stack", "version": "2.2.1",
         "links": {"self": "devfile-catalog/nodejs:2.2.1"}},
        {"name": "nodejs-basic", "type": "sample"}
    ])
}

/// A mock upstream serving both versions of `nodejs`, with `devfile_221`
/// standing in for the 2.2.1 devfile *layer* (to alter it in one test).
async fn upstream(devfile_221_layer: &[u8]) -> mockito::ServerGuard {
    let mut s = mockito::Server::new_async().await;
    for path in ["/v2index", "/v2index/all"] {
        s.mock("GET", path)
            .with_body(v2index().to_string())
            .create_async()
            .await;
    }
    s.mock("GET", "/index/all")
        .with_body(legacy_index().to_string())
        .create_async()
        .await;
    for (version, devfile) in [("2.2.1", DEVFILE_221), ("2.2.0", DEVFILE_220)] {
        let m = manifest(devfile);
        s.mock(
            "GET",
            format!("/v2/devfile-catalog/nodejs/manifests/{version}").as_str(),
        )
        .with_header("docker-content-digest", &sha(m.as_bytes()))
        .with_body(m)
        .create_async()
        .await;
    }
    s.mock(
        "GET",
        format!("/v2/devfile-catalog/nodejs/blobs/{}", sha(DEVFILE_221)).as_str(),
    )
    .with_body(devfile_221_layer)
    .create_async()
    .await;
    s.mock(
        "GET",
        format!("/v2/devfile-catalog/nodejs/blobs/{}", sha(DEVFILE_220)).as_str(),
    )
    .with_body(DEVFILE_220)
    .create_async()
    .await;
    s.mock(
        "GET",
        format!("/v2/devfile-catalog/nodejs/blobs/{}", sha(ARCHIVE)).as_str(),
    )
    .with_body(ARCHIVE)
    .create_async()
    .await;
    s.mock(
        "GET",
        "/devfiles/nodejs/2.2.1/starter-projects/nodejs-starter",
    )
    .with_status(202)
    .with_body(STARTER)
    .create_async()
    .await;
    s
}

async fn app_against(base: String, deny_latest: bool) -> impl TestService {
    let parts = local_registry_app_parts(REG, "devfile", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(DevfileRegistryClient::new(base, &Default::default()).unwrap())
                as Arc<dyn RegistryClient>,
        );
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: {
                    let mut rules: Vec<Box<dyn Rule>> = vec![Box::new(BlockListRule::new(
                        Arc::clone(&parts.proxy_svc.repo),
                    ))];
                    if deny_latest {
                        rules.push(Box::new(DenyLatestRule::new(vec![])));
                    }
                    rules
                },
            }),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

async fn app() -> (mockito::ServerGuard, impl TestService) {
    let server = upstream(DEVFILE_221).await;
    let app = app_against(server.url(), false).await;
    (server, app)
}

fn admin(method: Method, uri: &str) -> actix_http::Request {
    TestRequest::default()
        .method(method)
        .uri(uri)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request()
}

async fn status_and_body<S: TestService>(app: &S, uri: &str) -> (u16, Vec<u8>) {
    let resp = call_service(app, admin(Method::GET, uri)).await;
    let status = resp.status().as_u16();
    (status, read_body(resp).await.to_vec())
}

fn oci_code(body: &[u8]) -> String {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    v.pointer("/errors/0/code")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn versions_of(index: &serde_json::Value, stack: &str) -> Vec<String> {
    index
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == stack)
        .map(|e| {
            e["versions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["version"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

// ── the index is filtered registry-wide ──────────────────────────────────────

#[actix_web::test]
async fn the_v2_index_is_upstreams_when_nothing_is_blocked() {
    let (_s, app) = app().await;
    let index = get_json(&app, &format!("/proxy/{REG}/v2index")).await;
    assert_eq!(index, v2index());
}

#[actix_web::test]
async fn a_blocked_default_leaves_the_v2_index_and_the_default_moves() {
    let (_s, app) = app().await;
    block_version(&app, REG, "nodejs", "2.2.1").await;
    let index = get_json(&app, &format!("/proxy/{REG}/v2index/all")).await;
    assert_eq!(versions_of(&index, "nodejs"), ["2.2.0"]);
    assert_eq!(index[0]["versions"][0]["default"], true);
    assert_eq!(
        index[1]["name"], "nodejs-basic",
        "samples are never filtered"
    );
}

#[actix_web::test]
async fn a_blocked_default_takes_its_stack_out_of_the_legacy_index_che_reads() {
    let (_s, app) = app().await;
    block_version(&app, REG, "nodejs", "2.2.1").await;
    let index = get_json(&app, &format!("/proxy/{REG}/index/all")).await;
    let names: Vec<&str> = index
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["nodejs-basic"]);
}

#[actix_web::test]
async fn a_query_upstream_would_refuse_is_a_400_and_unknown_parameters_are_dropped() {
    let (mut s, app) = app().await;
    let (status, _) =
        status_and_body(&app, &format!("/proxy/{REG}/v2index?minSchemaVersion=zz")).await;
    assert_eq!(status, 400);

    let filtered = s
        .mock("GET", "/v2index?arch=amd64")
        .with_body("[]")
        .expect(1)
        .create_async()
        .await;
    let index = get_json(&app, &format!("/proxy/{REG}/v2index?foo=bar&arch=amd64")).await;
    assert_eq!(index, serde_json::json!([]));
    filtered.assert_async().await;
}

// ── the REST devfile and the starter projects ────────────────────────────────

#[actix_web::test]
async fn the_devfile_is_served_byte_exact_at_its_default_and_its_version() {
    let (_s, app) = app().await;
    let (status, body) = status_and_body(&app, &format!("/proxy/{REG}/devfiles/nodejs")).await;
    assert_eq!(status, 200);
    assert_eq!(body, DEVFILE_221);
    let (status, body) =
        status_and_body(&app, &format!("/proxy/{REG}/devfiles/nodejs/2.2.0")).await;
    assert_eq!(status, 200);
    assert_eq!(body, DEVFILE_220);
}

#[actix_web::test]
async fn a_blocked_version_is_refused_and_the_default_follows_the_filter() {
    let (_s, app) = app().await;
    block_version(&app, REG, "nodejs", "2.2.1").await;
    let (status, _) = status_and_body(&app, &format!("/proxy/{REG}/devfiles/nodejs/2.2.1")).await;
    assert_eq!(status, 404);
    let (status, body) = status_and_body(&app, &format!("/proxy/{REG}/devfiles/nodejs")).await;
    assert_eq!(status, 200);
    assert_eq!(
        body, DEVFILE_220,
        "the versionless route takes the moved default"
    );
}

#[actix_web::test]
async fn deny_latest_refuses_the_default_and_leaves_a_pinned_version() {
    let server = upstream(DEVFILE_221).await;
    let app = app_against(server.url(), true).await;
    for uri in [
        format!("/proxy/{REG}/devfiles/nodejs"),
        format!("/proxy/{REG}/devfiles/nodejs/starter-projects/nodejs-starter"),
    ] {
        let (status, _) = status_and_body(&app, &uri).await;
        assert_eq!(status, 403, "{uri}: the default is this registry's latest");
    }
    let (status, body) =
        status_and_body(&app, &format!("/proxy/{REG}/devfiles/nodejs/2.2.0")).await;
    assert_eq!(status, 200);
    assert_eq!(body, DEVFILE_220);
}

#[actix_web::test]
async fn a_starter_project_is_the_default_versions() {
    let (_s, app) = app().await;
    let resp = call_service(
        &app,
        admin(
            Method::GET,
            &format!("/proxy/{REG}/devfiles/nodejs/starter-projects/nodejs-starter"),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("content-disposition").unwrap(),
        "attachment; filename=\"nodejs-starter.zip\""
    );
    assert_eq!(read_body(resp).await.as_ref(), STARTER);
}

#[actix_web::test]
async fn devfile_traversal_version_returns_400() {
    let (_s, app) = app().await;
    let (status, _) = status_and_body(
        &app,
        &format!("/proxy/{REG}/devfiles/nodejs/..%2F..%2Fetc%2Fx"),
    )
    .await;
    assert_eq!(status, 400);
    let (status, _) = status_and_body(&app, &format!("/proxy/{REG}/devfiles/..%2Fx")).await;
    assert_eq!(status, 400);
}

// ── the OCI routes: the tag is the chokepoint ────────────────────────────────

#[actix_web::test]
async fn the_manifest_head_carries_the_real_digest_and_length() {
    let (_s, app) = app().await;
    let expected = manifest(DEVFILE_221);
    let uri = format!("/proxy/{REG}/v2/devfile-catalog/nodejs/manifests/2.2.1");

    let head = call_service(&app, admin(Method::HEAD, &uri)).await;
    assert_eq!(head.status(), 200);
    assert_eq!(
        head.headers()
            .get("docker-content-digest")
            .unwrap()
            .to_str()
            .unwrap(),
        sha(expected.as_bytes())
    );
    // The HTTP/1 encoder writes `Content-Length` from the body's size — also
    // for a `HEAD`, whose body it then drops — and `call_service` never runs
    // the encoder, so the size is what this can assert on.
    assert_eq!(
        actix_web::body::MessageBody::size(head.response().body()),
        actix_web::body::BodySize::Sized(expected.len() as u64),
        "a HEAD answered with length 0 is a digest failure on the client (§5.1)"
    );

    let get = call_service(&app, admin(Method::GET, &uri)).await;
    assert_eq!(get.status(), 200);
    assert_eq!(
        get.headers().get("content-type").unwrap(),
        "application/vnd.oci.image.manifest.v1+json"
    );
    assert_eq!(read_body(get).await.as_ref(), expected.as_bytes());
}

#[actix_web::test]
async fn a_pull_by_digest_reaches_the_manifest_and_every_layer() {
    let (_s, app) = app().await;
    let m = manifest(DEVFILE_221);
    let (status, body) = status_and_body(
        &app,
        &format!(
            "/proxy/{REG}/v2/devfile-catalog/nodejs/manifests/{}",
            sha(m.as_bytes())
        ),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, m.as_bytes());
    for layer in [DEVFILE_221, ARCHIVE] {
        let (status, body) = status_and_body(
            &app,
            &format!(
                "/proxy/{REG}/v2/devfile-catalog/nodejs/blobs/{}",
                sha(layer)
            ),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body, layer);
    }
}

#[actix_web::test]
async fn a_blocked_versions_tag_and_digests_are_unreachable() {
    let (_s, app) = app().await;
    block_version(&app, REG, "nodejs", "2.2.0").await;
    let (status, body) = status_and_body(
        &app,
        &format!("/proxy/{REG}/v2/devfile-catalog/nodejs/manifests/2.2.0"),
    )
    .await;
    assert_eq!(
        (status, oci_code(&body).as_str()),
        (404, "MANIFEST_UNKNOWN")
    );
    let (status, body) = status_and_body(
        &app,
        &format!(
            "/proxy/{REG}/v2/devfile-catalog/nodejs/blobs/{}",
            sha(DEVFILE_220)
        ),
    )
    .await;
    assert_eq!((status, oci_code(&body).as_str()), (404, "BLOB_UNKNOWN"));
    let tags = get_json(
        &app,
        &format!("/proxy/{REG}/v2/devfile-catalog/nodejs/tags/list"),
    )
    .await;
    assert_eq!(tags["tags"], serde_json::json!(["2.2.1"]));
}

#[actix_web::test]
async fn a_repository_the_index_does_not_name_is_name_unknown() {
    let (_s, app) = app().await;
    for uri in [
        format!("/proxy/{REG}/v2/other-catalog/nodejs/manifests/2.2.1"),
        format!("/proxy/{REG}/v2/devfile-catalog/go/manifests/2.6.0"),
    ] {
        let (status, body) = status_and_body(&app, &uri).await;
        assert_eq!(
            (status, oci_code(&body).as_str()),
            (404, "NAME_UNKNOWN"),
            "{uri}"
        );
    }
    let (status, body) = status_and_body(
        &app,
        &format!("/proxy/{REG}/v2/devfile-catalog/nodejs/blobs/sha256:..%2F..%2Fx"),
    )
    .await;
    assert_eq!((status, oci_code(&body).as_str()), (404, "DIGEST_INVALID"));
}

/// The difference from `registry-library` alone (RFC 0035 §2.3): an altered
/// layer is refused by this instance and no byte of it is served.
#[actix_web::test]
async fn an_altered_layer_is_never_served() {
    let mut tampered = DEVFILE_221.to_vec();
    *tampered.last_mut().unwrap() = b'#';
    let server = upstream(&tampered).await;
    let app = app_against(server.url(), false).await;
    let (status, body) = status_and_body(
        &app,
        &format!(
            "/proxy/{REG}/v2/devfile-catalog/nodejs/blobs/{}",
            sha(DEVFILE_221)
        ),
    )
    .await;
    assert!(status >= 500, "{status}");
    assert_ne!(body, tampered);
    let (status, _) = status_and_body(&app, &format!("/proxy/{REG}/devfiles/nodejs")).await;
    assert!(
        status >= 500,
        "the REST route reads the same layer: {status}"
    );
}

#[actix_web::test]
async fn the_oci_ping_answers_the_api_version() {
    let (_s, app) = app().await;
    let resp = call_service(&app, admin(Method::GET, &format!("/proxy/{REG}/v2/"))).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("docker-distribution-api-version")
            .unwrap(),
        "registry/2.0"
    );
}

// ── the guard: a devfile route never claims another kind's path ──────────────

#[actix_web::test]
async fn the_routes_leave_another_kinds_paths_alone() {
    let app = proxy_registry_app("npm-reg", "npm").await;
    for uri in ["/proxy/npm-reg/index", "/proxy/npm-reg/v2index"] {
        let (_, body) = status_and_body(&app, uri).await;
        let body = String::from_utf8_lossy(&body);
        assert!(
            !body.contains("devfile"),
            "{uri} reached a devfile route: {body}"
        );
    }
}
