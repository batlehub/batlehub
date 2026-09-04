//! JetBrains Marketplace registry integration tests: publish, IDE-facing
//! endpoints, mode guards, and offline resilience (see `tests/common/mod.rs`
//! for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures::stream;
use serde_json::Value;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryStorageBackend as InMemoryStorage,
    NoopArtifactMetaRepository as NoopArtifactMeta, NullUserTokenRepository as NullTokenRepository,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{
        CacheStore, FetchedArtifact, NoopWarmCoordinator, PackageRepository, RegistryClient,
        StorageBackend, StorageMeta, UserTokenRepository,
    },
    services::{
        new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy,
        WarmingService,
    },
};
use batlehub_web::RegistryModeMap;

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// The `Content-Disposition` IntelliJ's `PluginDownloader` needs on download
/// URLs that carry no filename in their path (`plugin/download?…`,
/// `pluginManager?…`) — without it the IDE aborts with "Invalid filename
/// returned by a server".
fn content_disposition<B>(resp: &actix_web::dev::ServiceResponse<B>) -> Option<String> {
    resp.headers()
        .get(actix_web::http::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn plugin_xml(id: &str, version: &str, since: &str, until: &str) -> String {
    format!(
        r#"<idea-plugin>
  <id>{id}</id>
  <name>Demo Plugin</name>
  <version>{version}</version>
  <vendor>Test Vendor</vendor>
  <description>A demo plugin</description>
  <change-notes>notes</change-notes>
  <depends>com.intellij.modules.platform</depends>
  <idea-version since-build="{since}" until-build="{until}"/>
</idea-plugin>"#
    )
}

fn make_plugin_jar(descriptor: &str) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    w.start_file(
        "META-INF/plugin.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    w.write_all(descriptor.as_bytes()).unwrap();
    w.finish().unwrap();
    buf.into_inner()
}

fn make_plugin_zip_dist(descriptor: &str) -> Vec<u8> {
    use std::io::Write;
    let jar = make_plugin_jar(descriptor);
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    w.start_file(
        "demo/lib/demo.jar",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    w.write_all(&jar).unwrap();
    w.finish().unwrap();
    buf.into_inner()
}

/// Build the marketplace upload `multipart/form-data` body.
fn make_upload_body(
    archive: &[u8],
    xml_id: Option<&str>,
    channel: Option<&str>,
    is_hidden: bool,
) -> (Vec<u8>, String) {
    let boundary = "jbmboundary";
    let mut body = Vec::new();
    let mut text_field = |name: &str, value: &str| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    if let Some(id) = xml_id {
        text_field("xmlId", id);
    }
    if let Some(ch) = channel {
        text_field("channel", ch);
    }
    if is_hidden {
        text_field("isHidden", "true");
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"plugin.zip\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(archive);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

async fn make_local_jbm_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-jbm", "jetbrains-marketplace", mode, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

async fn publish_plugin(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    archive: &[u8],
    xml_id: Option<&str>,
    is_hidden: bool,
) -> actix_web::dev::ServiceResponse<actix_web::body::BoxBody> {
    let (body, ct) = make_upload_body(archive, xml_id, None, is_hidden);
    let req = TestRequest::post()
        .uri("/proxy/local-jbm/api/updates/upload")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header(("Content-Type", ct))
        .set_payload(body)
        .to_request();
    call_service(app, req).await
}

// ── Publish ───────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn jbm_publish_jar_returns_201() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    let resp = publish_plugin(&app, &jar, Some("org.demo.a"), false).await;
    assert_eq!(resp.status(), 201);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["pluginId"], "org.demo.a");
    assert_eq!(body["version"], "1.0.0");
}

#[actix_web::test]
async fn jbm_publish_nested_zip_returns_201() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let dist = make_plugin_zip_dist(&plugin_xml("org.demo.zip", "2.0.0", "241.0", "242.*"));
    let resp = publish_plugin(&app, &dist, None, false).await;
    assert_eq!(resp.status(), 201);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["pluginId"], "org.demo.zip");
}

#[actix_web::test]
async fn jbm_publish_xml_id_mismatch_returns_400() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.real", "1.0.0", "233.0", "241.*"));
    let resp = publish_plugin(&app, &jar, Some("org.demo.other"), false).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn jbm_publish_without_descriptor_returns_422() {
    use std::io::Write;
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    w.start_file("readme.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    w.write_all(b"no descriptor here").unwrap();
    w.finish().unwrap();
    let resp = publish_plugin(&app, &buf.into_inner(), None, false).await;
    assert_eq!(resp.status(), 422);
}

#[actix_web::test]
async fn jetbrains_marketplace_publish_traversal_version_returns_400() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml(
        "org.demo.evil",
        "../../etc/x",
        "233.0",
        "241.*",
    ));
    let resp = publish_plugin(&app, &jar, None, false).await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn jbm_publish_on_proxy_mode_returns_404() {
    let app = make_local_jbm_app(RegistryMode::Proxy).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    let resp = publish_plugin(&app, &jar, None, false).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn jbm_routes_on_wrong_registry_type_return_404() {
    // A cargo-typed registry must not answer marketplace routes.
    let app = make_local_registry_app(RegistryMode::Local).await;
    let req = TestRequest::get()
        .uri("/proxy/local-cargo/updatePlugins.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}

// ── updatePlugins.xml / plugins/list ─────────────────────────────────────────

#[actix_web::test]
async fn jbm_update_plugins_xml_lists_published_plugin_with_self_url() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-jbm/updatePlugins.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains(r#"<plugin id="org.demo.a""#), "{body}");
    assert!(
        body.contains("/proxy/local-jbm/plugin/download?pluginId=org.demo.a&amp;version=1.0.0"),
        "self-referencing url missing: {body}"
    );
    assert!(body.contains(r#"since-build="233.0""#));
}

#[actix_web::test]
async fn jbm_update_plugins_xml_filters_by_build() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    // In range.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/updatePlugins.xml?build=IU-241.100")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = String::from_utf8(read_body(call_service(&app, req).await).await.to_vec()).unwrap();
    assert!(body.contains("org.demo.a"));

    // Out of range.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/updatePlugins.xml?build=IU-243.1")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = String::from_utf8(read_body(call_service(&app, req).await).await.to_vec()).unwrap();
    assert!(!body.contains("org.demo.a"), "{body}");
}

#[actix_web::test]
async fn jbm_update_plugins_xml_proxy_mode_returns_404() {
    let app = make_local_jbm_app(RegistryMode::Proxy).await;
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/updatePlugins.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn jbm_plugins_list_returns_repository_xml() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-jbm/plugins/list?pluginId=org.demo.a")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("<plugin-repository>"));
    assert!(body.contains("<id>org.demo.a</id>"));
    assert!(body.contains("<version>1.0.0</version>"));

    // Unknown plugin → empty repository, 200 (upstream semantics).
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/plugins/list?pluginId=org.missing")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("<plugin-repository/>"), "{body}");
}

// ── Artifact round-trips ─────────────────────────────────────────────────────

#[actix_web::test]
async fn jbm_plugin_download_roundtrip() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-jbm/plugin/download?pluginId=org.demo.a&version=1.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        content_disposition(&resp).as_deref(),
        Some(r#"attachment; filename="org.demo.a-1.0.0.zip""#)
    );
    assert_eq!(read_body(resp).await.to_vec(), jar);
}

#[actix_web::test]
async fn jbm_files_download_roundtrip() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-jbm/files/org.demo.a/1.0.0/plugin.zip")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await.to_vec(), jar);
}

#[actix_web::test]
async fn jbm_hybrid_download_falls_through_to_upstream() {
    let app = make_local_jbm_app(RegistryMode::Hybrid).await;
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/plugin/download?pluginId=org.not.local&version=9.9.9")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        content_disposition(&resp).as_deref(),
        Some(r#"attachment; filename="org.not.local-9.9.9.zip""#)
    );
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(
        body.starts_with("artifact:jetbrains-marketplace"),
        "hybrid miss must stream the FixedRegistry body, got: {body}"
    );
}

// ── Search / listings / compatible updates ───────────────────────────────────

#[actix_web::test]
async fn jbm_search_and_xml_ids_listing() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-jbm/api/searchPlugins?search=demo")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["plugins"][0]["xmlId"], "org.demo.a");

    // No-match search.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/api/searchPlugins?search=nothing-here")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 0);

    // pluginsXMLIds listing.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/files/pluginsXMLIds.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body, serde_json::json!(["org.demo.a"]));
}

#[actix_web::test]
async fn jbm_compatible_updates_respects_build_range() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    let post = |build: &str| {
        TestRequest::post()
            .uri("/proxy/local-jbm/api/search/updates/compatible")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_json(serde_json::json!({
                "build": build,
                "pluginXMLIds": ["org.demo.a", "org.unknown"],
            }))
            .to_request()
    };

    // In range: one update.
    let body: Value = read_body_json(call_service(&app, post("IU-240.5")).await).await;
    let updates = body.as_array().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["pluginXmlId"], "org.demo.a");
    assert_eq!(updates[0]["version"], "1.0.0");

    // Out of range: empty.
    let body: Value = read_body_json(call_service(&app, post("IU-243.1")).await).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[actix_web::test]
async fn jbm_plugin_manager_resolves_latest_compatible() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let v1 = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "240.*"));
    let v2 = make_plugin_jar(&plugin_xml("org.demo.a", "2.0.0", "241.0", "242.*"));
    assert_eq!(publish_plugin(&app, &v1, None, false).await.status(), 201);
    assert_eq!(publish_plugin(&app, &v2, None, false).await.status(), 201);

    // Build in v1's range only → must serve v1's bytes.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/pluginManager?action=download&id=org.demo.a&build=IU-234.100")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        content_disposition(&resp).as_deref(),
        Some(r#"attachment; filename="org.demo.a-1.0.0.zip""#)
    );
    assert_eq!(read_body(resp).await.to_vec(), v1);

    // Incompatible build → 404.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/pluginManager?action=download&id=org.demo.a&build=IU-999.1")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);

    // Unsupported action → 400.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/pluginManager?action=install&id=org.demo.a")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

