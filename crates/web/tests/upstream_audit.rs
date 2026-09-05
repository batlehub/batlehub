//! RFC 0014 phase 4, in process: a confirmed disappearance reaches a
//! webhook through the *existing* notification machinery — the store, the
//! subscription filter, the channel — with nothing added for it but two
//! event types. The proof is the filter: a subscription on
//! `package_disappeared_upstream` is delivered to, one on
//! `package_published` is not, and both were matched by the same
//! `get_matching_subscriptions` every publish goes through.
//!
//! The sweep runs over a scripted upstream and a seeded inventory (the
//! shape `services/upstream_audit/tests.rs` uses) with the in-memory status
//! store; the receiver is a `mockito` server, so "delivered" is an HTTP
//! request observed, not a method called.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actix_web::test::{call_service, TestRequest};
use async_trait::async_trait;
use batlehub_adapters::in_memory::InMemoryUpstreamStatusStore;
use batlehub_adapters::notification::InMemoryNotificationStore;
use batlehub_config::schema::{
    NotificationChannelConfig, NotificationsConfig, RegistryMode, WebhookChannelConfig,
};
use batlehub_core::{
    entities::{NotificationEventType, NotificationSubscription, PackageId, PackageMetadata},
    error::CoreError,
    ports::{
        ArtifactInventory, ArtifactMeta, FetchedArtifact, NotificationPort, NotificationSink,
        RegistryClient, UpstreamStatusPort,
    },
    services::{HotConfig, UpstreamAuditPolicy, UpstreamAuditService},
};
use batlehub_web::services::{NotificationService, NotificationSinkAdapter};
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

const REG: &str = "npm-audited";

// ── fakes ────────────────────────────────────────────────────────────────────

/// An upstream that lists what it is told and denies everything else.
struct ScriptedUpstream {
    listing: Mutex<HashMap<String, Vec<String>>>,
}

impl ScriptedUpstream {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            listing: Mutex::new(HashMap::new()),
        })
    }
    fn has(&self, name: &str, versions: &[&str]) {
        self.listing.lock().unwrap().insert(
            name.into(),
            versions.iter().map(|v| v.to_string()).collect(),
        );
    }
    fn gone(&self, name: &str) {
        self.listing.lock().unwrap().remove(name);
    }
}

#[async_trait]
impl RegistryClient for ScriptedUpstream {
    fn registry_type(&self) -> &str {
        "npm"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        match self.listing.lock().unwrap().get(&pkg.name) {
            Some(vs) if vs.contains(&pkg.version) => Ok(PackageMetadata::minimal(
                pkg.clone(),
                serde_json::Value::Null,
            )),
            _ => Err(CoreError::NotFound(format!("{pkg} not found"))),
        }
    }
    async fn fetch_artifact(&self, _: &PackageId) -> Result<FetchedArtifact, CoreError> {
        Err(CoreError::NotFound("no bytes in this fake".into()))
    }
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        match self.listing.lock().unwrap().get(package) {
            Some(vs) => Ok(vs.clone()),
            None => Err(CoreError::NotFound(format!("{package} not found"))),
        }
    }
}

struct SeededInventory {
    rows: Mutex<Vec<ArtifactMeta>>,
}

impl SeededInventory {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(Vec::new()),
        })
    }
    fn cached(&self, name: &str, version: &str) {
        self.cached_in(REG, name, version);
    }
    fn cached_in(&self, registry: &str, name: &str, version: &str) {
        let long_ago: DateTime<Utc> = Utc::now() - chrono::Duration::days(30);
        self.rows.lock().unwrap().push(ArtifactMeta {
            artifact_key: format!("{registry}/{name}/{version}"),
            registry: registry.into(),
            package_name: name.into(),
            version: version.into(),
            size_bytes: Some(1),
            cached_at: long_ago,
            last_accessed_at: long_ago,
        });
    }
    /// A path-addressed kind's row: `repo/_`, the file in the key — the
    /// shape `generic.rs` files a download under.
    fn cached_file(&self, registry: &str, path: &str) {
        let long_ago: DateTime<Utc> = Utc::now() - chrono::Duration::days(30);
        self.rows.lock().unwrap().push(ArtifactMeta {
            artifact_key: format!("artifact:{registry}/repo/_/{path}"),
            registry: registry.into(),
            package_name: "repo".into(),
            version: "_".into(),
            size_bytes: Some(1),
            cached_at: long_ago,
            last_accessed_at: long_ago,
        });
    }
}

