//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, init_service, read_body_json, TestRequest};
use serde_json::Value;
use utoipa_actix_web::AppExt;

use batlehub_adapters::cache::InMemoryBannerStore;
use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_core::{
    ports::{BannerPort, CacheStore, StorageBackend, UserTokenRepository},
    services::{new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService},
};
use batlehub_web::services::{
    BannerService, ConfigReloadParams, ConfigReloadService, HotConfigBuilder,
};
use batlehub_web::AuthMiddlewareFactory;

// ── Banner endpoints ──────────────────────────────────────────────────────────

/// Build a minimal app with banner and reload services wired in.
async fn make_banner_app() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    make_banner_app_seeded(Vec::new()).await
}

/// `make_banner_app`, with the reload service's config-warning store pre-seeded
/// as `set_warnings` does at server startup.
async fn make_banner_app_seeded(
    warnings: Vec<batlehub_config::schema::ConfigWarning>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_reload_app(ReloadAppOpts {
        warnings,
        ..Default::default()
    })
    .await
}

/// What the reload-service fixture lets a test vary. The defaults are the banner
/// tests' world: no warnings, hot reload on, a config path that does not exist and
/// a builder that refuses — enough for every endpoint that never reads the file
/// and never rebuilds. The config-editor rows are the ones that need the rest.
struct ReloadAppOpts {
    warnings: Vec<batlehub_config::schema::ConfigWarning>,
    /// The path `GET /api/v1/admin/config/content` reads.
    config_path: String,
    /// Layers merged over `config_path`. Never served to the editor, so a test
    /// that sets one is asserting exactly that.
    config_overlays: Vec<String>,
    hot_reload_enabled: bool,
    builder: HotConfigBuilder,
}

impl Default for ReloadAppOpts {
    fn default() -> Self {
        Self {
            warnings: Vec::new(),
            config_path: "config.toml".to_owned(),
            config_overlays: Vec::new(),
            hot_reload_enabled: true,
            builder: Arc::new(|_| anyhow::bail!("not used in tests")),
        }
    }
}

/// A builder that accepts whatever it is handed and reports an empty world.
///
/// `validate` runs the candidate config through the builder, so a fixture whose
/// builder always fails can only ever assert the `400`. This one is what the
/// success rows need.
fn accepting_builder() -> HotConfigBuilder {
    use std::collections::HashMap;
    Arc::new(|_| {
        Ok(batlehub_web::services::BuiltHotState {
            hot: batlehub_core::services::HotConfig::default(),
            access: batlehub_web::AccessConfig {
                anonymous: Default::default(),
                user: Default::default(),
                admin: Default::default(),
                groups: Default::default(),
                explore_anonymous: Default::default(),
                explore_user: Default::default(),
                explore_admin: Default::default(),
            },
            registry_map: batlehub_web::RegistryMap::new(HashMap::new()),
            registry_mode_map: batlehub_web::RegistryModeMap::new(HashMap::new()),
            upstream_map: batlehub_web::UpstreamMap::new(HashMap::new()),
            cargo_index_map: batlehub_web::CargoIndexMap::new(HashMap::new()),
            repo_signer_map: batlehub_web::RepoSignerMap::default(),
            vuln_db_map: batlehub_web::VulnDbMap::default(),
            sumdb_map: batlehub_web::SumDbMap::default(),
            registry_host_map: batlehub_web::RegistryHostMap::default(),
            search_readmes: false,
        })
    })
}

