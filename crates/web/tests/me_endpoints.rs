//! `/api/v1/me/*` — the caller-scoped read paths (RFC 0004 Phase 2).
//!
//! The assertion that matters in this file is **absence**: for every endpoint,
//! two users are seeded and the response to one must not contain the other's
//! rows (RFC 0004 §10). A test that only checks the caller's own rows are
//! present passes just as happily against a handler that returns everybody's.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use actix_web::App;
use chrono::{Duration, Utc};
use serde_json::Value;
use utoipa_actix_web::AppExt;
use uuid::Uuid;

use batlehub_adapters::auth::StaticTokenAuthProvider;
use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryOwnershipStore, InMemoryPackageRepository as InMemoryRepo, InMemoryQuotaRepository,
    InMemoryStorageBackend as InMemoryStorage, InMemoryVulnerabilityRepository,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::local_registry::InMemoryLocalRegistry;
use batlehub_core::{
    entities::{
        AccessAction, AccessEvent, AccessResult, ArtifactVulnerability, Identity, PackageId,
        PublishedPackage, Role, Severity, Visibility,
    },
    ports::{
        AuthProvider, CacheStore, OwnerEntry, OwnershipPort, PackageRepository, QuotaRepository,
        RegistryClient, StorageBackend, UserTokenRepository, VulnerabilityRepository,
    },
    services::{
        new_hot_lock, AdminService, HotConfig, LocalRegistryService, ProxyMetrics, ProxyService,
        QuotaEnforcement, QuotaService, RegistryQuotaConfig,
    },
};
use batlehub_web::{AuthMiddlewareFactory, RegistryModeMap};

const ALICE_TOKEN: &str = "alice-token";
const BOB_TOKEN: &str = "bob-token";

/// Everything the `me` endpoints read, so a test can seed one user's rows and
/// assert the other's are absent from the response.
struct MeApp {
    repo: Arc<InMemoryRepo>,
    vuln: Arc<InMemoryVulnerabilityRepository>,
    ownership: Arc<InMemoryOwnershipStore>,
    quota_repo: Arc<InMemoryQuotaRepository>,
    local: Arc<LocalRegistryService>,
}

fn quota_configs() -> HashMap<String, RegistryQuotaConfig> {
    HashMap::from([
        (
            "npm".to_owned(),
            RegistryQuotaConfig {
                max_storage_bytes_per_user: Some(1_000),
                max_packages_per_user: Some(10),
                warn_threshold: 0.8,
                enforcement: QuotaEnforcement::Block,
            },
        ),
        // Deliberately no entry for "cargo": a registry without a quota must not
        // appear in the response at all.
    ])
}

fn me_auth_providers() -> Vec<Arc<dyn AuthProvider>> {
    vec![Arc::new(StaticTokenAuthProvider::new([
        (ALICE_TOKEN.to_owned(), Some("alice".to_owned()), Role::User),
        (BOB_TOKEN.to_owned(), Some("bob".to_owned()), Role::User),
    ]))]
}

async fn make_me_app(
    parts: &MeApp,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = parts.repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let vuln_dyn: Arc<dyn VulnerabilityRepository> = parts.vuln.clone();

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [
        (
            "npm".to_owned(),
            FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
        ),
        (
            "cargo".to_owned(),
            FixedRegistry::new("cargo") as Arc<dyn RegistryClient>,
        ),
    ]
    .into();

    let proxy_svc = Arc::new(ProxyService {
        hot: new_hot_lock(HotConfig {
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
            policies: HashMap::new(),
            ..Default::default()
        }),
        storage: storage.clone(),
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let admin_svc =
        Arc::new(AdminService::new(repo_dyn).with_vulnerability_repo(Arc::clone(&vuln_dyn)));
    let quota_svc = Arc::new(QuotaService::new(parts.quota_repo.clone(), quota_configs()));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);

    let (app, _) = App::new()
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            admin_svc,
            token_repo,
            access_config_for(&["npm", "cargo"]),
            registry_map_for(&[("npm", "npm"), ("cargo", "cargo")]),
            ConfigureAppDefaults::default(),
        ))
        .split_for_parts();

    let app = app
        .app_data(actix_web::web::Data::new(
            batlehub_web::CargoIndexMap::default(),
        ))
        .app_data(actix_web::web::Data::new(parts.local.clone()))
        .app_data(actix_web::web::Data::new(RegistryModeMap::default()))
        .app_data(actix_web::web::Data::new(quota_svc));

    actix_web::test::init_service(app.wrap(AuthMiddlewareFactory::new(me_auth_providers()))).await
}

