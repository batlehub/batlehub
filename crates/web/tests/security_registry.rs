//! RFC 0018 phase 1 end to end, in process: a registry behind `[security]`
//! refuses an unscanned version, a worker scans it, the next request serves
//! it; a young version is held by age with a clock; a finding at the
//! threshold denies in `block` mode and serves in `warn` mode; a version past
//! `mature_age_secs` is served while its scan is pending; and a registry
//! without the section is untouched.
//!
//! The scanner is a fake keyed by version, so the test decides what "OSV"
//! says. Everything else — the queue, the verdict store, the gate, the worker
//! loop — is the real thing over in-memory stores.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use actix_web::test::call_service;
use async_trait::async_trait;
use batlehub_adapters::in_memory::{
    InMemoryScanQueue, InMemoryVerdictRepository, InMemoryWorkerRegistry,
};
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{
        Finding, FindingKind, PackageId, PackageMetadata, ReasonCode, RegistryKind, ScanTrigger,
        SecurityMode, SecurityPolicy, Severity, VerdictState,
    },
    ports::{
        ArtifactScanner, DocumentKind, FetchedArtifact, RegistryClient, ScanInput, ScanQueue,
        ScannerError, VerdictRepository, VersionDocument,
    },
    rules::VerdictGateRule,
    services::{RegistryPolicy, ScanWorker, VerdictService, WorkerConfig},
};
use chrono::Utc;

const REG: &str = "npm-sec";

/// A registry whose versions are dated as the test says.
struct DatedRegistry {
    inner: Arc<FixedRegistry>,
    /// version → age in seconds (`None` = undated).
    ages: HashMap<&'static str, Option<i64>>,
}

#[async_trait]
impl RegistryClient for DatedRegistry {
    fn registry_type(&self) -> &str {
        "npm"
    }
    async fn resolve_metadata(
        &self,
        pkg: &PackageId,
    ) -> Result<PackageMetadata, batlehub_core::error::CoreError> {
        let mut m = self.inner.resolve_metadata(pkg).await?;
        m.published_at = match self.ages.get(pkg.version.as_str()) {
            Some(Some(secs)) => Some(Utc::now() - chrono::Duration::seconds(*secs)),
            Some(None) => None,
            None => m.published_at,
        };
        Ok(m)
    }
    async fn fetch_artifact(
        &self,
        pkg: &PackageId,
    ) -> Result<FetchedArtifact, batlehub_core::error::CoreError> {
        self.inner.fetch_artifact(pkg).await
    }
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, batlehub_core::error::CoreError> {
        self.inner.fetch_version_document(package, kind).await
    }
}

/// "OSV", as the test scripts it: findings per version, and a switch to
/// make it fail.
struct FakeOsv {
    findings: Mutex<HashMap<String, Vec<Finding>>>,
    fail: Mutex<bool>,
    scans: AtomicUsize,
    /// Scans that arrived with the artifact bytes (phase 3): the worker
    /// fetched them because this scanner says it reads them.
    with_bytes: AtomicUsize,
}

impl FakeOsv {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            findings: Mutex::new(HashMap::new()),
            fail: Mutex::new(false),
            scans: AtomicUsize::new(0),
            with_bytes: AtomicUsize::new(0),
        })
    }
    fn vuln(&self, version: &str, severity: Severity) {
        self.findings.lock().unwrap().insert(
            version.to_owned(),
            vec![Finding::new(
                "osv",
                FindingKind::Vulnerability,
                ReasonCode::Vulnerability,
                severity,
                "GHSA-test",
            )
            .with_reference("GHSA-test")],
        );
    }
}

#[async_trait]
impl ArtifactScanner for FakeOsv {
    fn name(&self) -> &str {
        "osv"
    }
    fn supports(&self, _: RegistryKind) -> bool {
        true
    }
    fn needs_artifact(&self) -> bool {
        true
    }
    async fn scan(&self, input: &ScanInput) -> Result<Vec<Finding>, ScannerError> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        if input
            .artifact
            .as_ref()
            .is_some_and(|b| b.starts_with(b"artifact:npm:"))
        {
            self.with_bytes.fetch_add(1, Ordering::SeqCst);
        }
        if *self.fail.lock().unwrap() {
            return Err(ScannerError::Upstream("osv down".into()));
        }
        Ok(self
            .findings
            .lock()
            .unwrap()
            .get(&input.package.id.version)
            .cloned()
            .unwrap_or_default())
    }
}