/// A path-addressed upstream: a file tree that answers a `HEAD` (RFC 0014
/// §13.5) and, like the real client, resolves metadata without asking.
struct ScriptedFiles {
    files: Mutex<HashMap<String, bool>>,
}

impl ScriptedFiles {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            files: Mutex::new(HashMap::new()),
        })
    }
    fn has(&self, path: &str) {
        self.files.lock().unwrap().insert(path.into(), true);
    }
    fn lost(&self, path: &str) {
        self.files.lock().unwrap().insert(path.into(), false);
    }
}

#[async_trait]
impl RegistryClient for ScriptedFiles {
    fn registry_type(&self) -> &str {
        "generic"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata::minimal(
            pkg.clone(),
            serde_json::Value::Null,
        ))
    }
    async fn fetch_artifact(&self, _: &PackageId) -> Result<FetchedArtifact, CoreError> {
        Err(CoreError::NotFound("no bytes in this fake".into()))
    }
    async fn probe_artifact(&self, pkg: &PackageId) -> Result<(), CoreError> {
        let path = pkg.artifact.clone().unwrap_or_default();
        match self.files.lock().unwrap().get(&path) {
            Some(true) => Ok(()),
            _ => Err(CoreError::NotFound(format!("{path} not found upstream"))),
        }
    }
}

#[async_trait]
impl ArtifactInventory for SeededInventory {
    async fn list_artifacts(&self, registry: &str) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.registry == registry)
            .cloned()
            .collect())
    }
    async fn list_artifacts_by_package(&self) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn list_expired_by_ttl(
        &self,
        _: &str,
        _: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }
    async fn list_idle(&self, _: &str, _: DateTime<Utc>) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }
    async fn total_size_bytes(&self, _: &str) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_lru(&self, _: &str, _: i64) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }
}

// ── the lab ──────────────────────────────────────────────────────────────────

fn webhook(name: &str, url: String) -> NotificationChannelConfig {
    NotificationChannelConfig::Webhook(WebhookChannelConfig {
        name: name.to_owned(),
        url,
        secret: None,
        timeout_secs: 5,
    })
}

fn subscription(
    event_types: Vec<NotificationEventType>,
    channel: &str,
    package: Option<&str>,
) -> NotificationSubscription {
    NotificationSubscription {
        id: Uuid::new_v4(),
        registry: Some(REG.to_owned()),
        package_name: package.map(str::to_owned),
        event_types,
        channel_name: channel.to_owned(),
        created_by: "test".into(),
        created_at: Utc::now(),
        enabled: true,
    }
}

struct Lab {
    upstream: Arc<ScriptedUpstream>,
    inventory: Arc<SeededInventory>,
    notifications: Arc<NotificationService>,
    audit: UpstreamAuditService,
}

/// The audit over the notification service, with `channels` configured and
/// `subs` stored — the two halves a publish also goes through.
async fn lab(channels: Vec<NotificationChannelConfig>, subs: Vec<NotificationSubscription>) -> Lab {
    let store: Arc<dyn NotificationPort> = Arc::new(InMemoryNotificationStore::new());
    for sub in subs {
        store.add_subscription(sub).await.unwrap();
    }
    let notifications = Arc::new(NotificationService::new(
        Arc::clone(&store),
        &NotificationsConfig {
            enabled: true,
            channels,
            inbound: vec![],
        },
    ));

    let upstream = ScriptedUpstream::new();
    let inventory = SeededInventory::new();
    let mut hot = HotConfig::default();
    hot.registries
        .insert(REG.into(), Arc::clone(&upstream) as Arc<dyn RegistryClient>);
    let audit = UpstreamAuditService::new(
        Arc::clone(&inventory) as Arc<dyn ArtifactInventory>,
        InMemoryUpstreamStatusStore::new() as Arc<dyn UpstreamStatusPort>,
        Arc::new(RwLock::new(hot)),
        None,
        None,
        UpstreamAuditPolicy {
            confirm_after: 2,
            confirm_min_age: Duration::ZERO,
            skip_recently_seen: false,
            ..Default::default()
        },
        4,
        vec![REG.into()],
    )
    .with_notifier(
        Arc::new(NotificationSinkAdapter(Arc::clone(&notifications))) as Arc<dyn NotificationSink>,
    );
    Lab {
        upstream,
        inventory,
        notifications,
        audit,
    }
}