fn me_app_parts() -> MeApp {
    let ownership = InMemoryOwnershipStore::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let local = Arc::new(LocalRegistryService {
        backend: Arc::new(InMemoryLocalRegistry::new()),
        storage,
        hot: new_hot_lock(HotConfig::default()),
        quota: None,
        ownership: Some(ownership.clone() as Arc<dyn OwnershipPort>),
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    });
    MeApp {
        repo: InMemoryRepo::new(),
        vuln: Arc::new(InMemoryVulnerabilityRepository::new()),
        ownership,
        quota_repo: InMemoryQuotaRepository::new(),
        local,
    }
}

/// Record a successful download by `user` of `registry/name/version`, `ago_secs`
/// seconds in the past.
async fn seed_download(
    repo: &Arc<InMemoryRepo>,
    user: &str,
    registry: &str,
    name: &str,
    version: &str,
    ago_secs: i64,
) {
    repo.record_access(AccessEvent {
        id: Uuid::new_v4(),
        user_id: Some(user.to_owned()),
        user_role: Role::User,
        package_id: Some(PackageId::new(registry, name, version)),
        action: AccessAction::Download,
        result: AccessResult::Allowed,
        timestamp: Utc::now() - Duration::seconds(ago_secs),
        ip_address: None,
        user_agent: None,
        throttled_count: None,
        detail: None,
    })
    .await
    .unwrap();
}

/// Publish `version` of `registry/name` into the local index, so an owned
/// package resolves to a coordinate the findings table can be keyed by.
async fn publish_version(
    local: &Arc<LocalRegistryService>,
    registry: &str,
    name: &str,
    version: &str,
) {
    local
        .backend
        .publish(PublishedPackage {
            registry: registry.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            checksum: "deadbeef".to_owned(),
            yanked: false,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata: serde_json::json!({}),
            published_at: Utc::now(),
            published_by: None,
            signature_bytes: None,
            signature_type: None,
            visibility: Visibility::Public,
            retention_keep: false,
        })
        .await
        .unwrap();
    // `publish` inserts a *pending* row; `get_versions` only returns published
    // ones. The real publish path calls this after storage succeeds.
    local
        .backend
        .commit_publish(registry, name, version)
        .await
        .unwrap();
}

fn finding(registry: &str, name: &str, version: &str, severity: Severity) -> ArtifactVulnerability {
    ArtifactVulnerability {
        id: Uuid::new_v4(),
        artifact_key: format!("artifact:{registry}/{name}/{version}"),
        registry: registry.to_owned(),
        package_name: name.to_owned(),
        version: version.to_owned(),
        osv_id: format!("OSV-{name}-{version}"),
        severity,
        summary: format!("{name} {version} is affected"),
        fixed_version: Some("9.9.9".to_owned()),
        purl: format!("pkg:{registry}/{name}@{version}"),
        detected_at: Utc::now(),
    }
}

// ── /api/v1/me/downloads ──────────────────────────────────────────────────────

#[actix_web::test]
async fn my_downloads_excludes_another_users_rows() {
    let parts = me_app_parts();
    seed_download(&parts.repo, "alice", "npm", "left-pad", "1.0.0", 10).await;
    seed_download(&parts.repo, "bob", "npm", "bobs-secret-dep", "2.0.0", 5).await;
    let app = make_me_app(&parts).await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/downloads")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);

    let body: Value = read_body_json(resp).await;
    let rows = body.as_array().expect("array response");
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();

    assert!(
        !names.contains(&"bobs-secret-dep"),
        "alice must not see bob's downloads, got {names:?}"
    );
    assert_eq!(names, vec!["left-pad"]);
}