struct Lab {
    osv: Arc<FakeOsv>,
    queue: Arc<InMemoryScanQueue>,
    verdicts: Arc<InMemoryVerdictRepository>,
    worker: Arc<ScanWorker>,
}

fn policy(mode: SecurityMode) -> SecurityPolicy {
    let mut p = SecurityPolicy::defaults_for(REG);
    p.mode = mode;
    p.min_age = Duration::from_secs(3600);
    p.mature_age = Duration::from_secs(86_400);
    p
}

/// The app: `REG` behind `[security]` with the policy given, over in-memory
/// verdict and queue stores, and a worker over the same stores.
async fn lab(
    mode: SecurityMode,
    ages: HashMap<&'static str, Option<i64>>,
) -> (impl TestService, Lab) {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let osv = FakeOsv::new();
    let queue = InMemoryScanQueue::new();
    let verdicts = InMemoryVerdictRepository::new();
    let service = Arc::new(VerdictService::new(
        Arc::clone(&verdicts) as Arc<dyn VerdictRepository>,
        Arc::clone(&queue) as Arc<dyn ScanQueue>,
    ));
    let sec = policy(mode);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(DatedRegistry {
                inner: FixedRegistry::new("npm"),
                ages,
            }) as Arc<dyn RegistryClient>,
        );
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![Box::new(VerdictGateRule::new(
                    Arc::clone(&service),
                    sec.clone(),
                    None,
                ))],
            }),
        );
        hot.security.insert(REG.to_owned(), sec);
        hot.verdicts = Some(Arc::clone(&verdicts) as Arc<dyn VerdictRepository>);
        hot.scan_queue = Some(Arc::clone(&queue) as Arc<dyn ScanQueue>);
        // Anonymous may read the tarball here — the public-mirror case of
        // §4.2 — so it is the one caller with the bytes and without
        // `quarantine:read`: a held version must answer it a plain 404.
        let mut perms = rbac_policy_perms();
        perms
            .roles
            .entry(batlehub_core::entities::Role::Anonymous)
            .or_default()
            .push("source:read".to_owned());
        // `gates:exempt` is held by nobody by default (RFC 0015 §10); the
        // rescan endpoint reads it, so the admin gets it here and the user
        // does not — which is the pair the rescan test measures.
        use batlehub_core::entities::{Action, Role, SubjectMatcher};
        let mut grants = fixture_grants(REG, "npm", &RegistryMode::Proxy, &perms);
        let node = grants.registry.grants.take().unwrap_or_default();
        grants.registry.grants =
            Some(node.grant(SubjectMatcher::Role(Role::Admin), [Action::GatesExempt]));
        hot.grants.insert(REG.to_owned(), Arc::new(grants));
    }
    let mut scanners: HashMap<String, Arc<dyn ArtifactScanner>> = HashMap::new();
    scanners.insert("osv".into(), Arc::clone(&osv) as Arc<dyn ArtifactScanner>);
    let worker = Arc::new(ScanWorker {
        config: WorkerConfig {
            worker_id: "test-worker".into(),
            max_concurrent: 4,
            registries: vec![],
            job_timeout: Duration::from_secs(5),
            max_attempts: 2,
            idle_poll: Duration::from_millis(10),
        },
        queue: Arc::clone(&queue) as Arc<dyn ScanQueue>,
        verdicts: service,
        workers: Some(InMemoryWorkerRegistry::new()),
        sboms: None,
        hot: parts.proxy_svc.hot.clone(),
        scanners,
        storage: Some(Arc::clone(&parts.proxy_svc.storage)),
        max_artifact_bytes: 64 * 1024 * 1024,
    });
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (
        app,
        Lab {
            osv,
            queue,
            verdicts,
            worker,
        },
    )
}

fn tarball(version: &str) -> String {
    format!("/proxy/{REG}/pkg/{version}/tarball")
}

async fn status<S: TestService>(app: &S, uri: &str) -> (u16, String) {
    let resp = call_service(app, admin_get(uri)).await;
    let status = resp.status().as_u16();
    let body = actix_web::test::read_body(resp).await;
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn old() -> HashMap<&'static str, Option<i64>> {
    // Every fixture version is two hours old: past min_age, under mature_age.
    HashMap::from([
        ("1.0.0", Some(7200)),
        ("1.1.0", Some(7200)),
        ("2.0.0-beta.1", Some(7200)),
    ])
}

