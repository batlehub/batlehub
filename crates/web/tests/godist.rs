//! Go's toolchain distribution as a typed registry (RFC 0048 §6.7).
//!
//! What mise's `core:go` does, and what this proxy therefore has to get right
//! (mise 2026.9.5, `src/plugins/core/go.rs`):
//!
//! - it downloads `{go.download_mirror}/go{v}.{os}-{arch}.tar.gz` (`:384-387`)
//!   and reads `{that}.sha256` as the whole digest (`:126-128`, `:137-138`) —
//!   so the archive has to be the one the digest names and the `.sha256` has to
//!   be 64 hex digits and nothing else;
//! - it never reads a release listing from the mirror: its list comes from the
//!   versions host or `git ls-remote` (`:224-280`), so a blocked release is
//!   refused at the download, on both files.
//!
//! The registry is the real `GoDistRegistryClient` against a mock upstream
//! serving both hosts, because "a file the document does not list is never
//! fetched" and "the archive is verified against the document" are the
//! client's and the cache's jobs, and a fixture would prove neither.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::test::call_service;
use batlehub_adapters::registry::GoDistRegistryClient;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::Role, ports::RegistryClient, rules::BlockListRule, services::RegistryPolicy,
};
use sha2::{Digest, Sha256};

const REG: &str = "go";
const ARCHIVE: &[u8] = b"not really a tarball";

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn document(sha256: &str) -> String {
    serde_json::json!([
        {"version": "go1.25.4", "stable": true, "files": [
            {"filename": "go1.25.4.linux-amd64.tar.gz", "os": "linux", "arch": "amd64",
             "version": "go1.25.4", "sha256": sha256, "size": ARCHIVE.len(), "kind": "archive"}
        ]},
        {"version": "go1.25.3", "stable": true, "files": [
            {"filename": "go1.25.3.linux-amd64.tar.gz", "os": "linux", "arch": "amd64",
             "version": "go1.25.3", "sha256": sha256, "size": ARCHIVE.len(), "kind": "archive"}
        ]}
    ])
    .to_string()
}

/// A mock serving the release document at `/dl/` and the archives under
/// `/go/`, with the document's `sha256` as given.
async fn upstream(sha256: &str) -> mockito::ServerGuard {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/dl/")
        .match_query(mockito::Matcher::Any)
        .with_body(document(sha256))
        .create_async()
        .await;
    server
}

async fn app_against(server: &mockito::ServerGuard) -> impl TestService {
    app_against_with(server, None).await
}

/// `app_against`, its grants derived from `perms` when given.
async fn app_against_with(
    server: &mockito::ServerGuard,
    perms: Option<RbacFixture>,
) -> impl TestService {
    let parts = local_registry_app_parts(REG, "godist", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        if let Some(perms) = perms {
            hot.grants.insert(
                REG.to_owned(),
                Arc::new(fixture_grants(REG, "godist", &RegistryMode::Proxy, &perms)),
            );
        }
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(
                GoDistRegistryClient::new(
                    format!("{}/go", server.url()),
                    format!("{}/dl/?mode=json&include=all", server.url()),
                    &Default::default(),
                )
                .unwrap(),
            ) as Arc<dyn RegistryClient>,
        );
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![Box::new(BlockListRule::new(Arc::clone(
                    &parts.proxy_svc.repo,
                )))],
            }),
        );
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

fn file_url(file: &str) -> String {
    format!("/proxy/{REG}/godist/{file}")
}

#[actix_web::test]
async fn mise_gets_the_archive_and_its_sha256_from_the_document() {
    let mut server = upstream(&digest(ARCHIVE)).await;
    let archive = server
        .mock("GET", "/go/go1.25.4.linux-amd64.tar.gz")
        .with_body(ARCHIVE)
        .expect(1)
        .create_async()
        .await;
    let checksum = server
        .mock("GET", "/go/go1.25.4.linux-amd64.tar.gz.sha256")
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;

    let body = get_bytes(&app, &file_url("go1.25.4.linux-amd64.tar.gz")).await;
    assert_eq!(&body[..], ARCHIVE);
    // Second read from the cache: the upstream saw one archive request.
    get_bytes(&app, &file_url("go1.25.4.linux-amd64.tar.gz")).await;
    archive.assert_async().await;

    let url = file_url("go1.25.4.linux-amd64.tar.gz.sha256");
    assert_content_type(&app, &url, "text/plain").await;
    assert_eq!(get_text(&app, &url).await, digest(ARCHIVE));
    checksum.assert_async().await;
}

