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

/// A beta-channel version is as invisible to `why` as it is to the download:
/// a refusal would confirm to a non-member that the pre-release exists.
#[actix_web::test]
async fn a_beta_version_is_not_found_for_a_non_member() {
    use batlehub_core::ports::BetaChannelPort;
    let (app, parts) = app().await;
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri(&format!("/proxy/{REG}/api/v1/crates/new"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_payload(make_publish_payload("beta", "0.2.0-beta.1"))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let beta: std::sync::Arc<dyn BetaChannelPort> =
        batlehub_adapters::in_memory::InMemoryBetaChannelStore::new();
    parts
        .proxy_svc
        .hot
        .write()
        .await
        .beta_channel
        .insert(REG.to_owned(), beta);

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!(
                "/api/v1/packages/access?registry={REG}&name=beta&version=0.2.0-beta.1"
            ))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

/// A crate download asks `source:read`: a caller granted only `releases:read`
/// is refused it, so `why` must not call the crate pullable.
#[actix_web::test]
async fn why_judges_the_action_the_download_asks() {
    use batlehub_core::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };
    let (app, parts) = app().await;
    publish(&app, "src-only").await;
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
                    vec![Action::ReleasesRead, Action::ReleasesList],
                )),
            ),
            namespaces: Vec::new(),
        }),
    )]
    .into();

    assert_ne!(download(&app, "src-only", USER_TOKEN).await, 200);
    let (status, body, _) = ask(&app, "src-only", USER_TOKEN).await;
    assert_ne!(body["headline"], "pullable", "{status} {body}");
}

/// Resolution checks visibility before reading anything: an anonymous caller
/// on a registry closed to it gets the not-found an unknown name gets.
#[actix_web::test]
async fn resolution_on_a_closed_registry_is_not_found_for_anonymous() {
    let (app, _) = app().await;
    publish(&app, "closed").await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!(
                "/api/v1/packages/access?registry={REG}&name=closed"
            ))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

/// A read granted on one package alone — a stored package-tier grant, on a
/// registry whose own tier grants the caller nothing — opens the download, so
/// `why` sees the package too. It used to answer 404 for something that
/// installs; the next package, ungranted, still gets the same 404.
#[actix_web::test]
async fn a_package_tier_grant_alone_makes_the_package_visible() {
    use batlehub_core::entities::{
        Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
    };
    use batlehub_core::ports::{GrantRepository, NodeKind, StoredGrant};
    let (app, parts) = app().await;
    publish(&app, "pkgonly").await;
    publish(&app, "other").await;
    *parts.access_config.write().await = access_config(&[], &[]).read().await.clone();
    let repo = batlehub_adapters::in_memory::InMemoryGrantRepository::new();
    repo.put_grant(StoredGrant {
        registry: REG.to_owned(),
        node_kind: NodeKind::Package,
        node_key: "pkgonly".to_owned(),
        subject: SubjectMatcher::User("user-1".to_owned()),
        actions: vec![
            Action::ReleasesRead,
            Action::ReleasesList,
            Action::SourceRead,
        ],
        granted_by: None,
    })
    .await
    .unwrap();
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.grant_repo = Some(repo as std::sync::Arc<dyn GrantRepository>);
        hot.grants = [(
            REG.to_owned(),
            std::sync::Arc::new(RegistryGrants {
                kind: RegistryKind::Cargo,
                registry: Node::new(
                    Tier::Registry,
                    format!("registry:{REG}"),
                    Some(GrantMap::new()),
                ),
                namespaces: Vec::new(),
            }),
        )]
        .into();
    }

    assert_eq!(download(&app, "pkgonly", USER_TOKEN).await, 200);
    let (status, body, _) = ask(&app, "pkgonly", USER_TOKEN).await;
    assert_eq!(status, 200, "the download serves it: {body}");
    assert_eq!(body["headline"], "pullable", "{body}");
    assert_eq!(ask(&app, "other", USER_TOKEN).await.0, 404);
}

// ── The batch behind `check` (RFC 0040 §6.1, decision 37) ───────────────────

const PROXY: &str = "proxy-cargo";

/// A proxy registry whose upstream (`FixedRegistry`) knows every version, so
/// a `wait` "not yet resolved" can only mean the batch did not ask it.
async fn proxy_app(
    rate_limit: Option<std::sync::Arc<batlehub_web::RateLimitService>>,
) -> impl TestService {
    let parts = local_registry_app_parts(PROXY, "cargo", RegistryMode::Proxy, None);
    build_local_registry_app_with_defaults(
        parts,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            rate_limit,
            ..Default::default()
        },
    )
    .await
}

async fn batch<S: TestService>(app: &S, items: Value) -> (u16, Value) {
    let resp = call_service(
        app,
        TestRequest::post()
            .uri("/api/v1/packages/access/batch")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_json(serde_json::json!({ "items": items }))
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    (status, read_body_json(resp).await)
}

fn item(registry: &str, name: &str) -> Value {
    serde_json::json!({"registry": registry, "name": name, "version": "1.0.0"})
}

/// T11: the batch never goes upstream — an uncached version waits as "not
/// yet resolved", names `why` as the next step, and answers once the single
/// call has resolved it — and it takes at most 50 items. T1: an unknown and an
/// invisible coordinate are the same `not_found`.
#[actix_web::test]
async fn batch_is_cache_only_and_capped() {
    let app = proxy_app(None).await;

    let (status, _) = batch(&app, Value::Array(vec![item(PROXY, "serde"); 51])).await;
    assert_eq!(status, 400, "a batch of 51 is refused");

    let (status, body) = batch(
        &app,
        serde_json::json!([
            item(PROXY, "serde"),
            item("not-mine", "x"),
            item(PROXY, "serde")
        ]),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let first = &body["items"][0];
    assert_eq!(first["headline"], "waiting", "{body}");
    let upstream = first["causes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "upstream")
        .expect("an upstream cause");
    assert_eq!(upstream["state"], "wait");
    assert_eq!(
        upstream["next"],
        format!("batlehub-cli why {PROXY}/serde@1.0.0")
    );
    assert_eq!(body["items"][1], serde_json::json!({"state": "not_found"}));
    assert_eq!(
        body["items"][2]["headline"], "waiting",
        "request order kept"
    );

    // The single call resolves it — and fills the cache the batch reads.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!(
                "/api/v1/packages/access?registry={PROXY}&name=serde&version=1.0.0"
            ))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let (_, body) = batch(&app, serde_json::json!([item(PROXY, "serde")])).await;
    assert_eq!(body["items"][0]["headline"], "pullable", "{body}");
}

/// T11: a batch of 50 costs what 50 downloads cost — the next item over the
/// registry's limit is a `429`.
#[actix_web::test]
async fn batch_charges_the_bucket_once_per_item() {
    use batlehub_config::schema::{RateLimitConfig, RateLimitEnforcement};
    let configs = std::collections::HashMap::from([(
        PROXY.to_owned(),
        RateLimitConfig {
            requests_per_window: 50,
            window_secs: 3600,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![],
        },
    )]);
    let rl = std::sync::Arc::new(batlehub_web::RateLimitService::new(
        &configs,
        std::sync::Arc::new(batlehub_adapters::rate_limit::InMemoryRateLimitStore::new()),
    ));
    let app = proxy_app(Some(rl)).await;
    let (status, _) = batch(&app, Value::Array(vec![item(PROXY, "serde"); 50])).await;
    assert_eq!(status, 200);
    let (status, _) = batch(&app, serde_json::json!([item(PROXY, "serde")])).await;
    assert_eq!(status, 429, "the 51st item is over the limit");
}