// ── the invariant: never streamed without a served verdict ───────────────────

#[actix_web::test]
async fn first_request_is_held_pending_then_served_after_the_worker_scans() {
    let (app, lab) = lab(SecurityMode::Block, old()).await;

    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("SCAN_PENDING"), "{body}");
    assert!(body.contains("batlehub why"), "{body}");
    assert_eq!(lab.queue.open_jobs().await.len(), 1, "one FirstSeen job");
    assert_eq!(
        lab.queue.open_jobs().await[0].trigger,
        ScanTrigger::FirstSeen
    );

    // A second request while pending: still held, nothing re-queued.
    let (code, _) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403);
    assert_eq!(lab.queue.open_jobs().await.len(), 1);

    let report = lab.worker.run_once().await.unwrap();
    assert_eq!((report.leased, report.completed), (1, 1));
    assert_eq!(lab.osv.scans.load(Ordering::SeqCst), 1);
    // Phase 3: a scanner that reads bytes was handed the artifact — fetched
    // from upstream by the worker, since the refused request cached nothing.
    assert_eq!(lab.osv.with_bytes.load(Ordering::SeqCst), 1);
    assert!(lab.queue.open_jobs().await.is_empty(), "the job closed");

    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 200, "{body}");
    let stored = lab
        .verdicts
        .get(&PackageId::new(REG, "pkg", "1.1.0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, VerdictState::Allowed);
    assert_eq!(stored.scanners_done, vec!["osv"]);
}

#[actix_web::test]
async fn a_young_version_is_held_by_age_even_after_a_clean_scan() {
    let (app, lab) = lab(SecurityMode::Block, HashMap::from([("1.1.0", Some(60))])).await;
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403);
    assert!(body.contains("MIN_AGE_NOT_MET"), "{body}");
    assert!(
        body.contains("available "),
        "a time-bound hold names when it lifts: {body}"
    );
    lab.worker.run_once().await.unwrap();
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "clean but young: {body}");
    assert!(
        body.contains("MIN_AGE_NOT_MET") && !body.contains("SCAN_PENDING"),
        "{body}"
    );
}

#[actix_web::test]
async fn a_finding_at_the_threshold_denies_in_block_mode() {
    let (app, lab) = lab(SecurityMode::Block, old()).await;
    lab.osv.vuln("1.1.0", Severity::High);
    status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("is denied (VULNERABILITY)"), "{body}");
    // Every file of the release shares the verdict.
    let (code, _) = status(&app, &format!("/proxy/{REG}/pkg/1.1.0")).await;
    assert_eq!(code, 403);
    // A finding under the threshold is recorded and does not deny.
    lab.osv.vuln("1.0.0", Severity::Low);
    status(&app, &tarball("1.0.0")).await;
    lab.worker.run_once().await.unwrap();
    let (code, _) = status(&app, &tarball("1.0.0")).await;
    assert_eq!(code, 200);
}