/// The cache write verifies the bytes against the document: an archive whose
/// digest disagrees is refused rather than served and kept.
#[actix_web::test]
async fn an_archive_that_does_not_match_the_document_is_refused() {
    let mut server = upstream(&"0".repeat(64)).await;
    server
        .mock("GET", "/go/go1.25.4.linux-amd64.tar.gz")
        .with_body(ARCHIVE)
        .create_async()
        .await;
    let app = app_against(&server).await;
    let resp = call_service(&app, admin_get(&file_url("go1.25.4.linux-amd64.tar.gz"))).await;
    assert_eq!(
        resp.status(),
        502,
        "a mismatching archive must not be served"
    );
}

#[actix_web::test]
async fn a_blocked_go_version_download_is_403() {
    let mut server = upstream(&digest(ARCHIVE)).await;
    let never = server
        .mock("GET", "/go/go1.25.4.linux-amd64.tar.gz")
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;
    block_version(&app, REG, "go", "1.25.4").await;

    for file in [
        "go1.25.4.linux-amd64.tar.gz",
        "go1.25.4.linux-amd64.tar.gz.sha256",
    ] {
        let resp = call_service(&app, admin_get(&file_url(file))).await;
        assert_eq!(resp.status(), 403, "{file}");
    }
    never.assert_async().await;

    let resp = call_service(
        &app,
        admin_get(&file_url("go1.25.3.linux-amd64.tar.gz.sha256")),
    )
    .await;
    assert_eq!(resp.status(), 200, "the release next to it is untouched");
}

#[actix_web::test]
async fn an_unlisted_go_file_is_404_without_upstream() {
    let mut server = upstream(&digest(ARCHIVE)).await;
    let never = server
        .mock("GET", mockito::Matcher::Regex("^/go/".to_owned()))
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;
    for file in [
        "go1.25.4.plan9-amd64.tar.gz",
        "go9.9.9.linux-amd64.tar.gz",
        "go9.9.9.linux-amd64.tar.gz.sha256",
    ] {
        let resp = call_service(&app, admin_get(&file_url(file))).await;
        assert_eq!(resp.status(), 404, "{file}");
    }
    never.assert_async().await;
}

#[actix_web::test]
async fn the_release_document_is_served_without_a_blocked_release() {
    let server = upstream(&digest(ARCHIVE)).await;
    let app = app_against(&server).await;
    let url = format!("/proxy/{REG}/godist/index.json");
    assert_eq!(get_json(&app, &url).await.as_array().map(Vec::len), Some(2));

    block_version(&app, REG, "go", "1.25.4").await;
    let after = get_json(&app, &url).await;
    let versions: Vec<&str> = after
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["go1.25.3"]);
}

/// The file name reaches a storage key, so anything that is not a Go archive
/// name is a clean `400` at the edge.
#[actix_web::test]
async fn godist_file_traversal_returns_400() {
    let server = upstream(&digest(ARCHIVE)).await;
    let app = app_against(&server).await;
    for file in [
        "%2e%2e",
        "..%2Fgo1.25.4.linux-amd64.tar.gz",
        "go..%2F..%2Fetc.linux-amd64.tar.gz",
        "go1.25.4.src.tar.gz",
        "SHASUMS256.txt",
    ] {
        let resp = call_service(&app, admin_get(&file_url(file))).await;
        assert_eq!(resp.status(), 400, "{file}");
    }
}

#[actix_web::test]
async fn the_routes_refuse_a_registry_of_another_kind() {
    let app = proxy_registry_app("npm-reg", "npm").await;
    // Routed on its own kind's routes only (TD-0004 §1): npm answers what it
    // answers, and the godist handler is never reached.
    let resp = call_service(
        &app,
        admin_get("/proxy/npm-reg/godist/go1.25.4.linux-amd64.tar.gz"),
    )
    .await;
    assert_ne!(
        resp.request().match_pattern().as_deref(),
        Some("/proxy/{registry}/godist/{file}")
    );
}

