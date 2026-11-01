//! Devfile registries in `local` and `hybrid` mode (RFC 0039).
//!
//! What the two clients need of a *published* stack is what they need of a
//! proxied one (RFC 0035): the manifest byte-exact with a `HEAD` that carries
//! its real digest, every layer by digest, the index listing it. What this
//! mode adds, and what these tests hold it to:
//!
//! - the server derives the OCI objects from the bundle once, at publish, and
//!   the digest the `201` names is the digest every later read answers with;
//! - in `hybrid` a stack published here owns its whole name — upstream's
//!   entries for it leave the index and its upstream objects are never fetched;
//! - taking a name upstream lists is `devfile:stack:claim`, and an undecidable
//!   claim refuses (`503`) rather than lets the publish through.

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
    rules::{BlockListRule, Rule},
    services::RegistryPolicy,
};
use sha2::{Digest, Sha256};

const REG: &str = "devfile";

fn sha(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn devfile(name: &str, version: &str) -> String {
    format!(
        "schemaVersion: 2.2.0\nmetadata:\n  name: {name}\n  version: {version}\n  \
         displayName: {name} stack\n  icon: https://icons.acme.example/{name}.svg\n\
         starterProjects:\n  - name: hello\n    zip:\n      location: hello.zip\n\
         components:\n  - name: runtime\n    container:\n      image: golang:1.23\n"
    )
}

/// Only the gzip magic is read by the server; the pull hands it on verbatim.
const ARCHIVE: &[u8] = b"\x1f\x8b kubernetes/deploy.yaml, packed by the CLI";
const STARTER: &[u8] = b"PK\x03\x04 hello";

/// A bundle, as `batlehub publish` packs one.
fn bundle(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, bytes) in members {
        let mut h = tar::Header::new_ustar();
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(0);
        // Written raw: `set_path` refuses the `..` a traversal test needs.
        h.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
        h.set_cksum();
        b.append(&h, *bytes).unwrap();
    }
    b.into_inner().unwrap()
}

fn stack(name: &str, version: &str) -> Vec<u8> {
    let d = devfile(name, version);
    bundle(&[
        ("devfile.yaml", d.as_bytes()),
        ("archive.tar", ARCHIVE),
        ("starter-projects/hello.zip", STARTER),
    ])
}

fn publish(body: Vec<u8>, token: Option<&str>) -> actix_http::Request {
    let mut req = TestRequest::post()
        .uri(&format!("/proxy/{REG}/api/stacks"))
        .insert_header(("Content-Type", "application/x-tar"));
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    req.set_payload(body).to_request()
}

fn as_admin(method: Method, uri: &str) -> actix_http::Request {
    TestRequest::default()
        .method(method)
        .uri(uri)
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request()
}

async fn publish_ok<S: TestService>(app: &S, name: &str, version: &str) -> String {
    let resp = call_service(app, publish(stack(name, version), Some(ADMIN_TOKEN))).await;
    let status = resp.status();
    let body = read_body(resp).await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["stack"], name);
    v["digest"].as_str().unwrap().to_owned()
}

async fn status_of<S: TestService>(app: &S, req: actix_http::Request) -> (u16, Vec<u8>) {
    let resp = call_service(app, req).await;
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
        .filter(|e| e["name"] == stack)
        .flat_map(|e| e["versions"].as_array().cloned().unwrap_or_default())
        .map(|v| v["version"].as_str().unwrap().to_owned())
        .collect()
}

/// `registry.devfile.io` with `nodejs@2.2.1` and `go@2.6.0`, and the mock of
/// nodejs's upstream manifest: never asked for when `asked` is false, asked at
/// least once when it is true.
async fn upstream(asked: bool) -> (mockito::ServerGuard, mockito::Mock) {
    let mut s = mockito::Server::new_async().await;
    let index = serde_json::json!([
        {"name": "nodejs", "type": "stack", "versions": [
            {"version": "2.2.1", "default": true, "links": {"self": "devfile-catalog/nodejs:2.2.1"}}
        ]},
        {"name": "go", "type": "stack", "versions": [
            {"version": "2.6.0", "default": true, "links": {"self": "devfile-catalog/go:2.6.0"}}
        ]}
    ]);
    for path in ["/v2index", "/index"] {
        s.mock("GET", path)
            .with_body(index.to_string())
            .create_async()
            .await;
    }
    let manifest = s
        .mock("GET", "/v2/devfile-catalog/nodejs/manifests/2.2.1")
        .with_body("{}")
        .expect_at_least(usize::from(asked))
        .expect_at_most(if asked { usize::MAX } else { 0 })
        .create_async()
        .await;
    (s, manifest)
}