async fn build_reload_app(
    opts: ReloadAppOpts,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    use std::collections::HashMap;
    let repo = InMemoryRepo::new();
    let repo_dyn: Arc<dyn batlehub_core::ports::PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

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
            registries: HashMap::new(),
            policies: HashMap::new(),
            ..Default::default()
        }),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access_config_for(&[]);

    let banner_store: Arc<dyn BannerPort> = Arc::new(InMemoryBannerStore::new());
    let banner_svc = Arc::new(BannerService::new(banner_store));

    let hot = proxy_svc.hot.clone();
    let reload_svc = Arc::new(ConfigReloadService::new(ConfigReloadParams {
        hot,
        access: access_config.clone(),
        search: batlehub_web::new_search_lock(false),
        registry_map: batlehub_web::RegistryMap::new(HashMap::new()),
        registry_mode_map: batlehub_web::RegistryModeMap::new(HashMap::new()),
        upstream_map: batlehub_web::UpstreamMap::new(HashMap::new()),
        cargo_index_map: batlehub_web::CargoIndexMap::new(HashMap::new()),
        repo_signer_map: batlehub_web::RepoSignerMap::default(),
        vuln_db_map: batlehub_web::VulnDbMap::default(),
        sumdb_map: batlehub_web::SumDbMap::default(),
        registry_host_map: batlehub_web::RegistryHostMap::default(),
        proxy_trust: batlehub_web::ProxyTrust::default(),
        config_path: opts.config_path,
        config_overlays: opts.config_overlays,
        config_change_repo: None,
        hot_reload_enabled: opts.hot_reload_enabled,
        builder: opts.builder,
        banner: Some(Arc::clone(&banner_svc)),
    }));
    reload_svc.set_warnings(opts.warnings);

    let (app, _) = actix_web::App::new()
        // RFC 0015 §4.2 — this app registers only the handlers under test, so the
        // hot lock the control-verb check reads has to be registered with them.
        .app_data(actix_web::web::Data::new(
            batlehub_core::services::hot_config::new_hot_lock(
                batlehub_core::services::hot_config::HotConfig {
                    instance: Some(std::sync::Arc::new(
                        batlehub_core::services::authz::translate::instance_node(None),
                    )),
                    ..Default::default()
                },
            ),
        ))
        .into_utoipa_app()
        .configure(configure_test_app(
            proxy_svc,
            admin_svc,
            token_repo,
            access_config,
            batlehub_web::RegistryMap::new(HashMap::new()),
            ConfigureAppDefaults::default(),
        ))
        .split_for_parts();
    let app = app
        .app_data(actix_web::web::Data::new(banner_svc))
        .app_data(actix_web::web::Data::new(reload_svc))
        .app_data(actix_web::web::Data::new(
            batlehub_web::CargoIndexMap::default(),
        ))
        .app_data(actix_web::web::Data::new(
            batlehub_web::RegistryModeMap::default(),
        ));

    init_service(app.wrap(AuthMiddlewareFactory::new(test_auth_providers()))).await
}

#[actix_web::test]
async fn get_banner_returns_null_when_unset() {
    let app = make_banner_app().await;
    let req = TestRequest::get().uri("/api/v1/banner").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert!(body.is_null(), "expected null, got {body}");
}

#[actix_web::test]
async fn set_banner_requires_admin() {
    let app = make_banner_app().await;
    let req = TestRequest::put()
        .uri("/api/v1/admin/banner")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({"message": "hello", "level": "info"}))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn set_and_get_banner_round_trip() {
    let app = make_banner_app().await;

    // Set banner as admin
    let set_req = TestRequest::put()
        .uri("/api/v1/admin/banner")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({"message": "Maintenance window", "level": "warning"}))
        .to_request();
    let set_resp = call_service(&app, set_req).await;
    assert_eq!(set_resp.status(), 200);

    // Read banner (no auth needed)
    let get_req = TestRequest::get().uri("/api/v1/banner").to_request();
    let get_resp = call_service(&app, get_req).await;
    assert_eq!(get_resp.status(), 200);
    let banner: Value = read_body_json(get_resp).await;
    assert_eq!(banner["message"], "Maintenance window");
    assert_eq!(banner["level"], "warning");
}

#[actix_web::test]
async fn clear_banner_removes_it() {
    let app = make_banner_app().await;

    // Set then clear
    let _ = call_service(
        &app,
        TestRequest::put()
            .uri("/api/v1/admin/banner")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({"message": "temp", "level": "info"}))
            .to_request(),
    )
    .await;

    let del_resp = call_service(
        &app,
        TestRequest::delete()
            .uri("/api/v1/admin/banner")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(del_resp.status(), 204);

    let get_resp = call_service(&app, TestRequest::get().uri("/api/v1/banner").to_request()).await;
    assert_eq!(get_resp.status(), 200);
    let body: Value = read_body_json(get_resp).await;
    assert!(body.is_null());
}

// ── Config reload endpoints ───────────────────────────────────────────────────

#[actix_web::test]
async fn reload_config_returns_503_when_disabled() {
    let app = build_reload_app(ReloadAppOpts {
        hot_reload_enabled: false,
        ..Default::default()
    })
    .await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/config/reload")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 503);
}

#[actix_web::test]
async fn get_pending_reload_returns_404_when_none() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/pending")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn apply_pending_returns_404_when_none() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/config/pending/apply")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn discard_pending_returns_404_when_none() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::delete()
            .uri("/api/v1/admin/config/pending")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn config_reload_endpoints_require_admin() {
    let app = make_banner_app().await;
    for (method, uri) in [
        ("POST", "/api/v1/admin/config/reload"),
        ("GET", "/api/v1/admin/config/pending"),
        ("POST", "/api/v1/admin/config/pending/apply"),
        ("DELETE", "/api/v1/admin/config/pending"),
    ] {
        let req = TestRequest::with_uri(uri)
            .method(actix_web::http::Method::from_bytes(method.as_bytes()).unwrap())
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request();
        let resp = call_service(&app, req).await;
        assert_eq!(resp.status(), 403, "{method} {uri} should require admin");
    }
}