// ── Plugin JSON + meta.json shapes ───────────────────────────────────────────

#[actix_web::test]
async fn jbm_plugin_info_and_updates_json() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    let req = TestRequest::get()
        .uri("/proxy/local-jbm/api/plugins/org.demo.a")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["xmlId"], "org.demo.a");
    assert_eq!(body["name"], "Demo Plugin");
    assert_eq!(body["vendor"]["name"], "Test Vendor");

    let req = TestRequest::get()
        .uri("/proxy/local-jbm/api/plugins/org.demo.a/updates")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    let updates = body.as_array().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["version"], "1.0.0");
    assert_eq!(updates[0]["since"], "233.0");
    assert_eq!(updates[0]["until"], "241.*");
}

#[actix_web::test]
async fn jbm_both_meta_json_shapes() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.a", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, false).await.status(), 201);

    // Plugin-level meta.json.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/files/org.demo.a/meta.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["xmlId"], "org.demo.a");
    assert_eq!(body["versions"].as_array().unwrap().len(), 1);

    // Update-level meta.json.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/files/org.demo.a/1.0.0/meta.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["version"], "1.0.0");
    assert_eq!(body["since"], "233.0");

    // Unknown version → 404.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/files/org.demo.a/9.9.9/meta.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── isHidden semantics ───────────────────────────────────────────────────────