/// `git ls-remote <instance>/proxy/{reg}/godist` — mise's Go list when it does
/// not use the versions host (RFC 0048 §15 Q13): a tag per listed release, the
/// blocked one gone, in the dumb-HTTP form git falls back to on `text/plain`
/// whichever protocol version it asked for.
#[actix_web::test]
async fn the_git_ref_list_has_no_blocked_release() {
    let server = upstream(&digest(ARCHIVE)).await;
    let app = app_against(&server).await;
    block_version(&app, REG, "go", "1.25.3").await;

    let req = actix_web::test::TestRequest::get()
        .uri(&format!(
            "/proxy/{REG}/godist/info/refs?service=git-upload-pack"
        ))
        .insert_header(("Git-Protocol", "version=2"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let ctype = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(ctype.starts_with("text/plain"), "{ctype}");
    let body = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("\trefs/tags/go1.25.4\n"), "{body}");
    assert!(
        !body.contains("go1.25.3"),
        "a blocked release is listed: {body}"
    );
}

/// Only the ref list is routed: git's smart-protocol POST and the dumb paths
/// it would read next reach nothing that asks upstream.
#[actix_web::test]
async fn nothing_else_of_git_reaches_upstream() {
    let mut server = mockito::Server::new_async().await;
    let never = server
        .mock("GET", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let app = app_against(&server).await;
    let req = actix_web::test::TestRequest::post()
        .uri(&format!("/proxy/{REG}/godist/git-upload-pack"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let status = call_service(&app, req).await.status();
    assert!(status == 404 || status == 405, "{status}");
    for path in ["HEAD", "objects/info/packs"] {
        let resp = call_service(&app, admin_get(&file_url(path))).await;
        assert!(resp.status().is_client_error(), "{path}: {}", resp.status());
    }
    never.assert_async().await;
}

fn basic(user_and_token: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(user_and_token)
    )
}

fn challenge_of(resp: &actix_web::dev::ServiceResponse) -> Option<String> {
    resp.headers()
        .get("www-authenticate")
        .map(|v| v.to_str().unwrap().to_owned())
}

/// git sends the credential it holds (URL userinfo, `~/.netrc`, a credential
/// helper) only in answer to a `401` carrying a challenge, so an anonymous
/// refusal on a proxy route is that and not a bare `403`. A caller who *did*
/// authenticate and holds no grant stays a `403`: a challenge would only make
/// git ask for the same credential again.
#[actix_web::test]
async fn an_anonymous_refusal_challenges_and_an_authenticated_one_does_not() {
    let server = upstream(&digest(ARCHIVE)).await;
    // Closed to anonymous and to `role:user`, as `config.authz-live.toml` is.
    let closed = RbacFixture {
        roles: std::collections::HashMap::from([
            (Role::Anonymous, vec![]),
            (Role::User, vec![]),
            (Role::Admin, vec!["*".to_owned()]),
        ]),
        groups: std::collections::HashMap::new(),
    };
    let app = app_against_with(&server, Some(closed)).await;
    let refs = format!("/proxy/{REG}/godist/info/refs?service=git-upload-pack");
    let get = |auth: Option<String>| {
        let mut req = actix_web::test::TestRequest::get().uri(&refs);
        if let Some(a) = auth {
            req = req.insert_header(("Authorization", a));
        }
        req.to_request()
    };

    let resp = call_service(&app, get(None)).await;
    assert_eq!(resp.status(), 401);
    assert_eq!(
        challenge_of(&resp).as_deref(),
        Some("Basic realm=\"BatleHub\"")
    );

    // A credential no provider accepts is the same caller as none.
    let resp = call_service(&app, get(Some(basic("x:not-a-token")))).await;
    assert_eq!(resp.status(), 401);
    assert!(challenge_of(&resp).is_some());

    // The token in the password field, as git sends it after the challenge.
    let resp = call_service(&app, get(Some(basic(&format!("x:{USER_TOKEN}"))))).await;
    assert_eq!(resp.status(), 403, "authenticated, no grant");
    assert_eq!(challenge_of(&resp), None);
    let resp = call_service(&app, get(Some(basic(&format!("x:{ADMIN_TOKEN}"))))).await;
    assert_eq!(resp.status(), 200);

    // Never outside `/proxy/`, whatever the status: a browser would pop its
    // login dialog.
    for uri in ["/api/v1/me/downloads", "/api/v1/admin/stats"] {
        let resp = call_service(
            &app,
            actix_web::test::TestRequest::get().uri(uri).to_request(),
        )
        .await;
        assert!(resp.status().is_client_error(), "{uri}: {}", resp.status());
        assert_eq!(challenge_of(&resp), None, "{uri}");
    }
}

/// A rule refusing everybody alike is not a credential question: an anonymous
/// caller the grants admit, refused by a block, gets the `403` and no challenge.
#[actix_web::test]
async fn an_anonymous_refusal_by_a_rule_does_not_challenge() {
    let server = upstream(&digest(ARCHIVE)).await;
    let app = app_against(&server).await;
    block_version(&app, REG, "go", "1.25.4").await;
    let resp = call_service(
        &app,
        actix_web::test::TestRequest::get()
            .uri(&file_url("go1.25.4.linux-amd64.tar.gz.sha256"))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
    assert_eq!(challenge_of(&resp), None);
}
