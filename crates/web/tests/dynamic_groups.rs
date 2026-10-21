//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::auth::StaticTokenAuthProvider;
use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    entities::Role,
    ports::{
        AuthProvider, CacheStore, PackageRepository, RegistryClient, StorageBackend,
        UserTokenRepository,
    },
    rules::{BlockListRule, RbacRule},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy},
};
use batlehub_web::{new_access_lock, RegistryModeMap};

// ── Dynamic group tests ───────────────────────────────────────────────────────
//
// Scenario:
//   "github"  — normal registry; anonymous=["releases:read"], user=[...,"source:read"]
//   "github2" — group-restricted registry; anonymous=[], user=[], admin=["*"]
//               team-a = ["releases:read","source:read"]
//               team-b = ["releases:read"]
//
// Tokens:
//   TEAM_A_TOKEN  → anonymous role, groups=["team-a"]
//   TEAM_B_TOKEN  → anonymous role, groups=["team-b"]
//   TEAM_AB_TOKEN → anonymous role, groups=["team-a","team-b"]

fn group_auth_providers() -> Vec<Arc<dyn AuthProvider>> {
    vec![Arc::new(
        StaticTokenAuthProvider::new([
            (
                ADMIN_TOKEN.to_owned(),
                Some("admin".to_owned()),
                Role::Admin,
            ),
            (USER_TOKEN.to_owned(), Some("user-1".to_owned()), Role::User),
        ])
        .with_group_entries([
            (
                TEAM_A_TOKEN.to_owned(),
                Some("team-a-user".to_owned()),
                Role::Anonymous,
                vec!["team-a".to_owned()],
            ),
            (
                TEAM_B_TOKEN.to_owned(),
                Some("team-b-user".to_owned()),
                Role::Anonymous,
                vec!["team-b".to_owned()],
            ),
            (
                TEAM_AB_TOKEN.to_owned(),
                Some("team-ab-user".to_owned()),
                Role::Anonymous,
                vec!["team-a".to_owned(), "team-b".to_owned()],
            ),
        ]),
    )]
}

fn rbac_policy_group_registry(repo: Arc<dyn PackageRepository>) -> RegistryPolicy {
    let perms = HashMap::from([
        (Role::Anonymous, vec![]),
        (Role::User, vec![]),
        (Role::Admin, vec!["*".to_owned()]),
    ]);
    let group_perms = HashMap::from([
        (
            "team-a".to_owned(),
            vec!["releases:read".to_owned(), "source:read".to_owned()],
        ),
        ("team-b".to_owned(), vec!["releases:read".to_owned()]),
    ]);
    RegistryPolicy {
        metadata_ttl: Some(Duration::from_secs(300)),
        firewall_only: false,
        serve_stale_metadata: false,
        artifact_ttl: None,
        rules: vec![
            Box::new(
                RbacRule::from_patterns(perms)
                    .expect("fixture rbac patterns are valid")
                    .with_group_patterns(group_perms)
                    .expect("fixture group patterns are valid"),
            ),
            Box::new(BlockListRule::new(repo)),
        ],
    }
}

async fn make_group_app(
    repo: Arc<InMemoryRepo>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [
        (
            "github".to_owned(),
            FixedRegistry::new("github") as Arc<dyn RegistryClient>,
        ),
        (
            "github2".to_owned(),
            FixedRegistry::new("github") as Arc<dyn RegistryClient>,
        ),
    ]
    .into();

    let policies: HashMap<String, Arc<RegistryPolicy>> = [
        (
            "github".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
        (
            "github2".to_owned(),
            Arc::new(rbac_policy_group_registry(repo_dyn.clone())),
        ),
    ]
    .into();

    // §5.1 — grant resolution, not the fixture's own `RbacRule`, is what refuses
    // here. Derived from the same permissions the policies above were built from,
    // so the two cannot disagree: §13.5 records a commit where they did and the
    // whole suite stayed green while testing a path production had stopped taking.
    let group_perms = RbacFixture {
        roles: HashMap::from([
            (Role::Anonymous, vec![]),
            (Role::User, vec![]),
            (Role::Admin, vec!["*".to_owned()]),
        ]),
        groups: HashMap::from([
            (
                "team-a".to_owned(),
                vec!["releases:read".to_owned(), "source:read".to_owned()],
            ),
            ("team-b".to_owned(), vec!["releases:read".to_owned()]),
        ]),
    };
    let grants: HashMap<String, Arc<batlehub_core::entities::RegistryGrants>> = [
        (
            "github".to_owned(),
            Arc::new(fixture_grants(
                "github",
                "github",
                &batlehub_config::schema::RegistryMode::Proxy,
                &rbac_policy_perms(),
            )),
        ),
        (
            "github2".to_owned(),
            Arc::new(fixture_grants(
                "github2",
                "github",
                &batlehub_config::schema::RegistryMode::Proxy,
                &group_perms,
            )),
        ),
    ]
    .into();

    let hot = new_hot_lock(HotConfig {
        grants,
        // RFC 0015 §4.2's instance tier, wired exactly as production wires it:
        // `instance_node` is §10 rule 5's own translation, so the fixture's admin
        // holds the control verbs and nobody else does. Without it every
        // `require_verb` on a control endpoint refuses, including the admin the
        // suite is asserting about — a fixture that does not build the model
        // tests a server nobody runs (§13.5).
        instance: Some(std::sync::Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);

    // github: everyone can access (role-based)
    // github2: only accessible via group membership (no role-based access for anon/user)
    let access_config = new_access_lock(batlehub_web::AccessConfig {
        anonymous: ["github"].iter().map(ToString::to_string).collect(),
        user: ["github"].iter().map(ToString::to_string).collect(),
        admin: ["github", "github2"]
            .iter()
            .map(ToString::to_string)
            .collect(),
        groups: [
            (
                "team-a".to_owned(),
                ["github2"].iter().map(ToString::to_string).collect(),
            ),
            (
                "team-b".to_owned(),
                ["github2"].iter().map(ToString::to_string).collect(),
            ),
        ]
        .into_iter()
        .collect(),
        explore_anonymous: std::collections::HashSet::new(),
        explore_user: std::collections::HashSet::new(),
        explore_admin: std::collections::HashSet::new(),
    });
    let registry_map = registry_map_for(&[("github", "github"), ("github2", "github")]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();
    finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        cargo_indexes,
        ConfigureAppDefaults::default(),
        group_auth_providers(),
    )
    .await
}

// ── /api/v1/registries with groups ───────────────────────────────────────────

#[actix_web::test]
async fn group_member_sees_group_restricted_registry_in_listing() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("Authorization", bearer(TEAM_A_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"github2"), "team-a should see github2");
    assert!(
        names.contains(&"github"),
        "team-a should also see role-based github"
    );
}

#[actix_web::test]
async fn user_without_group_cannot_see_group_restricted_registry() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        !names.contains(&"github2"),
        "user without group should not see github2"
    );
    assert!(names.contains(&"github"));
}

