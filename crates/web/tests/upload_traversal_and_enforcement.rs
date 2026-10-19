//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, TestRequest};

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_adapters::local_registry::InMemoryLocalRegistry;
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::TeamNamespace;
use batlehub_core::ports::TeamNamespacePort;
use batlehub_core::{
    ports::{CacheStore, PackageRepository, RegistryClient, StorageBackend, UserTokenRepository},
    services::{
        new_hot_lock, AdminService, HotConfig, LocalRegistryService, ProxyMetrics, ProxyService,
        RegistryPolicy,
    },
};
use batlehub_web::RegistryModeMap;

// ── rubygems publish traversal ─────────────────────────────────────────────────

#[actix_web::test]
async fn rubygems_publish_traversal_version_returns_400() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_upload_app("local-gems", "rubygems", ns_store).await;

    let req = TestRequest::post()
        .uri("/proxy/local-gems/api/v1/gems")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(make_gem("my-gem", "../../etc/x"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

// ── composer publish traversal ─────────────────────────────────────────────────
// (uses the `make_composer_zip` helper defined further down, near the other
// composer tests)

#[actix_web::test]
async fn composer_publish_traversal_version_returns_400() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_upload_app("local-composer", "composer", ns_store).await;

    // No `?version=` override, so the traversal string comes from
    // composer.json's own `version` field (the query-param override has its
    // own, separate character-allowlist validation).
    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .set_payload(make_composer_zip("acme/widget", "../../etc/x"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

// ── openvsx publish traversal ───────────────────────────────────────────────────

/// Minimal VSIX: a ZIP with a `[Content_Types].xml` entry so the handler
/// recognises it as a valid archive. Name and version come from the URL path.
fn make_minimal_vsix() -> Vec<u8> {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("[Content_Types].xml", opts).unwrap();
        zw.write_all(b"<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"></Types>").unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

#[actix_web::test]
async fn openvsx_publish_traversal_version_returns_400() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_upload_app("local-vsx", "openvsx", ns_store).await;

    // `version` is its own routed path segment for vsix_publish, so a literal
    // ".." there must be rejected by the deep `local_svc.publish` funnel.
    let req = TestRequest::put()
        .uri("/proxy/local-vsx/acme.widget/../vsix")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .set_payload(make_minimal_vsix())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

// ── Generic ns-upload app factory ────────────────────────────────────────────

/// Build a Local-mode test app for `registry_name` (type `registry_type`) with
/// a `TeamNamespacePort` wired into the `LocalRegistryService`.
///
/// Used by the upload-enforcement tests for RubyGems, GoProxy, OpenVSX, and
/// Composer — only the registry name/type differ from `make_ns_cargo_app`.
async fn make_ns_upload_app(
    registry_name: &'static str,
    registry_type: &'static str,
    ns_store: Arc<dyn TeamNamespacePort>,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> = [(
        registry_name.to_owned(),
        FixedRegistry::new(registry_type) as Arc<dyn RegistryClient>,
    )]
    .into();
    let policies: HashMap<String, Arc<RegistryPolicy>> = [(
        registry_name.to_owned(),
        Arc::new(rbac_policy(repo_dyn.clone()).0),
    )]
    .into();

    let backend = Arc::new(InMemoryLocalRegistry::new());
    let local_svc = Arc::new(LocalRegistryService {
        backend: backend.clone(),
        storage: storage.clone(),
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
            ..Default::default()
        }),
        quota: None,
        ownership: None,
        team_namespace: Some(Arc::clone(&ns_store)),
        sbom: None,
        explore_cache: None,
        package_repo: None,
        readme: None,
    });

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
            policies,
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
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullTokenRepository);
    let access_config = access_config(&[], &[registry_name]);
    let registry_map = registry_map_for(&[(registry_name, registry_type)]);
    let cargo_indexes = batlehub_web::CargoIndexMap::default();
    let mode_map = RegistryModeMap::default();
    mode_map.insert(registry_name.to_owned(), RegistryMode::Local);

    finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        cargo_indexes,
        ConfigureAppDefaults::default(),
        ns_store,
        team_ns_auth_providers(),
    )
    .await
}

// ── Payload builders for upload-capable registry types ────────────────────────

/// Minimal `.gem` file: a TAR archive containing one `metadata.gz` entry with
/// name/version encoded as YAML inside a gzip stream.
fn ns_minimal_gem(name: &str, version: &str) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write as _;

    let yaml = format!("name: {name}\nversion:\n  version: '{version}'\nplatform: ruby\n");
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(yaml.as_bytes()).unwrap();
    let metadata_gz = gz.finish().unwrap();

    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata_gz.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "metadata.gz", metadata_gz.as_slice())
        .unwrap();
    builder.into_inner().unwrap()
}