/// An upstream that answers nothing: the claim cannot be decided.
async fn upstream_down() -> mockito::ServerGuard {
    let mut s = mockito::Server::new_async().await;
    s.mock("GET", mockito::Matcher::Any)
        .with_status(500)
        .create_async()
        .await;
    s
}

async fn app(mode: RegistryMode, base: Option<String>) -> impl TestService {
    let parts = local_registry_app_parts(REG, "devfile", mode, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        if let Some(base) = base {
            hot.registries.insert(
                REG.to_owned(),
                Arc::new(DevfileRegistryClient::new(base, &Default::default()).unwrap())
                    as Arc<dyn RegistryClient>,
            );
        }
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![
                    Box::new(BlockListRule::new(Arc::clone(&parts.proxy_svc.repo)))
                        as Box<dyn Rule>,
                ],
            }),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

// ── local: what a pull reads ─────────────────────────────────────────────────

#[actix_web::test]
async fn a_published_version_is_pulled_byte_exact_by_the_digest_its_publish_named() {
    let app = app(RegistryMode::Local, None).await;
    let digest = publish_ok(&app, "acme-go", "1.0.0").await;

    let uri = format!("/proxy/{REG}/v2/devfile-catalog/acme-go/manifests/1.0.0");
    let head = call_service(&app, as_admin(Method::HEAD, &uri)).await;
    assert_eq!(head.status(), 200);
    assert_eq!(
        head.headers().get("docker-content-digest").unwrap(),
        digest.as_str(),
        "HEAD answers the digest the 201 named"
    );
    let (status, manifest) = status_of(&app, as_admin(Method::GET, &uri)).await;
    assert_eq!(status, 200);
    assert_eq!(sha(&manifest), digest, "the GET body is those bytes");
    let (by_digest, again) = status_of(
        &app,
        as_admin(
            Method::GET,
            &format!("/proxy/{REG}/v2/devfile-catalog/acme-go/manifests/{digest}"),
        ),
    )
    .await;
    assert_eq!((by_digest, again), (200, manifest.clone()));

    let m: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let mut blobs: Vec<String> = vec![m["config"]["digest"].as_str().unwrap().to_owned()];
    blobs.extend(
        m["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["digest"].as_str().unwrap().to_owned()),
    );
    let published = devfile("acme-go", "1.0.0");
    let expected: [&[u8]; 3] = [b"{}", published.as_bytes(), ARCHIVE];
    for (blob, want) in blobs.iter().zip(expected) {
        let (status, body) = status_of(
            &app,
            as_admin(
                Method::GET,
                &format!("/proxy/{REG}/v2/devfile-catalog/acme-go/blobs/{blob}"),
            ),
        )
        .await;
        assert_eq!(status, 200, "{blob}");
        assert_eq!(body, want, "{blob}");
        assert_eq!(sha(&body), *blob);
    }

    let tags = get_json(
        &app,
        &format!("/proxy/{REG}/v2/devfile-catalog/acme-go/tags/list"),
    )
    .await;
    assert_eq!(tags["tags"], serde_json::json!(["1.0.0"]));
}

#[actix_web::test]
async fn the_rest_devfile_and_the_starter_zip_are_the_bundles_members() {
    let app = app(RegistryMode::Local, None).await;
    publish_ok(&app, "acme-go", "1.0.0").await;
    assert_eq!(
        get_text(&app, &format!("/proxy/{REG}/devfiles/acme-go")).await,
        devfile("acme-go", "1.0.0")
    );
    assert_eq!(
        get_bytes(
            &app,
            &format!("/proxy/{REG}/devfiles/acme-go/starter-projects/hello")
        )
        .await,
        STARTER
    );

    // Published without a zip for its starter: a 404 that says so.
    let d = devfile("bare", "1.0.0");
    let resp = call_service(
        &app,
        publish(bundle(&[("devfile.yaml", d.as_bytes())]), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(resp.status(), 201);
    let (status, body) = status_of(
        &app,
        as_admin(
            Method::GET,
            &format!("/proxy/{REG}/devfiles/bare/starter-projects/hello"),
        ),
    )
    .await;
    assert_eq!(status, 404);
    assert!(
        String::from_utf8_lossy(&body).contains("published without"),
        "{}",
        String::from_utf8_lossy(&body)
    );
}

#[actix_web::test]
async fn both_index_shapes_list_a_published_stack_with_its_icon() {
    let app = app(RegistryMode::Local, None).await;
    publish_ok(&app, "acme-go", "1.0.0").await;
    publish_ok(&app, "acme-go", "1.1.0").await;
    let v2 = get_json(&app, &format!("/proxy/{REG}/v2index")).await;
    assert_eq!(versions_of(&v2, "acme-go"), ["1.1.0", "1.0.0"]);
    assert_eq!(v2[0]["icon"], "https://icons.acme.example/acme-go.svg");
    assert_eq!(
        v2[0]["versions"][0]["links"]["self"],
        "devfile-catalog/acme-go:1.1.0"
    );
    let legacy = get_json(&app, &format!("/proxy/{REG}/index/all")).await;
    assert_eq!(legacy[0]["version"], "1.1.0");
    assert_eq!(
        get_json(&app, &format!("/proxy/{REG}/v2index/sample")).await,
        serde_json::json!([])
    );
}

#[actix_web::test]
async fn a_blocked_published_version_leaves_the_index_and_its_digests_are_unreachable() {
    let app = app(RegistryMode::Local, None).await;
    publish_ok(&app, "acme-go", "1.0.0").await;
    let manifest = get_bytes(
        &app,
        &format!("/proxy/{REG}/v2/devfile-catalog/acme-go/manifests/1.0.0"),
    )
    .await;
    let m: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let layer = m["layers"][0]["digest"].as_str().unwrap().to_owned();

    block_version(&app, REG, "acme-go", "1.0.0").await;
    assert!(versions_of(
        &get_json(&app, &format!("/proxy/{REG}/v2index")).await,
        "acme-go"
    )
    .is_empty());
    let (status, body) = status_of(
        &app,
        as_admin(
            Method::GET,
            &format!("/proxy/{REG}/v2/devfile-catalog/acme-go/manifests/1.0.0"),
        ),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(oci_code(&body), "NAME_UNKNOWN");
    let (status, _) = status_of(
        &app,
        as_admin(
            Method::GET,
            &format!("/proxy/{REG}/v2/devfile-catalog/acme-go/blobs/{layer}"),
        ),
    )
    .await;
    assert_eq!(status, 404, "a kept layer digest replayed is not served");
}

// ── local: the publish edge ──────────────────────────────────────────────────

#[actix_web::test]
async fn a_republish_is_409_and_the_same_bytes_give_the_same_digest() {
    let app = app(RegistryMode::Local, None).await;
    let first = publish_ok(&app, "acme-go", "1.0.0").await;
    let (status, _) = status_of(&app, publish(stack("acme-go", "1.0.0"), Some(ADMIN_TOKEN))).await;
    assert_eq!(status, 409);
    let other = self::app(RegistryMode::Local, None).await;
    assert_eq!(
        publish_ok(&other, "acme-go", "1.0.0").await,
        first,
        "the same bundle on another instance is the same manifest (RFC 0039 §6.7)"
    );
}

#[actix_web::test]
async fn an_anonymous_publish_is_refused_and_a_proxy_registry_has_no_publish_route() {
    let app = app(RegistryMode::Local, None).await;
    let (status, _) = status_of(&app, publish(stack("acme-go", "1.0.0"), None)).await;
    assert_eq!(status, 403);
    let proxy = app_proxy().await;
    let (status, _) = status_of(
        &proxy,
        publish(stack("acme-go", "1.0.0"), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(status, 404);
}

async fn app_proxy() -> impl TestService {
    app(RegistryMode::Proxy, None).await
}

#[actix_web::test]
async fn devfile_publish_traversal_member_returns_400() {
    let app = app(RegistryMode::Local, None).await;
    let d = devfile("acme-go", "1.0.0");
    let (status, body) = status_of(
        &app,
        publish(
            bundle(&[("devfile.yaml", d.as_bytes()), ("../../etc/x", b"x")]),
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, 400);
    assert!(String::from_utf8_lossy(&body).contains("../../etc/x"));
}

#[actix_web::test]
async fn devfile_publish_traversal_version_returns_400() {
    let app = app(RegistryMode::Local, None).await;
    let d = devfile("acme-go", "../../etc/x");
    let (status, _) = status_of(
        &app,
        publish(bundle(&[("devfile.yaml", d.as_bytes())]), Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(status, 400);
}

#[actix_web::test]
async fn a_deleted_version_leaves_the_index() {
    let app = app(RegistryMode::Local, None).await;
    publish_ok(&app, "acme-go", "1.0.0").await;
    let (status, _) = status_of(
        &app,
        as_admin(
            Method::DELETE,
            &format!("/proxy/{REG}/api/stacks/acme-go/1.0.0"),
        ),
    )
    .await;
    assert_eq!(status, 200);
    assert!(versions_of(
        &get_json(&app, &format!("/proxy/{REG}/v2index")).await,
        "acme-go"
    )
    .is_empty());
    let (status, _) = status_of(
        &app,
        as_admin(
            Method::DELETE,
            &format!("/proxy/{REG}/api/stacks/acme-go/1.0.0"),
        ),
    )
    .await;
    assert_eq!(status, 404);
}

// ── hybrid: a local stack owns its name ──────────────────────────────────────

#[actix_web::test]
async fn a_hybrid_index_lists_upstream_and_local_stacks_side_by_side() {
    let (up, _) = upstream(false).await;
    let app = app(RegistryMode::Hybrid, Some(up.url())).await;
    let (status, _) = status_of(&app, publish(stack("acme-go", "1.0.0"), Some(USER_TOKEN))).await;
    assert_eq!(status, 201, "a name upstream does not list needs no claim");
    let v2 = get_json(&app, &format!("/proxy/{REG}/v2index")).await;
    let names: Vec<&str> = v2
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, ["acme-go", "go", "nodejs"], "sorted by name");
}

#[actix_web::test]
async fn taking_a_name_upstream_lists_needs_the_claim_and_then_owns_it_whole() {
    let (up, manifest) = upstream(false).await;
    let app = app(RegistryMode::Hybrid, Some(up.url())).await;

    let (status, body) = status_of(&app, publish(stack("nodejs", "9.9.9"), Some(USER_TOKEN))).await;
    assert_eq!(status, 403);
    assert!(
        String::from_utf8_lossy(&body).contains("devfile:stack:claim"),
        "the refusal names the action: {}",
        String::from_utf8_lossy(&body)
    );

    publish_ok(&app, "nodejs", "9.9.9").await;
    let v2 = get_json(&app, &format!("/proxy/{REG}/v2index")).await;
    assert_eq!(
        versions_of(&v2, "nodejs"),
        ["9.9.9"],
        "upstream's 2.2.1 is gone"
    );
    let legacy = get_json(&app, &format!("/proxy/{REG}/index")).await;
    let node: Vec<_> = legacy
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["name"] == "nodejs")
        .collect();
    assert_eq!(node.len(), 1);
    assert_eq!(node[0]["version"], "9.9.9");

    let (status, body) = status_of(
        &app,
        as_admin(
            Method::GET,
            &format!("/proxy/{REG}/v2/devfile-catalog/nodejs/manifests/2.2.1"),
        ),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(oci_code(&body), "MANIFEST_UNKNOWN");
    // The owned stack's upstream manifest was never asked for.
    manifest.assert_async().await;
}

#[actix_web::test]
async fn an_undecidable_claim_is_503_and_a_claim_holder_publishes_anyway() {
    let down = upstream_down().await;
    let app = app(RegistryMode::Hybrid, Some(down.url())).await;
    let (status, body) =
        status_of(&app, publish(stack("acme-go", "1.0.0"), Some(USER_TOKEN))).await;
    assert_eq!(status, 503, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&body).contains("devfile:stack:claim"));
    publish_ok(&app, "acme-go", "1.0.0").await;
    // The name is owned now: a later version is an ordinary publish, and
    // upstream being down does not matter to it.
    let (status, body) =
        status_of(&app, publish(stack("acme-go", "1.1.0"), Some(USER_TOKEN))).await;
    assert_ne!(status, 503, "{}", String::from_utf8_lossy(&body));
}

#[actix_web::test]
async fn deleting_the_last_version_releases_the_name() {
    let (up, manifest) = upstream(true).await;
    let app = app(RegistryMode::Hybrid, Some(up.url())).await;
    publish_ok(&app, "nodejs", "9.9.9").await;
    let (status, _) = status_of(
        &app,
        as_admin(
            Method::DELETE,
            &format!("/proxy/{REG}/api/stacks/nodejs/9.9.9"),
        ),
    )
    .await;
    assert_eq!(status, 200);
    let v2 = get_json(&app, &format!("/proxy/{REG}/v2index")).await;
    assert_eq!(
        versions_of(&v2, "nodejs"),
        ["2.2.1"],
        "upstream's stack is back"
    );
    let (status, _) = status_of(
        &app,
        as_admin(
            Method::GET,
            &format!("/proxy/{REG}/v2/devfile-catalog/nodejs/manifests/2.2.1"),
        ),
    )
    .await;
    assert_ne!(status, 404, "and resolved upstream again");
    let (status, _) = status_of(&app, publish(stack("nodejs", "9.9.10"), Some(USER_TOKEN))).await;
    assert_eq!(status, 403, "a new first publish needs the claim again");
    manifest.assert_async().await;
}