#[actix_web::test]
async fn my_downloads_excludes_denied_and_non_download_events() {
    let parts = me_app_parts();
    seed_download(&parts.repo, "alice", "npm", "allowed-pkg", "1.0.0", 10).await;
    // A denied download and a metadata view, both alice's: neither is a pull.
    parts
        .repo
        .record_access(AccessEvent {
            id: Uuid::new_v4(),
            user_id: Some("alice".to_owned()),
            user_role: Role::User,
            package_id: Some(PackageId::new("npm", "denied-pkg", "1.0.0")),
            action: AccessAction::Download,
            result: AccessResult::Denied {
                reason: "blocked".to_owned(),
            },
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
            throttled_count: None,
            detail: None,
        })
        .await
        .unwrap();
    parts
        .repo
        .record_access(AccessEvent {
            id: Uuid::new_v4(),
            user_id: Some("alice".to_owned()),
            user_role: Role::User,
            package_id: Some(PackageId::new("npm", "viewed-pkg", "1.0.0")),
            action: AccessAction::ViewMetadata,
            result: AccessResult::Allowed,
            timestamp: Utc::now(),
            ip_address: None,
            user_agent: None,
            throttled_count: None,
            detail: None,
        })
        .await
        .unwrap();

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/downloads")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["allowed-pkg"], "got {names:?}");
}

#[actix_web::test]
async fn my_downloads_requires_authentication() {
    let parts = me_app_parts();
    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get().uri("/api/v1/me/downloads").to_request(),
    )
    .await;
    assert_eq!(resp.status(), 401);
}

#[actix_web::test]
async fn my_downloads_clamps_limit() {
    let parts = me_app_parts();
    for i in 0..5 {
        seed_download(&parts.repo, "alice", "npm", &format!("pkg-{i}"), "1.0.0", i).await;
    }
    let app = make_me_app(&parts).await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/downloads?limit=2")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 2);

    // `limit=0` is clamped up to 1 rather than meaning "unlimited".
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/downloads?limit=0")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

// ── /api/v1/me/quota ──────────────────────────────────────────────────────────

#[actix_web::test]
async fn my_quota_reports_only_the_callers_usage() {
    let parts = me_app_parts();
    parts
        .quota_repo
        .record_publish("alice", "npm", 100)
        .await
        .unwrap();
    parts
        .quota_repo
        .record_publish("bob", "npm", 900)
        .await
        .unwrap();

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/quota")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);

    let body: Value = read_body_json(resp).await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1, "only npm has a quota configured");
    assert_eq!(rows[0]["registry"], "npm");
    assert_eq!(
        rows[0]["bytes_used"], 100,
        "alice's own usage, not the sum of everyone's"
    );
    assert_eq!(rows[0]["bytes_limit"], 1_000);
    assert_eq!(rows[0]["packages_used"], 1);
    assert_eq!(rows[0]["state"], "ok");
    assert_eq!(rows[0]["bytes_state"], "ok");
    assert_eq!(rows[0]["packages_state"], "ok");
}

#[actix_web::test]
async fn my_quota_omits_registries_without_a_quota() {
    let parts = me_app_parts();
    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/quota")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    let registries: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["registry"].as_str().unwrap())
        .collect();
    assert!(
        !registries.contains(&"cargo"),
        "cargo has no quota — a meter with no limit measures nothing"
    );
}

#[actix_web::test]
async fn my_quota_reports_the_four_threshold_states() {
    // usage → expected state, against a 1000-byte limit warning at 80%.
    for (bytes, expected) in [
        (0u64, "ok"),
        (799, "ok"),
        (800, "warning"),
        (1_000, "at_limit"),
    ] {
        let parts = me_app_parts();
        if bytes > 0 {
            parts
                .quota_repo
                .record_publish("alice", "npm", bytes)
                .await
                .unwrap();
        }
        let app = make_me_app(&parts).await;
        let resp = call_service(
            &app,
            TestRequest::get()
                .uri("/api/v1/me/quota")
                .insert_header(("Authorization", bearer(ALICE_TOKEN)))
                .to_request(),
        )
        .await;
        let body: Value = read_body_json(resp).await;
        assert_eq!(
            body[0]["state"], expected,
            "{bytes} bytes against a 1000 limit warning at 80%"
        );
        assert_eq!(body[0]["warn_threshold_pct"], 80);
    }
}