/// Minimal Go module ZIP: one entry `{module}@{version}/go.mod`.
fn ns_go_module_zip(module: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
    let go_mod = format!("module {module}\n\ngo 1.21\n");
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file(format!("{module}@{version}/go.mod"), opts)
            .unwrap();
        zw.write_all(go_mod.as_bytes()).unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

/// Minimal Composer ZIP: `composer.json` at the archive root.
fn ns_composer_zip(vendor_pkg: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("composer.json", opts).unwrap();
        let json = serde_json::json!({
            "name": vendor_pkg,
            "version": version,
            "description": "test",
            "type": "library",
        });
        zw.write_all(json.to_string().as_bytes()).unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

/// Minimal VSIX: a ZIP with a `[Content_Types].xml` entry so the handler
/// recognises it as a valid archive.  Name and version come from the URL path.
fn ns_minimal_vsix() -> Vec<u8> {
    use std::io::Write as _;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("[Content_Types].xml", opts).unwrap();
        zw.write_all(b"<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"></Types>").unwrap();
        zw.finish().unwrap();
    }
    buf.into_inner()
}

// ── RubyGems upload enforcement ───────────────────────────────────────────────

#[actix_web::test]
async fn rubygems_upload_to_claimed_namespace_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-gems".to_owned(),
            prefix: "internal-gem".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-gems", "rubygems", ns_store).await;

    let req = TestRequest::post()
        .uri("/proxy/local-gems/api/v1/gems")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(ns_minimal_gem("internal-gem", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn rubygems_upload_to_claimed_namespace_allows_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-gems".to_owned(),
            prefix: "internal-gem".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-gems", "rubygems", ns_store).await;

    let req = TestRequest::post()
        .uri("/proxy/local-gems/api/v1/gems")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(ns_minimal_gem("internal-gem", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn rubygems_upload_to_unclaimed_namespace_allows_any_user() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_upload_app("local-gems", "rubygems", ns_store).await;

    let req = TestRequest::post()
        .uri("/proxy/local-gems/api/v1/gems")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(ns_minimal_gem("open-gem", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── GoProxy upload enforcement ────────────────────────────────────────────────

#[actix_web::test]
async fn goproxy_upload_to_claimed_namespace_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-go".to_owned(),
            prefix: "example.com/internal".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-go", "goproxy", ns_store).await;

    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/internal/utils/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(ns_go_module_zip("example.com/internal/utils", "v1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn goproxy_upload_to_claimed_namespace_allows_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-go".to_owned(),
            prefix: "example.com/internal".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-go", "goproxy", ns_store).await;

    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/internal/utils/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(ns_go_module_zip("example.com/internal/utils", "v1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn goproxy_upload_to_unclaimed_namespace_allows_any_user() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_upload_app("local-go", "goproxy", ns_store).await;

    let req = TestRequest::put()
        .uri("/proxy/local-go/example.com/open/pkg/@v/v1.0.0.zip")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(ns_go_module_zip("example.com/open/pkg", "v1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── OpenVSX upload enforcement ────────────────────────────────────────────────

#[actix_web::test]
async fn openvsx_upload_to_claimed_namespace_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    // Extension ID is "myorg.myext"; prefix "myorg.myext" covers it exactly.
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-vsx".to_owned(),
            prefix: "myorg.myext".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-vsx", "openvsx", ns_store).await;

    let req = TestRequest::put()
        .uri("/proxy/local-vsx/myorg.myext/1.0.0/vsix")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(ns_minimal_vsix())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn openvsx_upload_to_claimed_namespace_allows_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-vsx".to_owned(),
            prefix: "myorg.myext".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-vsx", "openvsx", ns_store).await;

    let req = TestRequest::put()
        .uri("/proxy/local-vsx/myorg.myext/1.0.0/vsix")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(ns_minimal_vsix())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn openvsx_upload_to_unclaimed_namespace_allows_any_user() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_upload_app("local-vsx", "openvsx", ns_store).await;

    let req = TestRequest::put()
        .uri("/proxy/local-vsx/openorg.openext/1.0.0/vsix")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(ns_minimal_vsix())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

// ── Composer upload enforcement ───────────────────────────────────────────────

#[actix_web::test]
async fn composer_upload_to_claimed_namespace_blocks_non_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-composer".to_owned(),
            prefix: "myvendor".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-composer", "composer", ns_store).await;

    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(ns_composer_zip("myvendor/mypkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

#[actix_web::test]
async fn composer_upload_to_claimed_namespace_allows_member() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    ns_store
        .claim_namespace(TeamNamespace {
            registry: "local-composer".to_owned(),
            prefix: "myvendor".to_owned(),
            group_id: "team-alpha".to_owned(),
            claimed_by: None,
            separator: '/',
        })
        .await
        .unwrap();
    let app = make_ns_upload_app("local-composer", "composer", ns_store).await;

    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(NS_MEMBER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(ns_composer_zip("myvendor/mypkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

#[actix_web::test]
async fn composer_upload_to_unclaimed_namespace_allows_any_user() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let app = make_ns_upload_app("local-composer", "composer", ns_store).await;

    let req = TestRequest::post()
        .uri("/proxy/local-composer/api/upload")
        .insert_header(("Authorization", bearer(NS_PLAIN_USER_TOKEN)))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(ns_composer_zip("openvendor/openpkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}
