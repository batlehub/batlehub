//! `GET /api/v1/registries` reports `installable` from the grants that enforce
//! reads (RFC 0038 §4.4: `setup apply` never writes a URL that will 401).
//!
//! Two registries, both npm:
//!   "npm"  — legacy tiers: anonymous = ["releases:read"],
//!            user = ["releases:read", "source:read"]. Listed for everyone,
//!            but an anonymous tarball fetch (`source:read`) is refused.
//!   "open" — no legacy access below admin, so `accessible_registries_for`
//!            hides it; `[registries.grants]` opens it to `*`.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use serde_json::Value;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{
    Action, GrantMap, Node, RegistryGrants, RegistryKind, SubjectMatcher, Tier,
};
use batlehub_core::ports::{
    CacheStore, PackageRepository, RegistryClient, StorageBackend, UserTokenRepository,
};
use batlehub_core::services::{
    new_hot_lock, AdminService, HotConfig, HotConfigLock, ProxyMetrics, ProxyService,
    RegistryPolicy,
};
use batlehub_web::{new_access_lock, RegistryModeMap};

fn open_grants(actions: &[Action]) -> Arc<RegistryGrants> {
    Arc::new(RegistryGrants {
        kind: RegistryKind::Npm,
        registry: Node::new(
            Tier::Registry,
            "registry:open",
            Some(GrantMap::new().grant(SubjectMatcher::Anyone, actions.to_vec())),
        ),
        namespaces: Vec::new(),
    })
}

async fn app() -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    HotConfigLock,
) {
    let repo = InMemoryRepo::new();
    let repo_dyn: Arc<dyn PackageRepository> = repo;
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let names = ["npm", "open"];
    let registries: HashMap<String, Arc<dyn RegistryClient>> = names
        .iter()
        .map(|n| {
            (
                n.to_string(),
                FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
            )
        })
        .collect();
    let policies: HashMap<String, Arc<RegistryPolicy>> = names
        .iter()
        .map(|n| (n.to_string(), Arc::new(rbac_policy(repo_dyn.clone()).0)))
        .collect();
    let grants = HashMap::from([
        (
            "npm".to_owned(),
            Arc::new(fixture_grants(
                "npm",
                "npm",
                &RegistryMode::Proxy,
                &rbac_policy_perms(),
            )),
        ),
        (
            "open".to_owned(),
            open_grants(&[
                Action::ReleasesList,
                Action::ReleasesRead,
                Action::SourceRead,
            ]),
        ),
    ]);
    let hot = new_hot_lock(HotConfig {
        instance: Some(Arc::new(
            batlehub_core::services::authz::translate::instance_node(None),
        )),
        registries,
        policies,
        grants,
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
    // What `build_access_config` makes of the legacy tiers: "open" has only
    // `rbac.admin`, so only an admin's legacy set contains it.
    let set = |v: &[&str]| v.iter().map(ToString::to_string).collect();
    let access = new_access_lock(batlehub_web::AccessConfig {
        anonymous: set(&["npm"]),
        user: set(&["npm"]),
        admin: set(&["npm", "open"]),
        groups: HashMap::new(),
        explore_anonymous: set(&["npm"]),
        explore_user: set(&["npm"]),
        explore_admin: set(&["npm", "open"]),
    });
    let app = finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access,
        registry_map_for(&[("npm", "npm"), ("open", "npm")]),
        local_svc,
        RegistryModeMap::default(),
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        test_auth_providers(),
    )
    .await;
    (app, hot)
}

/// name → installable, asserting the field is present on every entry.
async fn listing<S>(app: &S, token: Option<&str>) -> HashMap<String, bool>
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let mut req = TestRequest::get().uri("/api/v1/registries");
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    let resp = call_service(app, req.to_request()).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    body.as_array()
        .expect("an array")
        .iter()
        .map(|r| {
            (
                r["name"].as_str().expect("name").to_owned(),
                r["installable"]
                    .as_bool()
                    .unwrap_or_else(|| panic!("`installable` always serialised: {r}")),
            )
        })
        .collect()
}

/// Scenario 1: listed by the legacy filter, but the tarball needs
/// `source:read`, which anonymous does not hold — so not installable.
/// Scenario 2: hidden by the legacy filter, opened by grants — listed and
/// installable.
#[actix_web::test]
async fn anonymous_sees_the_grants_truth() {
    let (app, _) = app().await;
    let got = listing(&app, None).await;
    assert_eq!(
        got,
        HashMap::from([("npm".to_owned(), false), ("open".to_owned(), true)])
    );
}

/// A user holds `source:read` on "npm" through the legacy tiers.
#[actix_web::test]
async fn an_authenticated_user_can_install_from_both() {
    let (app, _) = app().await;
    let got = listing(&app, Some(USER_TOKEN)).await;
    assert_eq!(
        got,
        HashMap::from([("npm".to_owned(), true), ("open".to_owned(), true)])
    );
}

/// The answer follows a reload of the grants, not a map built at startup.
#[actix_web::test]
async fn a_grant_reload_changes_the_answer() {
    let (app, hot) = app().await;
    // `open` now lets anyone read metadata but not the tarball.
    hot.write().await.grants.insert(
        "open".to_owned(),
        open_grants(&[Action::ReleasesList, Action::ReleasesRead]),
    );
    let got = listing(&app, None).await;
    assert_eq!(got, HashMap::from([("npm".to_owned(), false)]));
    // An admin still sees it through the legacy filter — the console's view
    // is not shrunk — but these grants give nobody `source:read` on it.
    let admin = listing(&app, Some(ADMIN_TOKEN)).await;
    assert_eq!(admin.get("open"), Some(&false), "{admin:?}");
}