#[actix_web::test]
async fn the_same_finding_serves_in_warn_mode() {
    let (app, lab) = lab(SecurityMode::Warn, old()).await;
    lab.osv.vuln("1.1.0", Severity::Critical);
    status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();
    let (code, _) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 200);
    let stored = lab
        .verdicts
        .get(&PackageId::new(REG, "pkg", "1.1.0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, VerdictState::Warned);
    assert_eq!(stored.reason_codes, vec![ReasonCode::Vulnerability]);
}

/// A version older than `mature_age_secs` is served `warned` while nobody
/// has looked at it, and the scan still runs behind the request.
#[actix_web::test]
async fn a_mature_version_is_served_unscanned_and_scanned_behind_the_request() {
    let (app, lab) = lab(
        SecurityMode::Block,
        HashMap::from([("1.1.0", Some(30 * 86_400))]),
    )
    .await;
    let (code, _) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 200);
    let stored = lab
        .verdicts
        .get(&PackageId::new(REG, "pkg", "1.1.0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, VerdictState::Warned);
    assert_eq!(stored.reason_codes, vec![ReasonCode::ScanPending]);
    assert_eq!(lab.queue.open_jobs().await.len(), 1);

    // The scan lands with a finding: served-unscanned ends, the finding denies.
    lab.osv.vuln("1.1.0", Severity::Critical);
    lab.worker.run_once().await.unwrap();
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "{body}");
}

/// A scanner that cannot answer is a hold, not a clean scan — and once its
/// attempts are spent the verdict says `SCANNER_ERROR` rather than pending
/// forever.
#[actix_web::test]
async fn a_failing_scanner_holds_and_exhausted_attempts_become_scanner_error() {
    let (app, lab) = lab(SecurityMode::Block, old()).await;
    *lab.osv.fail.lock().unwrap() = true;
    status(&app, &tarball("1.1.0")).await;

    let r = lab.worker.run_once().await.unwrap();
    assert_eq!(
        (r.leased, r.completed),
        (1, 1),
        "an error is an answer the worker records"
    );
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("SCANNER_ERROR"), "{body}");
    // The scanner that errored is not counted as done: the hold is both.
    assert!(body.contains("SCAN_PENDING"), "{body}");

    // OSV recovers, a rescan is queued by the next read? No — phase 4. The
    // next job for this version comes from the queue being asked again.
    *lab.osv.fail.lock().unwrap() = false;
    lab.queue
        .enqueue(
            &PackageId::new(REG, "pkg", "1.1.0"),
            None,
            ScanTrigger::Rescan,
        )
        .await
        .unwrap();
    lab.worker.run_once().await.unwrap();
    let (code, _) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 200);
}

/// The undated case: held open-ended by default, with no clock.
#[actix_web::test]
async fn an_undated_version_is_held_open_ended() {
    let (app, lab) = lab(SecurityMode::Block, HashMap::from([("1.1.0", None)])).await;
    lab.worker.run_once().await.unwrap();
    status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("TIMESTAMP_MISSING"), "{body}");
    assert!(!body.contains("available "), "no clock to wait on: {body}");
}

/// A registry without `[security]` is exactly as it was.
#[actix_web::test]
async fn a_registry_without_the_section_is_untouched() {
    let app = proxy_registry_app("npm-plain", "npm").await;
    let resp = call_service(&app, admin_get("/proxy/npm-plain/pkg/1.1.0/tarball")).await;
    assert_eq!(resp.status(), 200);
}

// ── phase 2: the verdict on the wire ─────────────────────────────────────────

use actix_web::test::TestRequest;
use batlehub_web::handlers::security::{
    HEADER_AVAILABLE_AT, HEADER_DETAILS, HEADER_REASON, HEADER_VERDICT,
};

async fn get_as<S: TestService>(
    app: &S,
    uri: &str,
    token: Option<&str>,
) -> actix_web::dev::ServiceResponse {
    let mut req = TestRequest::get().uri(uri);
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    call_service(app, req.to_request()).await
}

fn header<'a>(resp: &'a actix_web::dev::ServiceResponse, name: &str) -> Option<&'a str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

/// A hold answers in the registry's own error shape with the verdict in
/// headers for a caller with `quarantine:read`, and as the registry's own
/// not-found — nothing in the headers — for one without (§4.2).
#[actix_web::test]
async fn a_hold_is_npms_error_shape_with_headers_and_a_404_for_anonymous() {
    let (app, _lab) = lab(SecurityMode::Block, old()).await;

    let resp = get_as(&app, &tarball("1.1.0"), Some(USER_TOKEN)).await;
    assert_eq!(resp.status(), 403);
    assert!(header(&resp, "content-type")
        .unwrap()
        .starts_with("application/json"));
    assert_eq!(header(&resp, HEADER_VERDICT), Some("quarantined"));
    assert_eq!(header(&resp, HEADER_REASON), Some("SCAN_PENDING"));
    assert!(header(&resp, HEADER_DETAILS)
        .unwrap()
        .contains("batlehub why"));
    assert!(
        header(&resp, "retry-after").is_none(),
        "a pending scan names no clock"
    );
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap().contains("SCAN_PENDING"),
        "{body}"
    );

    let resp = get_as(&app, &tarball("1.1.0"), None).await;
    let code = resp.status();
    assert!(header(&resp, HEADER_VERDICT).is_none(), "nothing leaks");
    let body = actix_web::test::read_body(resp).await;
    let body = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(code, 404, "anonymous holds neither permission: {body}");
    assert!(body.contains("not found"), "{body}");
}