#[actix_web::test]
async fn jbm_hidden_version_excluded_from_listings_but_downloadable() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.hidden", "1.0.0", "233.0", "241.*"));
    assert_eq!(publish_plugin(&app, &jar, None, true).await.status(), 201);

    // Excluded from updatePlugins.xml…
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/updatePlugins.xml")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body = String::from_utf8(read_body(call_service(&app, req).await).await.to_vec()).unwrap();
    assert!(!body.contains("org.demo.hidden"), "{body}");

    // …and from search…
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/api/searchPlugins?search=hidden")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert_eq!(body["total"], 0);

    // …but still downloadable by exact coordinate.
    let req = TestRequest::get()
        .uri("/proxy/local-jbm/plugin/download?pluginId=org.demo.hidden&version=1.0.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(read_body(resp).await.to_vec(), jar);
}

// ── Offline resilience ("JetBrains disappears") ──────────────────────────────

/// A registry client that can be flipped to unavailable mid-test — the
/// `UnavailableRegistry` pattern with a switch (`FixedRegistry` cannot fail).
struct FlakyJbmRegistry {
    down: AtomicBool,
}

impl FlakyJbmRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            down: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl RegistryClient for FlakyJbmRegistry {
    fn registry_type(&self) -> &str {
        "jetbrains-marketplace"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(CoreError::Registry("upstream down".into()));
        }
        Ok(PackageMetadata {
            id: PackageId {
                version: "1.2.0".to_owned(),
                ..pkg.clone()
            },
            published_at: Some(Utc::now() - chrono::Duration::days(30)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({
                "name": "Flaky Plugin",
                "vendor": "Upstream",
                "versions": [
                    {"version": "1.2.0", "since_build": "233.0", "until_build": "999.*", "date_ms": 200},
                ],
            }),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(CoreError::Registry("upstream down".into()));
        }
        let body = Bytes::from(format!("plugin-bytes:{}", pkg.cache_key()));
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(body) })),
            cache_control: None,
        })
    }
}