/// Two sweeps: the first records the miss, the second confirms it.
async fn confirm(lab: &Lab) {
    lab.audit.run_sweep().await;
    let report = lab.audit.run_sweep().await;
    assert_eq!(report.registries[0].confirmed(), 1, "{report:?}");
    // Delivery is a background task; wait for it the way shutdown does.
    lab.notifications.shutdown().await;
}

// ── phase 6: the block arm, through the registry's own endpoints ─────────────
//
// RFC 0006's lesson, applied to RFC 0014 §10: a block the admin API believes
// and the packument does not is not a block. So the assertion is on what a
// client resolves against — npm's packument and NuGet's flat index — with
// `get_status` nowhere in it.

/// A local npm app whose registry the audit also sweeps: the packages are
/// published through the API, the "cache" the sweep reads is seeded, and
/// the upstream is scripted. Local mode, so the packument is rendered from
/// the backend and the scripted client answers only the probe.
async fn blocking_lab(
    name: &str,
    kind: &str,
) -> (
    impl TestService,
    Arc<ScriptedUpstream>,
    Arc<SeededInventory>,
    Arc<UpstreamAuditService>,
) {
    use batlehub_core::services::OnConfirmed;
    let parts = local_registry_app_parts(name, kind, RegistryMode::Local, None);
    let upstream = ScriptedUpstream::new();
    parts.proxy_svc.hot.write().await.registries.insert(
        name.to_owned(),
        Arc::clone(&upstream) as Arc<dyn RegistryClient>,
    );
    let inventory = SeededInventory::new();
    let audit = UpstreamAuditService::new(
        Arc::clone(&inventory) as Arc<dyn ArtifactInventory>,
        InMemoryUpstreamStatusStore::new() as Arc<dyn UpstreamStatusPort>,
        parts.proxy_svc.hot.clone(),
        None,
        None,
        UpstreamAuditPolicy {
            confirm_after: 2,
            confirm_min_age: Duration::ZERO,
            skip_recently_seen: false,
            on_confirmed: OnConfirmed::Block,
            ..Default::default()
        },
        4,
        vec![name.to_owned()],
    )
    .with_admin(Arc::clone(&parts.admin_svc));
    let audit = Arc::new(audit);
    // Registered as the server registers it, so the admin routes answer.
    let app = build_local_registry_app_with_defaults(
        parts,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            upstream_audit: Some(Arc::clone(&audit)),
            ..Default::default()
        },
    )
    .await;
    (app, upstream, inventory, audit)
}

fn npm_publish_payload(name: &str, version: &str) -> serde_json::Value {
    use base64::Engine;
    let tarball_b64 = base64::engine::general_purpose::STANDARD.encode(b"fake-tarball-content");
    serde_json::json!({
        "name": name,
        "versions": { version: { "name": name, "version": version, "dist": { "shasum": "abc123" } } },
        "_attachments": {
            format!("{name}-{version}.tgz"): {
                "content_type": "application/octet-stream", "data": tarball_b64, "length": 20
            }
        }
    })
}

