//! RFC 0051 — `package_allowlist`, driven through the HTTP surface.
//!
//! The rule's own logic is unit-tested in `crates/core/src/rules/package_allowlist.rs`.
//! This suite proves what only the funnels can: an unlisted name is refused on
//! every read path **before any upstream call** (a counting client asserts
//! zero), a listed name at an out-of-range version is refused after the
//! resolve, local rows are judged too, and whole-registry documents keep
//! working under an allowlist.

mod common;
use common::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use async_trait::async_trait;
use base64::Engine as _;
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{PackageId, PackageMetadata, RegistryKind, Role};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument};
use batlehub_core::rules::{
    Allowlist, AllowlistEntrySpec, BlockListRule, PackageAllowlistRule,
    PackageAllowlistVersionRule, Rule,
};
use batlehub_core::services::RegistryPolicy;
use serde_json::Value;

/// A `FixedRegistry` that counts every upstream-facing call.
struct Counting {
    inner: Arc<FixedRegistry>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl RegistryClient for Counting {
    fn registry_type(&self) -> &str {
        self.inner.registry_type()
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.resolve_metadata(pkg).await
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch_artifact(pkg).await
    }

    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch_version_document(package, kind).await
    }
}

fn entry(name: &str, versions: Option<&[&str]>) -> AllowlistEntrySpec {
    AllowlistEntrySpec {
        name: name.to_owned(),
        versions: versions.map(|vs| vs.iter().map(|v| (*v).to_owned()).collect()),
    }
}

/// Build the app for `reg` of `kind`, with `entries` as its allowlist at the
/// head of the chain (as `server/src/builders.rs` puts it), and a counting
/// client in place of the fixture's.
async fn app_with_allowlist(
    reg: &str,
    kind: RegistryKind,
    mode: RegistryMode,
    entries: &[AllowlistEntrySpec],
) -> (impl TestService, Arc<AtomicUsize>) {
    let parts = local_registry_app_parts(reg, kind.as_str(), mode, None);
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            reg.to_owned(),
            Arc::new(Counting {
                inner: FixedRegistry::new(kind.as_str()),
                calls: Arc::clone(&calls),
            }),
        );
        let list = Arc::new(Allowlist::new(kind, entries, vec![Role::Admin]));
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(PackageAllowlistRule::new(Arc::clone(&list))),
            Box::new(PackageAllowlistVersionRule::new(list)),
            Box::new(BlockListRule::new(Arc::clone(&parts.proxy_svc.repo))),
        ];
        hot.policies.insert(
            reg.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules,
            }),
        );
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, calls)
}

async fn get<S: TestService>(app: &S, uri: &str, token: &str) -> (u16, String) {
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(uri)
            .insert_header(("Authorization", bearer(token)))
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    let body = String::from_utf8_lossy(&read_body(resp).await).into_owned();
    (status, body)
}

// ── Proxy: refused before any upstream call ───────────────────────────────────

#[actix_web::test]
async fn unlisted_tarball_is_refused_with_no_upstream_call() {
    let (app, calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    let (status, body) = get(&app, "/proxy/npm-p/left-pad/1.3.0/tarball", USER_TOKEN).await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("package allowlist: 'left-pad' is not listed for registry 'npm-p'"),
        "{body}"
    );
    assert!(body.contains("bypass requires role 'admin'"), "{body}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the refused name reached the upstream"
    );
}

#[actix_web::test]
async fn unlisted_packument_is_refused_with_no_upstream_call() {
    let (app, calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    let (status, body) = get(&app, "/proxy/npm-p/left-pad", USER_TOKEN).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the refused listing reached the upstream"
    );
}