/// Proxy-mode app around a flippable upstream, with `serve_stale_metadata`
/// enabled and a very short metadata TTL so the stale path is exercised.
///
/// The storage backend is returned too so warming tests can share the exact
/// cache the proxy read path uses.
async fn make_flaky_jbm_app(
    upstream_map: batlehub_web::UpstreamMap,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<FlakyJbmRegistry>,
    Arc<dyn StorageBackend>,
) {
    let client = FlakyJbmRegistry::new();
    let repo_dyn: Arc<dyn PackageRepository> = InMemoryRepo::new();
    let storage: Arc<dyn StorageBackend> = InMemoryStorage::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());

    let registries: HashMap<String, Arc<dyn RegistryClient>> =
        [("jbm".to_owned(), client.clone() as Arc<dyn RegistryClient>)].into();
    let policies: HashMap<String, Arc<RegistryPolicy>> = [(
        "jbm".to_owned(),
        Arc::new(RegistryPolicy {
            metadata_ttl: Some(std::time::Duration::from_millis(1)),
            firewall_only: false,
            serve_stale_metadata: true,
            artifact_ttl: None,
            rules: vec![],
        }),
    )]
    .into();

    let hot = new_hot_lock(HotConfig {
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
        storage: storage.clone(),
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
    let mode_map = RegistryModeMap::default();
    mode_map.insert("jbm".to_owned(), RegistryMode::Proxy);

    let app = finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config(&[], &["jbm"]),
        registry_map_for(&[("jbm", "jetbrains-marketplace")]),
        local_svc,
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            upstream_map,
            ..Default::default()
        },
        test_auth_providers(),
    )
    .await;
    (app, client, storage)
}

