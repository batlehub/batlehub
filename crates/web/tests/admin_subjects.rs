//! Integration tests for `GET /api/v1/admin/subjects` (RFC 0004-bis A8).
//!
//! The gap this endpoint closes is the only one in the RFC whose absence was
//! *invisible*: a subject field with no source does not error, it returns an
//! empty result that reads as an answer. So the assertions here are mostly
//! about the substring match — an operator typing `alice` on an instance that
//! stores `oidc:alice` is the exact case the audit log answered with a
//! confident empty table.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use chrono::Utc;
use serde_json::Value;

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_core::{
    entities::{AccessEvent, AccessResult, PackageId},
    ports::{PackageRepository, UserBlockRepository},
};

/// One allowed download by `user_id`, so the access log has a subject in it.
async fn record_pull(repo: &Arc<InMemoryRepo>, user_id: &str) {
    repo.record_access(AccessEvent {
        id: uuid::Uuid::new_v4(),
        timestamp: Utc::now(),
        user_id: Some(user_id.to_owned()),
        package_id: Some(PackageId::new("npm", "lodash", "1.0.0")),
        user_role: batlehub_core::entities::Role::User,
        action: batlehub_core::entities::AccessAction::Download,
        result: AccessResult::Allowed,
        ip_address: None,
        user_agent: None,
        throttled_count: None,
        detail: None,
    })
    .await
    .unwrap();
}

async fn get_subjects<S>(app: &S, query: &str) -> Value
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::get()
        .uri(&format!("/api/v1/admin/subjects{query}"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200);
    read_body_json(resp).await
}

fn ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["user_id"].as_str().unwrap().to_owned())
        .collect()
}

#[actix_web::test]
async fn non_admin_is_forbidden() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/subjects")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn lists_subjects_the_audit_log_has_seen() {
    let repo = InMemoryRepo::new();
    record_pull(&repo, "oidc:alice").await;
    record_pull(&repo, "oidc:bob").await;

    let app = make_app(repo).await;
    let body = get_subjects(&app, "").await;

    let found = ids(&body);
    assert!(found.contains(&"oidc:alice".to_owned()));
    assert!(found.contains(&"oidc:bob".to_owned()));
    assert_eq!(body["items"][0]["sources"][0], "audit");
}

/// The case the endpoint exists for.
///
/// Typing `alice` against an instance that stores `oidc:alice` returned an
/// empty audit table, which reads exactly like "this user did nothing" — on the
/// surface whose entire purpose is establishing what someone did.
#[actix_web::test]
async fn a_bare_name_matches_a_prefixed_subject() {
    let repo = InMemoryRepo::new();
    record_pull(&repo, "oidc:alice").await;

    let app = make_app(repo).await;
    assert_eq!(ids(&get_subjects(&app, "?q=alice").await), ["oidc:alice"]);
}

#[actix_web::test]
async fn the_match_is_case_insensitive() {
    let repo = InMemoryRepo::new();
    record_pull(&repo, "oidc:Alice").await;

    let app = make_app(repo).await;
    assert_eq!(ids(&get_subjects(&app, "?q=ALICE").await), ["oidc:Alice"]);
}

#[actix_web::test]
async fn a_query_matching_nothing_returns_an_empty_list_not_an_error() {
    let repo = InMemoryRepo::new();
    record_pull(&repo, "oidc:alice").await;

    let app = make_app(repo).await;
    let body = get_subjects(&app, "?q=nobody").await;
    assert_eq!(ids(&body), Vec::<String>::new());
    assert_eq!(body["truncated"], false);
}

#[actix_web::test]
async fn a_blocked_account_is_a_known_subject_even_with_no_activity() {
    let blocks = Arc::new(InMemoryUserBlockRepository::new()) as Arc<dyn UserBlockRepository>;
    blocks.block("oidc:mallory", "admin", None).await.unwrap();

    let app = make_app_with_blocks(blocks, Arc::new(InMemoryIpBlockStore::new())).await;
    let body = get_subjects(&app, "").await;

    assert_eq!(ids(&body), ["oidc:mallory"]);
    assert_eq!(body["items"][0]["sources"][0], "blocked");
}

/// A subject seen in two places lists both, so an operator can tell a namespace
/// owner who has never pulled anything from an account that only ever pulled.
#[actix_web::test]
async fn a_subject_seen_twice_lists_both_sources() {
    let repo = InMemoryRepo::new();
    record_pull(&repo, "oidc:alice").await;
    let blocks = Arc::new(InMemoryUserBlockRepository::new()) as Arc<dyn UserBlockRepository>;
    blocks.block("oidc:alice", "admin", None).await.unwrap();

    let app = make_app_with_defaults(
        repo,
        ConfigureAppDefaults {
            user_block_repo: blocks,
            ..Default::default()
        },
    )
    .await;
    let body = get_subjects(&app, "").await;

    assert_eq!(ids(&body), ["oidc:alice"]);
    let sources: Vec<&str> = body["items"][0]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(sources, ["audit", "blocked"]);
}

#[actix_web::test]
async fn limit_is_honoured_and_truncation_is_reported() {
    let repo = InMemoryRepo::new();
    for i in 0..5 {
        record_pull(&repo, &format!("oidc:user{i}")).await;
    }

    let app = make_app(repo).await;
    let body = get_subjects(&app, "?limit=2").await;

    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    // Stated, not implied: a field that shows two of five must be able to say
    // "keep typing" rather than let the operator read it as the whole answer.
    assert_eq!(body["truncated"], true);
}

#[actix_web::test]
async fn a_duplicate_subject_appears_once() {
    let repo = InMemoryRepo::new();
    for _ in 0..3 {
        record_pull(&repo, "oidc:alice").await;
    }

    let app = make_app(repo).await;
    assert_eq!(ids(&get_subjects(&app, "").await), ["oidc:alice"]);
}