/// RFC 0004 §4.2 asks which threshold was crossed, so a reader can tell a
/// registry that is out of *versions* from one that is out of *space*.
#[actix_web::test]
async fn my_quota_states_which_dimension_crossed() {
    let parts = me_app_parts();
    // 9 of 10 versions (90%, past the 80% mark); 100 of 1000 bytes (10%).
    for _ in 0..9 {
        parts
            .quota_repo
            .record_publish("alice", "npm", 100 / 9)
            .await
            .unwrap();
    }

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/quota")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;

    assert_eq!(body[0]["packages_used"], 9);
    assert_eq!(body[0]["packages_state"], "warning");
    assert_eq!(
        body[0]["bytes_state"], "ok",
        "storage is nowhere near its limit and must not be coloured as if it were"
    );
    assert_eq!(body[0]["state"], "warning", "the row takes the worse one");
}

#[actix_web::test]
async fn my_quota_requires_authentication() {
    let parts = me_app_parts();
    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get().uri("/api/v1/me/quota").to_request(),
    )
    .await;
    assert_eq!(resp.status(), 401);
}

// ── /api/v1/me/advisories ─────────────────────────────────────────────────────

#[actix_web::test]
async fn my_advisories_excludes_findings_for_another_users_pulls() {
    let parts = me_app_parts();
    seed_download(&parts.repo, "alice", "npm", "alice-dep", "1.0.0", 10).await;
    seed_download(&parts.repo, "bob", "npm", "bob-dep", "2.0.0", 10).await;
    parts
        .vuln
        .replace_findings_for_artifact(
            "artifact:npm/alice-dep/1.0.0",
            vec![finding("npm", "alice-dep", "1.0.0", Severity::High)],
        )
        .await
        .unwrap();
    parts
        .vuln
        .replace_findings_for_artifact(
            "artifact:npm/bob-dep/2.0.0",
            vec![finding("npm", "bob-dep", "2.0.0", Severity::Critical)],
        )
        .await
        .unwrap();

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/advisories")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);

    let body: Value = read_body_json(resp).await;
    let names: Vec<&str> = body["advisories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"bob-dep"),
        "alice must not learn what bob pulled, got {names:?}"
    );
    assert_eq!(names, vec!["alice-dep"]);
    assert_eq!(body["advisories"][0]["relation"], "pulled");
    assert_eq!(body["advisories"][0]["highest_severity"], "high");
    assert_eq!(body["window_days"], 7);
    assert_eq!(body["scanning_available"], true);
}

#[actix_web::test]
async fn my_advisories_ignores_pulls_older_than_the_window() {
    let parts = me_app_parts();
    // 8 days back — one day outside the 7-day window.
    seed_download(
        &parts.repo,
        "alice",
        "npm",
        "stale-dep",
        "1.0.0",
        8 * 86_400,
    )
    .await;
    parts
        .vuln
        .replace_findings_for_artifact(
            "artifact:npm/stale-dep/1.0.0",
            vec![finding("npm", "stale-dep", "1.0.0", Severity::Critical)],
        )
        .await
        .unwrap();

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/advisories")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    assert!(
        body["advisories"].as_array().unwrap().is_empty(),
        "a pull outside the window is not a recent pull"
    );
}

#[actix_web::test]
async fn my_advisories_labels_owned_packages_and_excludes_other_owners() {
    let parts = me_app_parts();
    // Alice owns npm/alice-lib; bob owns npm/bob-lib. Both have findings.
    parts
        .ownership
        .initialize_owner("npm", "alice-lib", "alice")
        .await
        .unwrap();
    parts
        .ownership
        .initialize_owner("npm", "bob-lib", "bob")
        .await
        .unwrap();
    publish_version(&parts.local, "npm", "alice-lib", "1.2.3").await;
    publish_version(&parts.local, "npm", "bob-lib", "4.5.6").await;
    parts
        .vuln
        .replace_findings_for_artifact(
            "artifact:npm/alice-lib/1.2.3",
            vec![finding("npm", "alice-lib", "1.2.3", Severity::Medium)],
        )
        .await
        .unwrap();
    parts
        .vuln
        .replace_findings_for_artifact(
            "artifact:npm/bob-lib/4.5.6",
            vec![finding("npm", "bob-lib", "4.5.6", Severity::Critical)],
        )
        .await
        .unwrap();

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/advisories")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    let rows = body["advisories"].as_array().unwrap();
    let names: Vec<&str> = rows.iter().map(|a| a["name"].as_str().unwrap()).collect();

    assert!(
        !names.contains(&"bob-lib"),
        "alice must not see advisories for packages she does not own, got {names:?}"
    );
    assert_eq!(names, vec!["alice-lib"]);
    assert_eq!(rows[0]["relation"], "owned");
    assert_eq!(rows[0]["version"], "1.2.3");
}