#[actix_web::test]
async fn jbm_offline_metadata_endpoint_serves_stale() {
    let (app, client, _storage) = make_flaky_jbm_app(batlehub_web::UpstreamMap::default()).await;

    // First hit while the upstream is alive: cached.
    let req = TestRequest::get()
        .uri("/proxy/jbm/plugins/list?pluginId=org.flaky.plugin")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("<version>1.2.0</version>"), "{body}");

    // Upstream dies; the TTL (1 ms) has long expired → stale path.
    client.down.store(true, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let req = TestRequest::get()
        .uri("/proxy/jbm/plugins/list?pluginId=org.flaky.plugin")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "stale metadata must keep serving");
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("<version>1.2.0</version>"), "{body}");

    // A never-seen plugin cannot be served offline.
    let req = TestRequest::get()
        .uri("/proxy/jbm/api/plugins/org.never.seen")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_ne!(resp.status(), 200);
}

#[actix_web::test]
async fn jbm_offline_artifact_still_streams_from_storage() {
    let (app, client, _storage) = make_flaky_jbm_app(batlehub_web::UpstreamMap::default()).await;

    // Fetch once while alive — cached in storage.
    let req = TestRequest::get()
        .uri("/proxy/jbm/plugin/download?pluginId=org.flaky.plugin&version=1.2.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let first = read_body(resp).await;

    client.down.store(true, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let req = TestRequest::get()
        .uri("/proxy/jbm/plugin/download?pluginId=org.flaky.plugin&version=1.2.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        200,
        "cached artifact must survive upstream loss"
    );
    assert_eq!(read_body(resp).await, first);
}

// ── Cache warming ─────────────────────────────────────────────────────────────

/// Warming must land the plugin archive in the very slot `plugin/download`
/// reads from — otherwise a warm run reports success while every download still
/// goes to upstream.
///
/// The key is never spelled out here: whatever key warming wrote is overwritten
/// with a sentinel, and the download endpoint has to return that sentinel.
#[actix_web::test]
async fn jbm_warmed_plugin_lands_in_the_download_cache_slot() {
    let (app, client, storage) = make_flaky_jbm_app(batlehub_web::UpstreamMap::default()).await;

    let warming = WarmingService {
        client: client.clone() as Arc<dyn RegistryClient>,
        storage: storage.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        registry_name: "jbm".to_owned(),
        latest_n: 1,
        concurrency: 2,
        coordinator: Arc::new(NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(ProxyMetrics::new(&["jbm".to_owned()])),
    };
    let report = warming.warm_package("org.flaky.plugin@1.2.0").await;
    assert_eq!(report.warmed, 1, "pinned version must be fetched");
    assert_eq!(report.errors, 0);

    let keys = storage.list_keys("artifact:").await.unwrap();
    assert_eq!(keys.len(), 1, "exactly one artifact warmed: {keys:?}");
    storage
        .store(
            &keys[0],
            Bytes::from_static(b"sentinel-from-warming"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/proxy/jbm/plugin/download?pluginId=org.flaky.plugin&version=1.2.0")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        read_body(resp).await,
        Bytes::from_static(b"sentinel-from-warming"),
        "download must be served from the warmed slot ({}), not re-fetched",
        keys[0]
    );
}

#[actix_web::test]
async fn jbm_offline_forwarded_blob_serves_stale() {
    // Real HTTP upstream for the cached-forward path.
    let mut server = mockito::Server::new_async().await;
    let ok_mock = server
        .mock("GET", "/files/brokenPlugins.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"[{"id":"broken.plugin"}]"#)
        .expect(1)
        .create_async()
        .await;

    let upstream_map =
        batlehub_web::UpstreamMap::new([("jbm".to_owned(), server.url())].into_iter().collect());
    let (app, _client, _storage) = make_flaky_jbm_app(upstream_map).await;

    let req = TestRequest::get()
        .uri("/proxy/jbm/files/brokenPlugins.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let first: Value = read_body_json(resp).await;
    assert_eq!(first[0]["id"], "broken.plugin");
    ok_mock.assert_async().await;

    // Upstream now returns 500; TTL (1 ms) expired → stale body must serve.
    let _err_mock = server
        .mock("GET", "/files/brokenPlugins.json")
        .with_status(500)
        .create_async()
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let req = TestRequest::get()
        .uri("/proxy/jbm/files/brokenPlugins.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "stale forwarded blob must keep serving");
    let second: Value = read_body_json(resp).await;
    assert_eq!(second, first);
}

// ── `jetbrains:channel:assign` ───────────────────────────────────────────────
//
// The one verb in RFC 0015 §4.2's ecosystem table whose action this server could
// already perform on the read side and could not perform at all on the write
// side: `updatePlugins.xml?channel=eap` has always selected on the field, and
// nothing but a republish could set it. These rows cover the endpoint that
// closes that, and the authorization it takes.
//
// The channel is observed through `updatePlugins.xml`, not through an admin
// read-back, because the field only means anything if the *client* sees it move.

/// Every plugin the given channel offers, as an IDE would ask.
async fn channel_ids(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    channel: &str,
) -> String {
    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/local-jbm/updatePlugins.xml?channel={channel}"
        ))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert!(resp.status().is_success(), "channel = {channel}");
    String::from_utf8(read_body(resp).await.to_vec()).expect("xml")
}

/// The positive control and the effect in one: an administrator moves a build
/// from Stable to `eap`, and the two channels swap what they offer.
#[actix_web::test]
async fn jbm_channel_assign_moves_the_build_between_channels() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.chan", "1.0.0", "233.0", "241.*"));
    assert_eq!(
        publish_plugin(&app, &jar, Some("org.demo.chan"), false)
            .await
            .status(),
        201
    );

    assert!(
        channel_ids(&app, "").await.contains("org.demo.chan"),
        "published without a channel field, so it starts in Stable"
    );
    assert!(
        !channel_ids(&app, "eap").await.contains("org.demo.chan"),
        "and the assertion below proves nothing unless eap starts without it"
    );

    let req = TestRequest::put()
        .uri("/api/v1/admin/registries/local-jbm/plugins/org.demo.chan/1.0.0/channel")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "channel": "eap" }))
        .to_request();
    assert_eq!(
        call_service(&app, req).await.status(),
        200,
        "the verb is on the administrative floor; no translation rule produces it"
    );

    assert!(
        channel_ids(&app, "eap").await.contains("org.demo.chan"),
        "eap must now offer the build"
    );
    assert!(
        !channel_ids(&app, "").await.contains("org.demo.chan"),
        "and Stable must stop offering it — a build in two channels at once is \
         not a move, it is a duplicate"
    );
}

/// A plain `USER` cannot repoint a published build.
///
/// This is the row that makes the verb worth having. Publishing is `role:user`
/// work under rule 5, and a publisher who could reassign channels could push a
/// build into Stable for every IDE on the estate without republishing it — the
/// version set would look untouched while what Stable serves had changed.
#[actix_web::test]
async fn jbm_channel_assign_is_refused_for_a_user_and_moves_nothing() {
    let app = make_local_jbm_app(RegistryMode::Local).await;
    let jar = make_plugin_jar(&plugin_xml("org.demo.chan", "1.0.0", "233.0", "241.*"));
    publish_plugin(&app, &jar, Some("org.demo.chan"), false).await;

    for auth in [None, Some(USER_TOKEN)] {
        let mut req = TestRequest::put()
            .uri("/api/v1/admin/registries/local-jbm/plugins/org.demo.chan/1.0.0/channel")
            .set_json(serde_json::json!({ "channel": "eap" }));
        if let Some(token) = auth {
            req = req.insert_header(("Authorization", bearer(token)));
        }
        assert_eq!(
            call_service(&app, req.to_request()).await.status(),
            403,
            "auth = {auth:?}"
        );
    }

    assert!(
        channel_ids(&app, "").await.contains("org.demo.chan"),
        "the build must still be where it was"
    );
    assert!(!channel_ids(&app, "eap").await.contains("org.demo.chan"));
}