#[actix_web::test]
async fn a_time_bound_hold_carries_retry_after_and_available_at() {
    let (app, lab) = lab(SecurityMode::Block, HashMap::from([("1.1.0", Some(60))])).await;
    lab.worker.run_once().await.ok();
    let _ = status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();

    let resp = get_as(&app, &tarball("1.1.0"), Some(ADMIN_TOKEN)).await;
    assert_eq!(resp.status(), 403);
    assert_eq!(header(&resp, HEADER_REASON), Some("MIN_AGE_NOT_MET"));
    assert!(header(&resp, HEADER_AVAILABLE_AT).is_some());
    let retry: u64 = header(&resp, "retry-after").unwrap().parse().unwrap();
    assert!((3400..=3600).contains(&retry), "{retry}");
}

/// The same finding that denies in `block` serves in `warn` — with the
/// verdict on the response, so a pipeline can log what it pulled under.
#[actix_web::test]
async fn a_warned_artifact_is_served_with_the_verdict_headers() {
    let (app, lab) = lab(SecurityMode::Warn, old()).await;
    lab.osv.vuln("1.1.0", Severity::Critical);
    let _ = status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();

    let resp = get_as(&app, &tarball("1.1.0"), Some(USER_TOKEN)).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, HEADER_VERDICT), Some("warned"));
    assert_eq!(header(&resp, HEADER_REASON), Some("VULNERABILITY"));
    assert!(header(&resp, "retry-after").is_none());
}

/// A held version leaves the packument by the same mechanism as a block
/// (§4.2 *Listings*), and comes back when the verdict serves.
#[actix_web::test]
async fn a_held_version_is_hidden_from_the_listing_until_it_serves() {
    let (app, lab) = lab(SecurityMode::Block, old()).await;
    let packument = format!("/proxy/{REG}/pkg");

    let before = get_json(&app, &packument).await;
    assert!(before["versions"].get("1.1.0").is_some(), "{before}");

    let _ = status(&app, &tarball("1.1.0")).await;
    let held = get_json(&app, &packument).await;
    assert!(
        held["versions"].get("1.1.0").is_none(),
        "SCAN_PENDING hides the version: {held}"
    );
    assert!(held["versions"].get("1.0.0").is_some());
    assert_ne!(held["dist-tags"]["latest"], "1.1.0", "latest is repaired");

    lab.worker.run_once().await.unwrap();
    let after = get_json(&app, &packument).await;
    assert!(after["versions"].get("1.1.0").is_some(), "{after}");
}

/// `GET /api/v1/verdicts/…`: the verdict for `quarantine:read`, findings
/// for `findings:read`, `404` for nobody else and for a version never seen.
#[actix_web::test]
async fn the_verdict_endpoint_answers_by_permission() {
    let (app, lab) = lab(SecurityMode::Block, old()).await;
    lab.osv.vuln("1.1.0", Severity::Critical);
    let uri = format!("/api/v1/verdicts/{REG}/pkg/1.1.0");

    let resp = get_as(&app, &uri, Some(ADMIN_TOKEN)).await;
    assert_eq!(resp.status(), 404, "never seen");

    let _ = status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();

    let resp = get_as(&app, &uri, Some(ADMIN_TOKEN)).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(body["state"], "denied", "{body}");
    assert_eq!(body["findings_withheld"], false);
    assert_eq!(body["findings"][0]["summary"], "GHSA-test");

    let resp = get_as(&app, &uri, Some(USER_TOKEN)).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(body["findings_withheld"], true);
    assert!(body["findings"].as_array().unwrap().is_empty());

    let resp = get_as(&app, &uri, None).await;
    assert_eq!(resp.status(), 404, "anonymous cannot enumerate holds");
}

#[actix_web::test]
async fn a_rescan_needs_gates_exempt_and_queues_one_job() {
    let (app, lab) = lab(SecurityMode::Block, old()).await;
    let _ = status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();
    assert!(lab.queue.open_jobs().await.is_empty());
    let uri = format!("/api/v1/verdicts/{REG}/pkg/1.1.0/rescan");

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&uri)
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&uri)
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    let code = resp.status();
    let body = actix_web::test::read_body(resp).await;
    let body = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(code, 202, "{body}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["queued"], true);
    let jobs = lab.queue.open_jobs().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].trigger, ScanTrigger::Rescan);
}

// ── RFC 0002 (recast): a pushed flag on a `[security]` registry ──────────────

