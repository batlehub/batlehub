//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use chrono::Utc;
use serde_json::Value;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    InMemoryVulnerabilityRepository, NoopArtifactMetaRepository as NoopArtifactMeta,
    NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    entities::{AccessEvent, ArtifactVulnerability, PackageId, Role, Severity},
    ports::{
        CacheStore, PackageRepository, RegistryClient, StorageBackend, UserTokenRepository,
        VulnerabilityRepository,
    },
    services::{
        new_hot_lock, AdminService, FeatureFlags, HotConfig, ProxyMetrics, ProxyService,
        RegistryPolicy,
    },
};
use batlehub_web::RegistryModeMap;

/// Variant of `make_app` that attaches a (pre-seeded) vulnerability repository to
/// the `AdminService` and a custom per-registry `feature_flags` map, so the
/// vulnerability findings and socket.dev badge surfacing can be tested over HTTP.
async fn make_vuln_app(
    repo: Arc<InMemoryRepo>,
    vuln_repo: Arc<dyn VulnerabilityRepository>,
    feature_flags: HashMap<String, FeatureFlags>,
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
            "npm".to_owned(),
            FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
        ),
        (
            "cargo".to_owned(),
            FixedRegistry::new("cargo") as Arc<dyn RegistryClient>,
        ),
    ]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> = [
        ("npm".to_owned(), Arc::new(rbac_policy(repo_dyn.clone()).0)),
        (
            "cargo".to_owned(),
            Arc::new(rbac_policy(repo_dyn.clone()).0),
        ),
    ]
    .into();

    // §4.2 — the explore routes resolve `catalogue:browse` from the hierarchy,
    // so a fixture that only sets `access_config_with_explore` grants it to
    // nobody and the detail page answers `404`.
    let grants: HashMap<String, Arc<batlehub_core::entities::RegistryGrants>> = ["npm", "cargo"]
        .into_iter()
        .map(|n| {
            (
                n.to_owned(),
                Arc::new(fixture_grants(
                    n,
                    n,
                    &batlehub_config::schema::RegistryMode::Proxy,
                    &rbac_policy_perms(),
                )),
            )
        })
        .collect();

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
        feature_flags,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage: storage.clone(),
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn).with_vulnerability_repo(vuln_repo));

    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access_config_with_explore(&["npm", "cargo"]);
    let registry_map = registry_map_for(&[("npm", "npm"), ("cargo", "cargo")]);
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
        test_auth_providers(),
    )
    .await
}

/// Build a single vulnerability finding for a coordinate (helper for the tests below).
fn vuln_finding(
    reg: &str,
    name: &str,
    ver: &str,
    osv_id: &str,
    sev: Severity,
) -> ArtifactVulnerability {
    ArtifactVulnerability {
        id: uuid::Uuid::new_v4(),
        artifact_key: format!("artifact:{reg}/{name}/{ver}"),
        registry: reg.to_owned(),
        package_name: name.to_owned(),
        version: ver.to_owned(),
        osv_id: osv_id.to_owned(),
        severity: sev,
        summary: "remote code execution".to_owned(),
        fixed_version: Some("9.9.9".to_owned()),
        purl: format!("pkg:{reg}/{name}@{ver}"),
        detected_at: Utc::now(),
    }
}

#[actix_web::test]
async fn package_detail_surfaces_vulnerability_findings() {
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("npm", "lodash", "4.17.21");
    repo.record_access(AccessEvent::allowed_download(
        pkg,
        Some("u".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    let vuln_repo = Arc::new(InMemoryVulnerabilityRepository::new());
    vuln_repo
        .replace_findings_for_artifact(
            "artifact:npm/lodash/4.17.21",
            vec![vuln_finding(
                "npm",
                "lodash",
                "4.17.21",
                "GHSA-xyz",
                Severity::Critical,
            )],
        )
        .await
        .unwrap();

    let app = make_vuln_app(repo, vuln_repo, HashMap::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages/detail?registry=npm&name=lodash")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let v = &body["versions"][0];
    let vulns = v["vulnerabilities"].as_array().unwrap();
    assert_eq!(vulns.len(), 1);
    assert_eq!(vulns[0]["osv_id"], "GHSA-xyz");
    assert_eq!(vulns[0]["severity"], "critical");
    assert_eq!(vulns[0]["fixed_version"], "9.9.9");
    // Default feature flags → badge present for npm.
    assert_eq!(
        v["socket_badge_url"],
        "https://badge.socket.dev/npm/package/lodash/4.17.21"
    );
}

#[actix_web::test]
async fn package_detail_socket_badge_hidden_when_flag_disabled() {
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("npm", "lodash", "4.17.21");
    repo.record_access(AccessEvent::allowed_download(
        pkg,
        Some("u".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    // Disable the socket badge for npm.
    let flags = HashMap::from([(
        "npm".to_owned(),
        FeatureFlags {
            socket_badge: false,
        },
    )]);
    let app = make_vuln_app(repo, InMemoryVulnerabilityRepository::arc(), flags).await;
    let req = TestRequest::get()
        .uri("/api/v1/admin/packages/detail?registry=npm&name=lodash")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(
        body["versions"][0]["socket_badge_url"].is_null(),
        "badge must be hidden when feature flag is disabled"
    );
}

#[actix_web::test]
async fn explore_detail_surfaces_vulnerabilities_and_badge() {
    let repo = InMemoryRepo::new();
    let pkg = PackageId::new("cargo", "yaml", "0.3.0");
    repo.record_access(AccessEvent::allowed_download(
        pkg,
        Some("u".to_owned()),
        Role::User,
    ))
    .await
    .unwrap();

    let vuln_repo = Arc::new(InMemoryVulnerabilityRepository::new());
    vuln_repo
        .replace_findings_for_artifact(
            "artifact:cargo/yaml/0.3.0",
            vec![vuln_finding(
                "cargo",
                "yaml",
                "0.3.0",
                "RUSTSEC-2021-1",
                Severity::High,
            )],
        )
        .await
        .unwrap();

    let app = make_vuln_app(repo, vuln_repo, HashMap::new()).await;
    let req = TestRequest::get()
        .uri("/api/v1/explore/packages/cargo/yaml")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let ver = body["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["version"] == "0.3.0")
        .expect("version present");
    assert_eq!(ver["vulnerabilities"][0]["osv_id"], "RUSTSEC-2021-1");
    assert_eq!(ver["vulnerabilities"][0]["severity"], "high");
    assert_eq!(
        ver["socket_badge_url"],
        "https://badge.socket.dev/cargo/package/yaml/0.3.0"
    );
}