#[actix_web::test]
async fn listed_tarball_is_served() {
    let (app, calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    let (status, body) = get(&app, "/proxy/npm-p/react/18.3.1/tarball", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    assert!(calls.load(Ordering::SeqCst) > 0);
}

#[actix_web::test]
async fn out_of_range_version_is_refused_after_the_resolve() {
    let (app, _calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("lodash", Some(&[">=4.17.21, <5"]))],
    )
    .await;
    let (status, body) = get(&app, "/proxy/npm-p/lodash/4.17.20/tarball", USER_TOKEN).await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("version '4.17.20' of 'lodash' is not allowed for registry 'npm-p'"),
        "{body}"
    );
    assert!(
        !body.contains("4.17.21"),
        "the range must not be disclosed: {body}"
    );
    let (status, body) = get(&app, "/proxy/npm-p/lodash/4.17.21/tarball", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
}

#[actix_web::test]
async fn bypass_role_is_served_an_unlisted_package() {
    let (app, _calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    let (status, body) = get(&app, "/proxy/npm-p/left-pad/1.3.0/tarball", ADMIN_TOKEN).await;
    assert_eq!(status, 200, "{body}");
}

// ── why: the name is judged before the upstream is asked ──────────────────────

#[actix_web::test]
async fn why_names_the_allowlist_without_asking_the_upstream() {
    let (app, calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/packages/access?registry=npm-p&name=reqeusts&version=1.0.0")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let causes = body["causes"].as_array().expect("causes");
    assert!(
        causes
            .iter()
            .any(|c| c["reason_code"] == "PACKAGE_NOT_ALLOWED" && c["state"] == "refuse"),
        "{body}"
    );
    assert!(
        !causes.iter().any(|c| c["kind"] == "upstream"),
        "no upstream cause: nothing was asked: {body}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "why reached the upstream");
}

// ── Local rows are judged too (§15 decision 1) ────────────────────────────────

fn npm_publish_payload(name: &str, version: &str) -> Value {
    let tarball_b64 = base64::engine::general_purpose::STANDARD.encode(b"fake-tarball-content");
    serde_json::json!({
        "name": name,
        "versions": { version: { "name": name, "version": version, "dist": { "shasum": "abc" } } },
        "_attachments": {
            format!("{name}-{version}.tgz"): {
                "content_type": "application/octet-stream",
                "data": tarball_b64,
                "length": 20
            }
        }
    })
}

#[actix_web::test]
async fn an_unlisted_local_package_is_published_but_never_served() {
    let (app, _calls) = app_with_allowlist(
        "npm-l",
        RegistryKind::Npm,
        RegistryMode::Local,
        &[entry("@acme/*", None)],
    )
    .await;
    for name in ["@acme/widget", "@other/thing"] {
        let resp = call_service(
            &app,
            TestRequest::put()
                .uri(&format!("/proxy/npm-l/{}", name.replace('/', "%2f")))
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .set_json(npm_publish_payload(name, "1.0.0"))
                .to_request(),
        )
        .await;
        assert_eq!(
            resp.status(),
            200,
            "publish is a grant, not this rule's: {name}"
        );
    }
    let (status, body) = get(&app, "/proxy/npm-l/@acme%2fwidget", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = get(&app, "/proxy/npm-l/@other%2fthing", USER_TOKEN).await;
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("package allowlist"), "{body}");
}

// ── Whole-registry documents keep working ─────────────────────────────────────

#[actix_web::test]
async fn cargo_config_json_is_served_under_an_allowlist() {
    let (app, _calls) = app_with_allowlist(
        "cargo-l",
        RegistryKind::Cargo,
        RegistryMode::Local,
        &[entry("serde", None)],
    )
    .await;
    let (status, body) = get(&app, "/proxy/cargo-l/registry/config.json", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
}

#[actix_web::test]
async fn pypi_simple_root_is_served_under_an_allowlist() {
    let (app, _calls) = app_with_allowlist(
        "pypi-p",
        RegistryKind::Pypi,
        RegistryMode::Proxy,
        &[entry("requests", None)],
    )
    .await;
    let (status, body) = get(&app, "/proxy/pypi-p/simple/", USER_TOKEN).await;
    assert_ne!(
        status, 403,
        "the simple root is a registry document: {body}"
    );
}

#[actix_web::test]
async fn pypi_spellings_of_a_listed_name_are_served() {
    let (app, _calls) = app_with_allowlist(
        "pypi-p",
        RegistryKind::Pypi,
        RegistryMode::Proxy,
        &[entry("python-dateutil", None)],
    )
    .await;
    for spelling in ["python-dateutil", "Python.Dateutil", "python_dateutil"] {
        let (status, body) = get(
            &app,
            &format!("/proxy/pypi-p/simple/{spelling}/"),
            USER_TOKEN,
        )
        .await;
        assert_ne!(status, 403, "{spelling}: {body}");
    }
    let (status, _) = get(&app, "/proxy/pypi-p/simple/reqeusts/", USER_TOKEN).await;
    assert_eq!(status, 403);
}