#[actix_web::test]
async fn my_advisories_prefers_the_owned_label_over_pulled() {
    let parts = me_app_parts();
    parts
        .ownership
        .initialize_owner("npm", "mine", "alice")
        .await
        .unwrap();
    publish_version(&parts.local, "npm", "mine", "1.0.0").await;
    seed_download(&parts.repo, "alice", "npm", "mine", "1.0.0", 10).await;
    parts
        .vuln
        .replace_findings_for_artifact(
            "artifact:npm/mine/1.0.0",
            vec![finding("npm", "mine", "1.0.0", Severity::Low)],
        )
        .await
        .unwrap();

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/advisories")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    let rows = body["advisories"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one row, not one per relationship");
    assert_eq!(
        rows[0]["relation"], "owned",
        "'you can fix this' outranks 'you are exposed to this'"
    );
}

#[actix_web::test]
async fn my_advisories_keeps_a_pulled_version_separate_from_the_owned_one() {
    let parts = me_app_parts();
    // Alice owns `lib` (latest 2.0.0) and also pulled the older 1.0.0. Both
    // versions are affected, and both concern her differently: she can fix the
    // one she owns, and she is running the one she pulled (RFC 0004 R15).
    parts
        .ownership
        .initialize_owner("npm", "lib", "alice")
        .await
        .unwrap();
    publish_version(&parts.local, "npm", "lib", "1.0.0").await;
    publish_version(&parts.local, "npm", "lib", "2.0.0").await;
    seed_download(&parts.repo, "alice", "npm", "lib", "1.0.0", 10).await;

    for version in ["1.0.0", "2.0.0"] {
        parts
            .vuln
            .replace_findings_for_artifact(
                &format!("artifact:npm/lib/{version}"),
                vec![finding("npm", "lib", version, Severity::High)],
            )
            .await
            .unwrap();
    }

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/advisories")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    let rows = body["advisories"].as_array().unwrap();

    let mut seen: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r["version"].as_str().unwrap(),
                r["relation"].as_str().unwrap(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![("1.0.0", "pulled"), ("2.0.0", "owned")],
        "the version you run and the version you maintain are two rows"
    );
}

#[actix_web::test]
async fn my_advisories_group_ownership_is_honoured() {
    let parts = me_app_parts();
    parts
        .ownership
        .add_owner(
            "npm",
            "team-lib",
            OwnerEntry {
                principal_type: "group".to_owned(),
                principal_id: "team-a".to_owned(),
                role: "admin".to_owned(),
                granted_by: None,
            },
        )
        .await
        .unwrap();

    let identity = Identity {
        user_id: Some("alice".to_owned()),
        role: Role::User,
        auth_provider: None,
        groups: vec!["team-a".to_owned()],
    };
    let owned = parts.ownership.list_owned_by(&identity).await.unwrap();
    assert_eq!(owned, vec![("npm".to_owned(), "team-lib".to_owned())]);

    // …and a user outside the group owns nothing.
    let outsider = Identity {
        user_id: Some("bob".to_owned()),
        role: Role::User,
        auth_provider: None,
        groups: vec![],
    };
    assert!(parts
        .ownership
        .list_owned_by(&outsider)
        .await
        .unwrap()
        .is_empty());
}

#[actix_web::test]
async fn my_advisories_reports_a_real_answer_when_nothing_is_affected() {
    let parts = me_app_parts();
    seed_download(&parts.repo, "alice", "npm", "clean-dep", "1.0.0", 10).await;

    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/me/advisories")
            .insert_header(("Authorization", bearer(ALICE_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    assert!(body["advisories"].as_array().unwrap().is_empty());
    assert_eq!(
        body["scanning_available"], true,
        "an empty list with scanning on means 'you are clear', not 'we do not know'"
    );
}

#[actix_web::test]
async fn my_advisories_requires_authentication() {
    let parts = me_app_parts();
    let app = make_me_app(&parts).await;
    let resp = call_service(
        &app,
        TestRequest::get().uri("/api/v1/me/advisories").to_request(),
    )
    .await;
    assert_eq!(resp.status(), 401);
}