#[actix_web::test]
async fn anonymous_without_group_cannot_see_group_restricted_registry() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get().uri("/api/v1/registries").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        !names.contains(&"github2"),
        "anonymous should not see github2"
    );
}

#[actix_web::test]
async fn multi_group_user_sees_union_of_registries() {
    let app = make_group_app(InMemoryRepo::new()).await;
    // team-ab has both groups → should see github and github2
    let req = TestRequest::get()
        .uri("/api/v1/registries")
        .insert_header(("Authorization", bearer(TEAM_AB_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(
        names.contains(&"github"),
        "team-ab should see github (anonymous role)"
    );
    assert!(
        names.contains(&"github2"),
        "team-ab should see github2 (team-a or team-b group)"
    );
}

// ── Proxy access with group permissions ──────────────────────────────────────

#[actix_web::test]
async fn group_member_can_list_releases_from_group_registry() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github2/rust-lang/rust/releases")
        .insert_header(("Authorization", bearer(TEAM_A_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn group_member_can_download_tarball_from_group_registry() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github2/rust-lang/rust/tarball/v1.80.0")
        .insert_header(("Authorization", bearer(TEAM_A_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn team_b_can_read_releases_but_not_source_on_group_registry() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let releases_req = TestRequest::get()
        .uri("/proxy/github2/rust-lang/rust/releases")
        .insert_header(("Authorization", bearer(TEAM_B_TOKEN)))
        .to_request();
    let releases_resp = call_service(&app, releases_req).await;
    assert_eq!(releases_resp.status(), 200, "team-b can releases:read");

    let tarball_req = TestRequest::get()
        .uri("/proxy/github2/rust-lang/rust/tarball/v1.80.0")
        .insert_header(("Authorization", bearer(TEAM_B_TOKEN)))
        .to_request();
    let tarball_resp = call_service(&app, tarball_req).await;
    assert_eq!(tarball_resp.status(), 403, "team-b cannot source:read");
}

#[actix_web::test]
async fn user_without_group_denied_group_registry_proxy() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github2/rust-lang/rust/releases")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn anonymous_denied_group_registry_proxy() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/github2/rust-lang/rust/releases")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

// ── /api/v1/me with groups ────────────────────────────────────────────────────

#[actix_web::test]
async fn me_endpoint_returns_groups_for_group_token() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/me")
        .insert_header(("Authorization", bearer(TEAM_A_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["role"], "anonymous");
    let groups: Vec<&str> = body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        groups.contains(&"team-a"),
        "groups field should contain team-a"
    );
    assert_eq!(
        body["has_registry_access"], true,
        "team-a has registry access via group"
    );
}

#[actix_web::test]
async fn me_endpoint_returns_empty_groups_for_regular_token() {
    let app = make_group_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/me")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let groups = body["groups"].as_array().unwrap();
    assert!(
        groups.is_empty(),
        "regular user token should have no groups"
    );
}

// ── Group access recorded in audit log ───────────────────────────────────────

#[actix_web::test]
async fn group_proxy_access_is_recorded_in_audit_log() {
    let repo = InMemoryRepo::new();
    let app = make_group_app(repo.clone()).await;

    // A *download*, not the release listing. Both are group-authorised reads of
    // `github2`, which is what this test is about — but RFC 0006 §4.5
    // deliberately stopped filing an `AccessEvent` for an allowed *listing*
    // (they move a per-registry counter instead, because a dependency-graph
    // resolution is hundreds of listings and no bytes). Downloads still get a
    // row each, so that is where group attribution is observable.
    let req = TestRequest::get()
        .uri("/proxy/github2/rust-lang/rust/tarball/v1.80.0")
        .insert_header(("Authorization", bearer(TEAM_A_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let audit_req = TestRequest::get()
        .uri("/api/v1/admin/audit-log")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let audit_resp = call_service(&app, audit_req).await;
    let body: Value = read_body_json(audit_resp).await;
    let events = body["items"].as_array().unwrap();
    assert!(!events.is_empty(), "group access event should be recorded");
    assert_eq!(events[0]["result"]["outcome"], "allowed");
    assert_eq!(events[0]["package_id"]["registry"], "github2");
    assert_eq!(events[0]["action"], "download");
}
