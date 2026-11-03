//! RFC 0040 §5.2 made checkable: the pullability answer and the download
//! agree. Each case sets a state up, asks `GET /api/v1/packages/access`, then
//! asks for the artifact itself — refused ⇔ ⛔/⏳, served ⇔ ✅ — and the
//! answer writes nothing the download would (T2b).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::EventFilter;
use serde_json::Value;

const REG: &str = "local-cargo";

async fn app() -> (impl TestService, LocalRegistryAppParts) {
    let parts = local_registry_app_parts(REG, "cargo", RegistryMode::Local, None);
    let kept = LocalRegistryAppParts {
        proxy_svc: parts.proxy_svc.clone(),
        admin_svc: parts.admin_svc.clone(),
        token_repo: parts.token_repo.clone(),
        access_config: parts.access_config.clone(),
        registry_map: parts.registry_map.clone(),
        local_svc: parts.local_svc.clone(),
        mode_map: parts.mode_map.clone(),
    };
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, kept)
}

async fn publish<S: TestService>(app: &S, name: &str) {
    let resp = call_service(
        app,
        TestRequest::put()
            .uri(&format!("/proxy/{REG}/api/v1/crates/new"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_publish_payload(name, "0.1.0"))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "publishing {name}");
}

/// `(status, body)` of the pullability answer, as `token`.
async fn ask<S: TestService>(app: &S, name: &str, token: &str) -> (u16, Value, Vec<u8>) {
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!(
                "/api/v1/packages/access?registry={REG}&name={name}&version=0.1.0"
            ))
            .insert_header(("Authorization", bearer(token)))
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    let bytes = read_body(resp).await.to_vec();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        bytes,
    )
}

async fn download<S: TestService>(app: &S, name: &str, token: &str) -> u16 {
    call_service(
        app,
        TestRequest::get()
            .uri(&format!("/proxy/{REG}/{name}/0.1.0/download"))
            .insert_header(("Authorization", bearer(token)))
            .to_request(),
    )
    .await
    .status()
    .as_u16()
}

#[actix_web::test]
async fn a_served_version_is_pullable_and_says_so() {
    let (app, _) = app().await;
    publish(&app, "fine").await;
    let (status, body, _) = ask(&app, "fine", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["headline"], "pullable", "{body}");
    assert_eq!(body["can_access"], true);
    let kinds: Vec<&str> = body["causes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["kind"].as_str())
        .collect();
    assert_eq!(
        kinds.first(),
        Some(&"access"),
        "the server's order: {kinds:?}"
    );
    assert!(
        kinds.contains(&"block") && kinds.contains(&"lifecycle"),
        "{kinds:?}"
    );
    assert_eq!(download(&app, "fine", USER_TOKEN).await, 200);
}

/// A block refuses both, and the answer's code is the block's.
#[actix_web::test]
async fn a_blocked_version_is_refused_by_both() {
    let (app, _) = app().await;
    publish(&app, "held").await;
    let blocked = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/packages/block")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({
                "registry": REG, "name": "held", "version": "0.1.0", "reason": "SEC-42 internal"
            }))
            .to_request(),
    )
    .await;
    assert!(blocked.status().is_success(), "{}", blocked.status());

    let (status, body, _) = ask(&app, "held", USER_TOKEN).await;
    assert_eq!(status, 200);
    assert_eq!(body["headline"], "refused", "{body}");
    let first_refusal = body["causes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["state"] == "refuse")
        .unwrap();
    assert_eq!(first_refusal["reason_code"], "BLOCK_LIST");
    // T2: the administrator's note does not reach a non-admin.
    assert!(!body.to_string().contains("SEC-42"), "{body}");
    assert_eq!(download(&app, "held", USER_TOKEN).await, 403);

    // The local funnel records the same code `why --last` reads (decision 39).
    let rows: Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri("/api/v1/me/downloads?outcome=denied")
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    let row = rows
        .as_array()
        .into_iter()
        .flatten()
        .chain(rows["items"].as_array().into_iter().flatten())
        .find(|r| r["name"] == "held")
        .unwrap_or_else(|| panic!("no refusal row: {rows}"));
    assert_eq!(row["reason_code"], "BLOCK_LIST", "{rows}");
}