/// A `hard_block` pushed for a version this instance already judged denies it
/// at once — no worker in the loop — and the rescan that follows re-derives
/// the same denial from the `flags` scanner; a revoke lifts it on the next
/// rescan. The push goes through `FlagService` directly: the endpoint's
/// signature check is measured in `flags.rs`, and what is measured here is
/// what a flag does to a verdict.
#[actix_web::test]
async fn a_pushed_hard_block_denies_a_judged_version_and_a_revoke_lifts_it() {
    use batlehub_adapters::in_memory::InMemoryAdvisoryRepository;
    use batlehub_core::entities::{FlagEffect, FlagKind, FlagPush};
    use batlehub_core::ports::AdvisoryRepository;
    use batlehub_core::services::{FlagService, FlagSourceLimits, FlagsScanner};

    let (app, lab) = lab(SecurityMode::Block, old()).await;
    let advisories: Arc<dyn AdvisoryRepository> = Arc::new(InMemoryAdvisoryRepository::new());
    {
        let mut hot = lab.worker.hot.write().await;
        hot.internal_scanners.insert(
            REG.to_owned(),
            vec![Arc::new(FlagsScanner {
                repo: Arc::clone(&advisories),
            }) as Arc<dyn ArtifactScanner>],
        );
    }
    let flags = FlagService::new(Arc::clone(&advisories), lab.worker.hot.clone());
    let soc = FlagSourceLimits {
        name: "soc".into(),
        max_effect: FlagEffect::HardBlock,
        registries: vec![],
        max_flags_per_minute: 0,
    };
    let push = |effect: FlagEffect| FlagPush {
        external_id: "CASE-42".into(),
        registry: REG.into(),
        package_name: "pkg".into(),
        version: Some("1.1.0".into()),
        version_range: None,
        kind: FlagKind::Malware,
        effect,
        severity: None,
        summary: "credential stealer".into(),
        url: None,
        expires_at: None,
    };

    // Judged clean and served.
    status(&app, &tarball("1.1.0")).await;
    lab.worker.run_once().await.unwrap();
    let (code, _) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 200);

    // The SOC's word: denied before any worker runs, and a rescan queued.
    let out = flags
        .push(&soc, vec![push(FlagEffect::HardBlock)], Utc::now())
        .await
        .unwrap();
    assert_eq!(out.accepted, 1, "{out:?}");
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("SOC_VERDICT"), "{body}");
    assert_eq!(lab.queue.open_jobs().await.len(), 1, "one Webhook rescan");
    assert_eq!(lab.queue.open_jobs().await[0].trigger, ScanTrigger::Webhook);

    // The rescan agrees: the `flags` scanner re-emits the finding.
    lab.worker.run_once().await.unwrap();
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 403, "{body}");
    let stored = lab
        .verdicts
        .get(&PackageId::new(REG, "pkg", "1.1.0"))
        .await
        .unwrap()
        .unwrap();
    let from_flags: Vec<_> = stored
        .findings
        .iter()
        .filter(|f| f.scanner == "flags")
        .collect();
    assert_eq!(
        from_flags.len(),
        1,
        "no duplicate across rescans: {stored:?}"
    );
    assert_eq!(from_flags[0].reference.as_deref(), Some("soc:CASE-42"));

    // Revoked: the rescan drops the finding and the version is served again.
    assert!(flags.revoke("soc", "CASE-42", Utc::now()).await.unwrap());
    assert_eq!(lab.queue.open_jobs().await.len(), 1);
    lab.worker.run_once().await.unwrap();
    let (code, body) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 200, "{body}");

    // A `warn` flag on every version is recorded, and the version stays served.
    let mut warn = push(FlagEffect::Warn);
    warn.external_id = "NOTE-1".into();
    warn.version = None;
    warn.version_range = Some("*".into());
    flags.push(&soc, vec![warn], Utc::now()).await.unwrap();
    lab.worker.run_once().await.unwrap();
    let (code, _) = status(&app, &tarball("1.1.0")).await;
    assert_eq!(code, 200);
    let stored = lab
        .verdicts
        .get(&PackageId::new(REG, "pkg", "1.1.0"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        stored
            .findings
            .iter()
            .any(|f| f.reference.as_deref() == Some("soc:NOTE-1")),
        "{stored:?}"
    );
}