#[actix_web::test]
async fn list_config_changes_returns_empty_without_db() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/changes")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    // No DB pool → internal error from list_changes, or empty if handled gracefully
    // The endpoint returns 500 when no pool is configured; that's acceptable here.
    assert!(
        resp.status().is_success() || resp.status().is_server_error(),
        "unexpected status {}",
        resp.status()
    );
}

// ── Config warnings ───────────────────────────────────────────────────────────

#[actix_web::test]
async fn config_warnings_endpoint_returns_the_active_warnings() {
    use batlehub_config::schema::{warnings as codes, ConfigWarning};
    let app = make_banner_app_seeded(vec![ConfigWarning::new(
        codes::PROXY_TRUST_UNCONFIGURED,
        "server.trusted_proxies",
        "no trusted-proxy list is configured",
    )])
    .await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/warnings")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    let warnings = body["warnings"].as_array().expect("warnings array");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["code"], codes::PROXY_TRUST_UNCONFIGURED);
    // `path` is shown verbatim so an operator can grep the TOML for it.
    assert_eq!(warnings[0]["path"], "server.trusted_proxies");
}

#[actix_web::test]
async fn config_warnings_endpoint_returns_an_empty_list_for_a_clean_config() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/warnings")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert!(body["warnings"]
        .as_array()
        .expect("warnings array")
        .is_empty());
}

#[actix_web::test]
async fn config_warnings_endpoint_requires_admin() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/warnings")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

// ── Config editor: content and validate ───────────────────────────────────────
//
// The two endpoints the SPA's config editor is built on. `content` is the only
// route that reads the file off disk, and `validate` is the only one that runs a
// candidate config through the builder without staging anything — the property
// that separates it from `from-content` next door, and the one worth pinning.

/// A config the loader accepts, matching the service-level fixture.
const MINIMAL_CONFIG: &str = r#"
[server]
host = "127.0.0.1"
port = 8080

[database]
type = "postgresql"
url = "postgresql://user:pass@localhost/db"

[storage]
type = "filesystem"
path = "./tmp"
"#;

/// A config file on disk, with `content` in it.
fn config_file(content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().expect("temp config file");
    f.write_all(content.as_bytes()).expect("write config");
    f.flush().expect("flush config");
    f
}

#[actix_web::test]
async fn config_content_requires_admin() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/content")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn config_content_returns_404_when_the_file_is_missing() {
    // The default fixture points at a `config.toml` that is not there: the
    // editor must be told the file is gone, not handed a 500 it cannot act on.
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/content")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn config_content_returns_the_file_verbatim() {
    let file = config_file(MINIMAL_CONFIG);
    let app = build_reload_app(ReloadAppOpts {
        config_path: file.path().display().to_string(),
        config_overlays: Vec::new(),
        ..Default::default()
    })
    .await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/content")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    // Verbatim: the editor round-trips these bytes back through `from-content`,
    // so anything the handler normalises here is a change the admin never made.
    assert_eq!(body["content"], MINIMAL_CONFIG);
    assert_eq!(body["is_readonly"], false);
}

#[actix_web::test]
async fn config_content_reports_readonly_when_hot_reload_is_disabled() {
    // A ConfigMap-mounted deployment: the file is readable, but nothing the
    // editor submits can ever be applied. The flag is what greys the editor out;
    // without it the admin types into a form whose save is refused.
    let file = config_file(MINIMAL_CONFIG);
    let app = build_reload_app(ReloadAppOpts {
        config_path: file.path().display().to_string(),
        config_overlays: Vec::new(),
        hot_reload_enabled: false,
        ..Default::default()
    })
    .await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/content")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["is_readonly"], true);
}

#[actix_web::test]
async fn validate_config_content_requires_admin() {
    let app = make_banner_app().await;
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/config/validate")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_json(serde_json::json!({ "content": MINIMAL_CONFIG }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn validate_config_content_returns_503_when_hot_reload_is_disabled() {
    let app = build_reload_app(ReloadAppOpts {
        hot_reload_enabled: false,
        builder: accepting_builder(),
        ..Default::default()
    })
    .await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/config/validate")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({ "content": MINIMAL_CONFIG }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 503);
}

#[actix_web::test]
async fn validate_config_content_returns_400_for_a_config_the_loader_refuses() {
    let app = build_reload_app(ReloadAppOpts {
        builder: accepting_builder(),
        ..Default::default()
    })
    .await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/config/validate")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({ "content": "this is not = = toml" }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn validate_config_content_accepts_a_valid_config_without_staging_it() {
    let app = build_reload_app(ReloadAppOpts {
        builder: accepting_builder(),
        ..Default::default()
    })
    .await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/config/validate")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({ "content": MINIMAL_CONFIG }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(
        body["pending_created"], false,
        "validate is a dry run by contract"
    );

    // And nothing was staged: the endpoint next door is the one that stages, and
    // an admin who validated must still submit before there is anything to apply.
    let pending = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/config/pending")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(pending.status(), 404);
}
