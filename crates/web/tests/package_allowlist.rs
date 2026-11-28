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

    /// Not counted: resolving an alias is the one upstream lookup RFC 0051
    /// T13 accepts before the name can be judged.
    async fn canonical_coordinate(&self, pkg: &PackageId) -> Result<Option<PackageId>, CoreError> {
        self.inner.canonical_coordinate(pkg).await
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

// ── why, without a version ────────────────────────────────────────────────────

#[actix_web::test]
async fn why_without_a_version_names_the_allowlist_without_asking_the_upstream() {
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
            .uri("/api/v1/packages/access?registry=npm-p&name=left-pad")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["headline"], "refused", "{body}");
    let versions = body["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 1, "{body}");
    assert_eq!(versions[0]["version"], "", "{body}");
    assert!(
        body["causes"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["reason_code"] == "PACKAGE_NOT_ALLOWED"),
        "{body}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "why reached the upstream");
}

// ── The access log carries the code (`why --last`) ────────────────────────────

async fn refusal_code<S: TestService>(app: &S, name: &str) -> Value {
    let rows: Value = read_body_json(
        call_service(
            app,
            TestRequest::get()
                .uri("/api/v1/me/downloads?outcome=denied")
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    rows.as_array()
        .into_iter()
        .flatten()
        .chain(rows["items"].as_array().into_iter().flatten())
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no refusal row for {name}: {rows}"))["reason_code"]
        .clone()
}

#[actix_web::test]
async fn refusals_are_recorded_with_their_reason_code() {
    let (app, _calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("lodash", Some(&[">=4.17.21, <5"]))],
    )
    .await;
    assert_eq!(
        get(&app, "/proxy/npm-p/left-pad/1.3.0/tarball", USER_TOKEN)
            .await
            .0,
        403
    );
    assert_eq!(
        get(&app, "/proxy/npm-p/lodash/4.17.20/tarball", USER_TOKEN)
            .await
            .0,
        403
    );
    assert_eq!(refusal_code(&app, "left-pad").await, "PACKAGE_NOT_ALLOWED");
    assert_eq!(refusal_code(&app, "lodash").await, "VERSION_NOT_ALLOWED");
}

#[actix_web::test]
async fn a_refused_listing_is_recorded_with_its_reason_code() {
    let (app, _calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    assert_eq!(get(&app, "/proxy/npm-p/left-pad", USER_TOKEN).await.0, 403);
    // A listing refusal is a metadata view, not a download: it is in the
    // administrator's audit log rather than in `me/downloads`.
    let body: Value = read_body_json(
        call_service(
            &app,
            TestRequest::get()
                .uri("/api/v1/admin/audit-log")
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .to_request(),
        )
        .await,
    )
    .await;
    let row = body["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["package_id"]["name"] == "left-pad")
        .unwrap_or_else(|| panic!("no audit row for left-pad: {body}"));
    assert_eq!(row["reason_code"], "PACKAGE_NOT_ALLOWED", "{row}");
}

// ── T9: a namespace's list replaces the registry's for the names under it ─────

#[actix_web::test]
async fn namespace_allowlist_replaces_registry_allowlist() {
    let (app, _calls) = app_with_allowlist_and_namespace(
        "npm-p",
        RegistryKind::Npm,
        &[entry("react", None)],
        ("@acme", &[entry("@acme/*", None)]),
    )
    .await;
    let ok = |s: u16| s == 200;
    assert!(ok(get(
        &app,
        "/proxy/npm-p/react/18.3.1/tarball",
        USER_TOKEN
    )
    .await
    .0));
    assert!(ok(get(
        &app,
        "/proxy/npm-p/@acme%2fwidget/1.0.0/tarball",
        USER_TOKEN
    )
    .await
    .0));
    // Under the namespace, the registry's list no longer applies: `react` is
    // not on the namespace's list, but `react` is not under `@acme` either.
    assert_eq!(
        get(&app, "/proxy/npm-p/left-pad/1.3.0/tarball", USER_TOKEN)
            .await
            .0,
        403
    );
}

async fn app_with_allowlist_and_namespace(
    reg: &str,
    kind: RegistryKind,
    registry_entries: &[AllowlistEntrySpec],
    (prefix, ns_entries): (&str, &[AllowlistEntrySpec]),
) -> (impl TestService, Arc<AtomicUsize>) {
    let parts = local_registry_app_parts(reg, kind.as_str(), RegistryMode::Proxy, None);
    let calls = Arc::new(AtomicUsize::new(0));
    let policy = |entries: &[AllowlistEntrySpec]| {
        let list = Arc::new(Allowlist::new(kind, entries, vec![]));
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(PackageAllowlistRule::new(Arc::clone(&list))),
            Box::new(PackageAllowlistVersionRule::new(list)),
            Box::new(BlockListRule::new(Arc::clone(&parts.proxy_svc.repo))),
        ];
        Arc::new(RegistryPolicy {
            metadata_ttl: Some(Duration::from_secs(300)),
            firewall_only: false,
            serve_stale_metadata: false,
            artifact_ttl: None,
            rules,
        })
    };
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            reg.to_owned(),
            Arc::new(Counting {
                inner: FixedRegistry::new(kind.as_str()),
                calls: Arc::clone(&calls),
            }),
        );
        hot.policies
            .insert(reg.to_owned(), policy(registry_entries));
        hot.namespace_policies.insert(
            reg.to_owned(),
            vec![(prefix.to_owned(), policy(ns_entries))],
        );
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, calls)
}

// ── A forge archive is refused before the forge is asked ──────────────────────

#[actix_web::test]
async fn an_unlisted_forge_archive_is_refused_with_no_upstream_call() {
    let (app, calls) = app_with_allowlist(
        "github",
        RegistryKind::Github,
        RegistryMode::Proxy,
        &[entry("rust-lang/rust", None)],
    )
    .await;
    let (status, body) = get(&app, "/proxy/github/acme/evil/zipball/v1.0.0", USER_TOKEN).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (status, body) = get(
        &app,
        "/proxy/github/rust-lang/rust/zipball/v1.80.0",
        USER_TOKEN,
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

// ── The console's package page asks the upstream nothing for an unlisted name ─

#[actix_web::test]
async fn the_console_detail_of_an_unlisted_name_fetches_nothing() {
    let (app, calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    let _ = get(&app, "/api/v1/explore/packages/npm-p/lodahs", USER_TOKEN).await;
    let _ = get(
        &app,
        "/api/v1/explore/packages/npm-p/lodahs/1.0.0",
        USER_TOKEN,
    )
    .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the console asked the upstream about a name the allowlist refuses"
    );
}

// ── The access simulator names the refusal ────────────────────────────────────

#[actix_web::test]
async fn the_access_simulator_names_the_allowlist() {
    let (app, calls) = app_with_allowlist(
        "npm-p",
        RegistryKind::Npm,
        RegistryMode::Proxy,
        &[entry("react", None)],
    )
    .await;
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/access-check")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({
                "registry": "npm-p",
                "package_name": "left-pad",
                "version": "1.3.0",
                "resource_type": "releases:read",
                "role": "user",
            }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    assert_eq!(body["decision"], "deny", "{body}");
    assert_eq!(body["rule_matched"], "package_allowlist", "{body}");
    let _ = calls;
}

// ── T13: an alias is judged under the name it resolves to ─────────────────────

#[actix_web::test]
async fn allowlist_judges_the_canonical_name_of_an_alias() {
    // The fixture resolves role id 4567 to `roles/geerlingguy.docker`.
    let (listed, _) = app_with_allowlist(
        "galaxy",
        RegistryKind::Galaxy,
        RegistryMode::Proxy,
        &[entry("roles/geerlingguy.docker", None)],
    )
    .await;
    let (status, body) = get(
        &listed,
        "/proxy/galaxy/galaxy/api/v1/roles/4567/versions/",
        USER_TOKEN,
    )
    .await;
    assert_eq!(status, 200, "listed under its canonical name: {body}");

    let (unlisted, _) = app_with_allowlist(
        "galaxy",
        RegistryKind::Galaxy,
        RegistryMode::Proxy,
        &[entry("roles/other.role", None)],
    )
    .await;
    let (status, body) = get(
        &unlisted,
        "/proxy/galaxy/galaxy/api/v1/roles/4567/versions/",
        USER_TOKEN,
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body.contains("roles/geerlingguy.docker"),
        "judged under the canonical name: {body}"
    );
}

// ── T10: the namespace is selected on the canonical spelling ──────────────────

#[actix_web::test]
async fn allowlist_namespace_override_alternate_spelling() {
    // The prefix as `build_namespace_policies` stores it: canonical, so Go's
    // `!`-escaped form of `github.com/Acme`.
    let (app, _calls) = app_with_allowlist_and_namespace(
        "go-p",
        RegistryKind::Goproxy,
        &[entry("example.com/other", None)],
        ("github.com/!acme", &[entry("github.com/Acme/*", None)]),
    )
    .await;
    // The go client escapes upper case; a hand-written request may not. Both
    // spellings select the namespace and its list.
    for module in ["github.com/!acme/tool", "github.com/Acme/tool"] {
        let (status, body) = get(&app, &format!("/proxy/go-p/{module}/@v/list"), USER_TOKEN).await;
        assert_ne!(
            status, 403,
            "{module} must be judged by the namespace's list: {body}"
        );
    }
    let (status, _) = get(&app, "/proxy/go-p/github.com/evil/tool/@v/list", USER_TOKEN).await;
    assert_eq!(status, 403);
}