async fn npm_versions<S: TestService>(app: &S, uri: &str) -> Vec<String> {
    let doc = get_json(app, uri).await;
    let mut v: Vec<String> = doc["versions"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[actix_web::test]
async fn under_block_the_npm_packument_omits_a_confirmed_version_and_lists_it_again_on_reappearance(
) {
    let (app, upstream, inventory, audit) = blocking_lab("audited-npm", "npm").await;
    for v in ["1.0.0", "1.1.0"] {
        let req = actix_web::test::TestRequest::put()
            .uri("/proxy/audited-npm/left-pad")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(npm_publish_payload("left-pad", v))
            .to_request();
        assert_eq!(actix_web::test::call_service(&app, req).await.status(), 200);
    }
    let packument = "/proxy/audited-npm/left-pad";
    assert_eq!(npm_versions(&app, packument).await, ["1.0.0", "1.1.0"]);

    // The sweep's picture: both versions came from upstream, and upstream
    // now lists only the first.
    inventory.cached_in("audited-npm", "left-pad", "1.0.0");
    inventory.cached_in("audited-npm", "left-pad", "1.1.0");
    upstream.has("left-pad", &["1.0.0"]);
    audit.run_sweep().await;
    assert_eq!(
        npm_versions(&app, packument).await,
        ["1.0.0", "1.1.0"],
        "one miss changes nothing"
    );
    let report = audit.run_sweep().await;
    assert_eq!(report.registries[0].confirmed(), 1, "{report:?}");
    assert_eq!(
        npm_versions(&app, packument).await,
        ["1.0.0"],
        "confirmed: the packument no longer names it"
    );
    let tarball =
        actix_web::test::call_service(&app, admin_get("/proxy/audited-npm/left-pad/1.1.0/tarball"))
            .await;
    assert_eq!(tarball.status(), 403, "and the bytes are refused");

    upstream.has("left-pad", &["1.0.0", "1.1.0"]);
    let report = audit.run_sweep().await;
    assert_eq!(report.registries[0].reappeared(), 1, "{report:?}");
    assert_eq!(
        npm_versions(&app, packument).await,
        ["1.0.0", "1.1.0"],
        "reappeared: the audit lifted its own block"
    );
}

#[actix_web::test]
async fn under_block_the_nuget_flat_index_omits_a_confirmed_version() {
    let (app, upstream, inventory, audit) = blocking_lab("audited-nuget", "nuget").await;
    for v in ["1.0.0", "2.0.0"] {
        let nupkg = sample_nupkg("Held.Lib", v);
        let boundary = "nugetboundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"package\"; filename=\"package.nupkg\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(&nupkg);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let req = actix_web::test::TestRequest::put()
            .uri("/proxy/audited-nuget/nuget/api/v2/package")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .insert_header((
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            ))
            .set_payload(body)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success(), "{}", resp.status());
    }
    let flat = "/proxy/audited-nuget/nuget/v3/flat/held.lib/index.json";
    let versions = |doc: serde_json::Value| -> Vec<String> {
        doc["versions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(versions(get_json(&app, flat).await), ["1.0.0", "2.0.0"]);

    inventory.cached_in("audited-nuget", "held.lib", "1.0.0");
    inventory.cached_in("audited-nuget", "held.lib", "2.0.0");
    upstream.has("held.lib", &["1.0.0"]);
    audit.run_sweep().await;
    let report = audit.run_sweep().await;
    assert_eq!(report.registries[0].confirmed(), 1, "{report:?}");
    assert_eq!(
        versions(get_json(&app, flat).await),
        ["1.0.0"],
        "the flat index — what `dotnet` resolves against — no longer names it"
    );
}

/// A minimal `.nupkg`: a zip holding one `.nuspec`.
fn sample_nupkg(id: &str, version: &str) -> Vec<u8> {
    use std::io::Write;
    let nuspec = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata><id>{id}</id><version>{version}</version><description>held</description><authors>a</authors></metadata>
</package>"#
    );
    let mut buf = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
    zip.start_file(
        format!("{id}.nuspec"),
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(nuspec.as_bytes()).unwrap();
    zip.finish().unwrap();
    buf
}

// ── phase 7: the admin API ───────────────────────────────────────────────────

async fn get_as<S: TestService>(
    app: &S,
    uri: &str,
    token: Option<&str>,
) -> actix_web::dev::ServiceResponse {
    let mut req = actix_web::test::TestRequest::get().uri(uri);
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    actix_web::test::call_service(app, req.to_request()).await
}

/// The listing's shape, its filters and its pages; the per-package status;
/// `403` for a non-admin.
#[actix_web::test]
async fn the_admin_listing_has_the_rows_the_policy_and_the_counts() {
    let (app, upstream, inventory, audit) = blocking_lab("audited-npm", "npm").await;
    for (name, held, up) in [
        ("gone", vec!["1.0.0", "1.1.0"], vec![]),
        ("wobbly", vec!["2.0.0"], vec!["1.0.0"]),
        ("steady", vec!["3.0.0"], vec!["3.0.0"]),
    ] {
        for v in &held {
            inventory.cached_in("audited-npm", name, v);
        }
        if up.is_empty() {
            upstream.gone(name);
        } else {
            upstream.has(name, &up);
        }
    }
    // One sweep: two misses, nothing confirmed. Then `wobbly` comes back and
    // a second sweep confirms `gone`.
    audit.run_sweep().await;
    upstream.has("wobbly", &["1.0.0", "2.0.0"]);
    audit.run_sweep().await;

    let resp = get_as(
        &app,
        "/api/v1/admin/upstream/disappeared",
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let page: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(page["policy"], "block");
    assert_eq!(page["registries"], serde_json::json!(["audited-npm"]));
    assert_eq!(page["total"], 1, "{page}");
    assert_eq!(page["items"][0]["package_name"], "gone");
    assert_eq!(page["items"][0]["state"], "disappeared");
    assert!(
        page["items"][0].get("version").is_none(),
        "whole-package row"
    );
    assert_eq!(page["items"][0]["consecutive_misses"], 2);
    assert_eq!(
        page["counts"],
        serde_json::json!([{ "registry": "audited-npm", "missing": 0, "disappeared": 1, "policy": "block", "overridden": false }])
    );

    // Filters: state, registry; an unknown state is a 400.
    let resp = get_as(
        &app,
        "/api/v1/admin/upstream/disappeared?state=missing",
        Some(ADMIN_TOKEN),
    )
    .await;
    let page: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(page["total"], 0);
    let resp = get_as(
        &app,
        "/api/v1/admin/upstream/disappeared?registry=other",
        Some(ADMIN_TOKEN),
    )
    .await;
    let page: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(page["total"], 0);
    assert_eq!(
        get_as(
            &app,
            "/api/v1/admin/upstream/disappeared?state=vanished",
            Some(ADMIN_TOKEN)
        )
        .await
        .status(),
        400
    );

    // Pagination: an empty second page, the total unchanged.
    let resp = get_as(
        &app,
        "/api/v1/admin/upstream/disappeared?per_page=1&page=1",
        Some(ADMIN_TOKEN),
    )
    .await;
    let page: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"].as_array().unwrap().len(), 0);
    assert_eq!(page["per_page"], 1);

    // The per-package status: rows for the gone one, none for the present.
    let resp = get_as(
        &app,
        "/api/v1/admin/upstream/status/audited-npm/gone",
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let status: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(status["rows"][0]["state"], "disappeared");
    assert_eq!(status["policy"], "block");
    let resp = get_as(
        &app,
        "/api/v1/admin/upstream/status/audited-npm/steady",
        Some(ADMIN_TOKEN),
    )
    .await;
    let status: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(
        status["rows"].as_array().unwrap().len(),
        0,
        "present is no row"
    );
    assert_eq!(
        get_as(
            &app,
            "/api/v1/admin/upstream/status/nowhere/steady",
            Some(ADMIN_TOKEN)
        )
        .await
        .status(),
        404
    );

    // Not an admin: refused.
    assert_eq!(
        get_as(&app, "/api/v1/admin/upstream/disappeared", Some(USER_TOKEN))
            .await
            .status(),
        403
    );
    assert_eq!(
        get_as(&app, "/api/v1/admin/upstream/disappeared", None)
            .await
            .status(),
        403
    );
}

/// A process without the audit — proxy-only, or the section off — says so.
#[actix_web::test]
async fn without_the_audit_the_admin_routes_answer_503() {
    let parts = local_registry_app_parts("plain-npm", "npm", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    for uri in [
        "/api/v1/admin/upstream/disappeared",
        "/api/v1/admin/upstream/status/plain-npm/x",
    ] {
        assert_eq!(
            get_as(&app, uri, Some(ADMIN_TOKEN)).await.status(),
            503,
            "{uri}"
        );
    }
    let req = actix_web::test::TestRequest::post()
        .uri("/api/v1/admin/upstream/recheck")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({ "registry": "plain-npm", "package_name": "x" }))
        .to_request();
    assert_eq!(actix_web::test::call_service(&app, req).await.status(), 503);
}

// ── the tests ────────────────────────────────────────────────────────────────

/// The new type routes through the existing filter: the subscription that
/// names it is delivered to, the one that names another type is not.
#[actix_web::test]
async fn a_confirmed_disappearance_reaches_the_subscription_that_named_it() {
    let mut receiver = mockito::Server::new_async().await;
    let gone = receiver
        .mock("POST", "/gone")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "event_type": "package_disappeared_upstream",
            "registry": REG,
            "package_name": "withdrawn",
            "actor": "system:upstream-audit",
            "metadata": { "policy": "audit", "blocked": false, "probe": "package" }
        })))
        .with_status(200)
        .expect(1)
        .create_async()
        .await;
    let published = receiver
        .mock("POST", "/published")
        .with_status(200)
        .expect(0)
        .create_async()
        .await;

    let lab = lab(
        vec![
            webhook("gone", format!("{}/gone", receiver.url())),
            webhook("published", format!("{}/published", receiver.url())),
        ],
        vec![
            subscription(
                vec![NotificationEventType::PackageDisappearedUpstream],
                "gone",
                None,
            ),
            subscription(
                vec![NotificationEventType::PackagePublished],
                "published",
                None,
            ),
        ],
    )
    .await;
    lab.inventory.cached("withdrawn", "1.0.0");
    lab.upstream.gone("withdrawn");

    confirm(&lab).await;

    gone.assert_async().await;
    published.assert_async().await;
}

/// A package filter on the subscription is honoured for the new type too —
/// and a package-scoped subscription never sees the registry-scoped
/// `upstream_unreachable`, whose package is `*`.
#[actix_web::test]
async fn the_package_filter_applies_to_the_new_types() {
    let mut receiver = mockito::Server::new_async().await;
    let mine = receiver
        .mock("POST", "/mine")
        .with_status(200)
        .expect(1)
        .create_async()
        .await;
    let other = receiver
        .mock("POST", "/other")
        .with_status(200)
        .expect(0)
        .create_async()
        .await;

    let lab = lab(
        vec![
            webhook("mine", format!("{}/mine", receiver.url())),
            webhook("other", format!("{}/other", receiver.url())),
        ],
        vec![
            subscription(
                vec![
                    NotificationEventType::PackageDisappearedUpstream,
                    NotificationEventType::UpstreamUnreachable,
                ],
                "mine",
                Some("withdrawn"),
            ),
            subscription(
                vec![
                    NotificationEventType::PackageDisappearedUpstream,
                    NotificationEventType::UpstreamUnreachable,
                ],
                "other",
                Some("somebody-else"),
            ),
        ],
    )
    .await;
    lab.inventory.cached("withdrawn", "1.0.0");
    lab.upstream.gone("withdrawn");

    confirm(&lab).await;

    mine.assert_async().await;
    other.assert_async().await;
}

/// A reappearance is the second kind, delivered through the same path.
#[actix_web::test]
async fn a_reappearance_is_delivered_as_its_own_type() {
    let mut receiver = mockito::Server::new_async().await;
    let back = receiver
        .mock("POST", "/back")
        .match_body(mockito::Matcher::PartialJson(serde_json::json!({
            "event_type": "package_reappeared_upstream",
            "package_name": "withdrawn",
            "version": "1.0.0",
            "metadata": { "unblocked": false }
        })))
        .with_status(200)
        .expect(1)
        .create_async()
        .await;

    let lab = lab(
        vec![webhook("back", format!("{}/back", receiver.url()))],
        vec![subscription(
            vec![NotificationEventType::PackageReappearedUpstream],
            "back",
            None,
        )],
    )
    .await;
    lab.inventory.cached("withdrawn", "1.0.0");
    // The package exists but the cached version is gone: a version-level row.
    lab.upstream.has("withdrawn", &["0.9.0"]);
    confirm(&lab).await;
    lab.upstream.has("withdrawn", &["0.9.0", "1.0.0"]);
    let report = lab.audit.run_sweep().await;
    assert_eq!(report.registries[0].reappeared(), 1, "{report:?}");
    lab.notifications.shutdown().await;

    back.assert_async().await;
}

// ── RFC 0014 §13.5: a path-addressed registry, probed per file ──────────────

/// Under `"block"`, a file a `generic` registry holds and its upstream has
/// lost is refused on its own route once confirmed, and served again once
/// it is back — the block is placed on the file's coordinate (`repo/_` with
/// the path as the artifact), never on the bare `repo/_` that would be every
/// file of the registry.
#[actix_web::test]
async fn under_block_a_confirmed_file_of_a_generic_registry_is_refused_on_its_path() {
    use batlehub_core::services::OnConfirmed;
    const GEN: &str = "audited-files";
    let parts = local_registry_app_parts(GEN, "generic", RegistryMode::Proxy, None);
    let upstream = ScriptedFiles::new();
    parts.proxy_svc.hot.write().await.registries.insert(
        GEN.to_owned(),
        Arc::clone(&upstream) as Arc<dyn RegistryClient>,
    );
    let inventory = SeededInventory::new();
    let audit = Arc::new(
        UpstreamAuditService::new(
            Arc::clone(&inventory) as Arc<dyn ArtifactInventory>,
            InMemoryUpstreamStatusStore::new() as Arc<dyn UpstreamStatusPort>,
            parts.proxy_svc.hot.clone(),
            None,
            None,
            UpstreamAuditPolicy {
                confirm_after: 2,
                confirm_min_age: Duration::ZERO,
                skip_recently_seen: false,
                on_confirmed: OnConfirmed::Block,
                ..Default::default()
            },
            4,
            vec![GEN.to_owned()],
        )
        .with_admin(Arc::clone(&parts.admin_svc)),
    );
    // Twelve files upstream still has, and the one it will lose — held
    // here, bytes and all, the way a served download leaves them.
    for i in 0..12 {
        let path = format!("node/v20.0.{i}/node.tar.gz");
        inventory.cached_file(GEN, &path);
        upstream.has(&path);
    }
    let gone = "node/v18.0.0/node-v18.0.0-linux-x64.tar.gz";
    inventory.cached_file(GEN, gone);
    upstream.has(gone);
    parts
        .proxy_svc
        .storage
        .store(
            &format!("artifact:{GEN}/repo/_/{gone}"),
            bytes::Bytes::from_static(b"the tarball"),
            Default::default(),
        )
        .await
        .unwrap();
    let app = build_local_registry_app_with_defaults(
        parts,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            upstream_audit: Some(Arc::clone(&audit)),
            ..Default::default()
        },
    )
    .await;
    let route = format!("/proxy/{GEN}/generic/{gone}");
    let other = format!("/proxy/{GEN}/generic/node/v20.0.0/node.tar.gz");

    let resp = call_service(&app, TestRequest::get().uri(&route).to_request()).await;
    assert_eq!(resp.status(), 200, "held and upstream has it");

    upstream.lost(gone);
    audit.run_sweep().await;
    let resp = call_service(&app, TestRequest::get().uri(&route).to_request()).await;
    assert_eq!(resp.status(), 200, "one miss blocks nothing");
    let report = audit.run_sweep().await;
    assert_eq!(report.registries[0].confirmed(), 1, "{report:?}");

    let resp = call_service(&app, TestRequest::get().uri(&route).to_request()).await;
    assert_eq!(
        resp.status(),
        403,
        "confirmed: the file is refused on its route"
    );
    // The bare coordinate was not blocked: another file of the same
    // registry is still resolvable (its bytes are not held, so the fake
    // upstream's refusal to stream is the answer — not a 403).
    let resp = call_service(&app, TestRequest::get().uri(&other).to_request()).await;
    assert_ne!(
        resp.status(),
        403,
        "a block on repo/_ would refuse every file"
    );

    upstream.has(gone);
    let report = audit.run_sweep().await;
    assert_eq!(report.registries[0].reappeared(), 1, "{report:?}");
    let resp = call_service(&app, TestRequest::get().uri(&route).to_request()).await;
    assert_eq!(
        resp.status(),
        200,
        "back upstream: the audit's block is lifted"
    );

    // The status row and the recheck name the file, not a version.
    let report = audit.recheck(GEN, "repo", Some(gone)).await.unwrap();
    assert_eq!(report.probed, 1);
}