/// A yank is a warning, not a refusal — and the exact version still downloads.
#[actix_web::test]
async fn a_yanked_version_warns_and_still_downloads_by_exact_version() {
    let (app, _) = app().await;
    publish(&app, "gone").await;
    let yanked = call_service(
        &app,
        TestRequest::delete()
            .uri(&format!("/proxy/{REG}/api/v1/crates/gone/0.1.0/yank"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(yanked.status(), 200);
    let (_, body, _) = ask(&app, "gone", USER_TOKEN).await;
    assert_eq!(body["headline"], "pullable", "{body}");
    let lifecycle = body["causes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "lifecycle")
        .unwrap();
    assert_eq!(lifecycle["state"], "warn");
    assert_eq!(download(&app, "gone", USER_TOKEN).await, 200);
}

/// T1: an unknown version and a registry the caller may not see answer the
/// same 404, byte for byte.
#[actix_web::test]
async fn invisible_and_unknown_are_the_same_not_found() {
    let (app, _) = app().await;
    let (unknown, _, unknown_body) = ask(&app, "never-published", USER_TOKEN).await;
    assert_eq!(unknown, 404);
    let hidden = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/packages/access?registry=not-mine&name=x&version=0.1.0")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(hidden.status(), 404);
    assert_eq!(read_body(hidden).await.to_vec(), unknown_body);
}

/// T2b: asking writes nothing — no access-log row, whatever the answer.
#[actix_web::test]
async fn why_writes_nothing() {
    let (app, parts) = app().await;
    publish(&app, "quiet").await;
    let events = || async {
        parts
            .admin_svc
            .repo
            .count_events(EventFilter::default())
            .await
            .unwrap()
    };
    let before = events().await;
    for _ in 0..3 {
        let (status, _, _) = ask(&app, "quiet", USER_TOKEN).await;
        assert_eq!(status, 200);
    }
    ask(&app, "never-published", USER_TOKEN).await;
    assert_eq!(
        events().await,
        before,
        "a pullability answer recorded an access event"
    );
}

/// Without a version, the answer explains resolution: which version a client
/// receives, with the newest versions' answers.
#[actix_web::test]
async fn no_version_explains_resolution() {
    let (app, _) = app().await;
    publish(&app, "many").await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!("/api/v1/packages/access?registry={REG}&name=many"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["resolved_version"], "0.1.0", "{body}");
    assert_eq!(body["versions"].as_array().map(Vec::len), Some(1), "{body}");
}

/// A registry opened only by `[registries.grants]` — the legacy role table
/// lists it for nobody — is visible to `why` exactly as it is to the download.
/// `why` once read the legacy table alone and answered a pullable package 404.
#[actix_web::test]
async fn a_registry_opened_by_grants_alone_is_visible() {
    use batlehub_core::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };
    let (app, parts) = app().await;
    publish(&app, "granted").await;
    *parts.access_config.write().await = access_config(&[], &[]).read().await.clone();
    parts.proxy_svc.hot.write().await.grants = [(
        REG.to_owned(),
        std::sync::Arc::new(RegistryGrants {
            kind: RegistryKind::Cargo,
            registry: Node::new(
                Tier::Registry,
                format!("registry:{REG}"),
                Some(GrantMap::new().grant(
                    SubjectMatcher::Anyone,
                    vec![
                        Action::ReleasesRead,
                        Action::ReleasesList,
                        Action::SourceRead,
                    ],
                )),
            ),
            namespaces: Vec::new(),
        }),
    )]
    .into();

    assert_eq!(download(&app, "granted", USER_TOKEN).await, 200);
    let (status, body, _) = ask(&app, "granted", USER_TOKEN).await;
    assert_eq!(
        status, 200,
        "the download serves it, so why must see it: {body}"
    );
    assert_eq!(body["headline"], "pullable", "{body}");
    // Still no oracle: an unknown name in the same registry is a 404.
    assert_eq!(ask(&app, "never-published", USER_TOKEN).await.0, 404);
}