// ── RFC 0014 §13 O6: the registry-tier `on_confirmed` on the admin API ──────

/// The estate says audit and one registry's own row says block: the
/// listing reports both, the package status reports the registry's, and
/// the block lands.
#[actix_web::test]
async fn the_listing_reports_the_registrys_own_policy_beside_the_estates() {
    use batlehub_core::entities::{RegistryKind, RegistryPolicyTiers};
    use batlehub_core::services::OnConfirmed;
    let name = "tiered-npm";
    let parts = local_registry_app_parts(name, "npm", RegistryMode::Local, None);
    let upstream = ScriptedUpstream::new();
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            name.to_owned(),
            Arc::clone(&upstream) as Arc<dyn RegistryClient>,
        );
        let mut tiers = RegistryPolicyTiers::open(RegistryKind::Npm, name);
        tiers.registry.on_confirmed = Some(OnConfirmed::Block);
        hot.policy_tiers.insert(name.to_owned(), Arc::new(tiers));
    }
    let inventory = SeededInventory::new();
    let audit = Arc::new(
        UpstreamAuditService::new(
            Arc::clone(&inventory) as Arc<dyn ArtifactInventory>,
            InMemoryUpstreamStatusStore::new() as Arc<dyn UpstreamStatusPort>,
            parts.proxy_svc.hot.clone(),
            None,
            None,
            UpstreamAuditPolicy {
                confirm_after: 2,
                confirm_min_age: Duration::ZERO,
                skip_recently_seen: false,
                on_confirmed: OnConfirmed::Audit,
                ..Default::default()
            },
            4,
            vec![name.to_owned()],
        )
        .with_admin(Arc::clone(&parts.admin_svc)),
    );
    let app = build_local_registry_app_with_defaults(
        parts,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            upstream_audit: Some(Arc::clone(&audit)),
            ..Default::default()
        },
    )
    .await;
    for i in 0..12 {
        let pkg = format!("steady-{i}");
        inventory.cached_in(name, &pkg, "1.0.0");
        upstream.has(&pkg, &["1.0.0"]);
    }
    inventory.cached_in(name, "gone", "1.0.0");
    upstream.gone("gone");
    audit.run_sweep().await;
    let report = audit.run_sweep().await;
    assert_eq!(report.registries[0].confirmed(), 1, "{report:?}");

    let resp = get_as(
        &app,
        "/api/v1/admin/upstream/disappeared",
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let page: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(page["policy"], "audit", "the estate's key");
    assert_eq!(page["counts"][0]["registry"], name);
    assert_eq!(page["counts"][0]["policy"], "block", "the registry's row");
    assert_eq!(page["counts"][0]["overridden"], true);

    let resp = get_as(
        &app,
        &format!("/api/v1/admin/upstream/status/{name}/gone"),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let status: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(status["policy"], "block");
}
