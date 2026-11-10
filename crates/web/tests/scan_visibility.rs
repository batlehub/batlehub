//! RFC 0042 phase 1 — `scan_status` on the explore list, the explore detail
//! and the admin package list: who sees it, what it costs, and what it never
//! does (§4.2, §7.1 T1/T2/T5/T8b/T15).

mod common;
use common::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use actix_web::test::{call_service, read_body_json, TestRequest};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryScanQueue, InMemoryStorageBackend,
    InMemoryVerdictRepository, NoopArtifactMetaRepository, NullUserTokenRepository,
};
use batlehub_core::entities::{
    AccessEvent, Action, GrantMap, PackageId, Role, ScanJob, ScanTrigger, SecurityPolicy,
    SubjectMatcher, Verdict, VerdictState, EXHAUSTED_PREFIX,
};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{
    CacheStore, PackageRepository, QueuedCount, RegistryClient, ScanQueue, StorageBackend,
    UserTokenRepository, VerdictRepository,
};
use batlehub_core::services::{
    new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy,
};
use batlehub_web::{new_access_lock, RegistryModeMap};

/// Two registries behind `[security]`, one without.
const SEC: &str = "npm-sec";
const SEC2: &str = "npm-sec2";
const PLAIN: &str = "npm";

// ── stores that count what a request did ─────────────────────────────────────

/// The in-memory stores, counting the batch reads and every write, and able
/// to fail the batch.
#[derive(Default)]
struct Counts {
    jobs_for: AtomicUsize,
    get_many: AtomicUsize,
    writes: AtomicUsize,
    fail: AtomicBool,
}

struct Queue(Arc<InMemoryScanQueue>, Arc<Counts>);
struct Verdicts(Arc<InMemoryVerdictRepository>, Arc<Counts>);

#[async_trait]
impl ScanQueue for Queue {
    async fn enqueue(
        &self,
        p: &PackageId,
        at: Option<DateTime<Utc>>,
        t: ScanTrigger,
    ) -> Result<bool, CoreError> {
        self.1.writes.fetch_add(1, Ordering::SeqCst);
        self.0.enqueue(p, at, t).await
    }
    async fn lease(
        &self,
        w: &str,
        r: &[String],
        n: u32,
        s: u64,
        m: u32,
    ) -> Result<Vec<ScanJob>, CoreError> {
        self.0.lease(w, r, n, s, m).await
    }
    async fn heartbeat(&self, id: uuid::Uuid, s: u64) -> Result<(), CoreError> {
        self.0.heartbeat(id, s).await
    }
    async fn complete(&self, id: uuid::Uuid, w: Option<&str>) -> Result<(), CoreError> {
        self.0.complete(id, w).await
    }
    async fn complete_exhausted(&self, id: uuid::Uuid, e: &str) -> Result<(), CoreError> {
        self.0.complete_exhausted(id, e).await
    }
    async fn fail(&self, id: uuid::Uuid, w: &str, e: &str) -> Result<(), CoreError> {
        self.0.fail(id, w, e).await
    }
    async fn exhausted(&self, m: u32, n: u32) -> Result<Vec<ScanJob>, CoreError> {
        self.0.exhausted(m, n).await
    }
    async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError> {
        self.0.queued().await
    }
    async fn try_lead(&self, k: i64) -> Result<bool, CoreError> {
        self.0.try_lead(k).await
    }
    async fn jobs_for(
        &self,
        coords: &[PackageId],
    ) -> Result<Vec<(Option<ScanJob>, Option<ScanJob>)>, CoreError> {
        self.1.jobs_for.fetch_add(1, Ordering::SeqCst);
        if self.1.fail.load(Ordering::SeqCst) {
            return Err(CoreError::Other(anyhow::anyhow!("database is down")));
        }
        self.0.jobs_for(coords).await
    }
    async fn list_jobs(
        &self,
        r: &[String],
        f: batlehub_core::entities::ScanJobFilter,
        l: u64,
        o: u64,
    ) -> Result<(Vec<ScanJob>, u64), CoreError> {
        self.0.list_jobs(r, f, l, o).await
    }
}

#[async_trait]
impl VerdictRepository for Verdicts {
    async fn upsert(&self, v: &Verdict) -> Result<(), CoreError> {
        self.1.writes.fetch_add(1, Ordering::SeqCst);
        self.0.upsert(v).await
    }
    async fn get(&self, p: &PackageId) -> Result<Option<Verdict>, CoreError> {
        self.0.get(p).await
    }
    async fn get_many(&self, c: &[PackageId]) -> Result<Vec<Option<Verdict>>, CoreError> {
        self.1.get_many.fetch_add(1, Ordering::SeqCst);
        self.0.get_many(c).await
    }
    async fn list_for_package(&self, r: &str, p: &str) -> Result<Vec<Verdict>, CoreError> {
        self.0.list_for_package(r, p).await
    }
    async fn list_by_state(
        &self,
        r: &str,
        s: VerdictState,
        l: u64,
    ) -> Result<Vec<Verdict>, CoreError> {
        self.0.list_by_state(r, s, l).await
    }
    async fn list_due_for_rescan(
        &self,
        r: &str,
        b: DateTime<Utc>,
        l: u64,
    ) -> Result<Vec<PackageId>, CoreError> {
        self.0.list_due_for_rescan(r, b, l).await
    }
    async fn list_findings(
        &self,
        f: &batlehub_core::entities::FindingsFilter,
        l: u64,
        o: u64,
    ) -> Result<(Vec<batlehub_core::entities::FindingRow>, u64), CoreError> {
        self.0.list_findings(f, l, o).await
    }
}

// ── the app ──────────────────────────────────────────────────────────────────

struct Lab {
    repo: Arc<InMemoryRepo>,
    queue: Arc<InMemoryScanQueue>,
    verdicts: Arc<InMemoryVerdictRepository>,
    counts: Arc<Counts>,
}

impl Lab {
    async fn seed(&self, registry: &str, name: &str, version: &str) {
        self.repo
            .record_access(AccessEvent::allowed_download(
                PackageId::new(registry, name, version),
                Some("user-1".to_owned()),
                Role::User,
            ))
            .await
            .unwrap();
    }

    async fn verdict(&self, registry: &str, name: &str, version: &str, state: VerdictState) {
        self.verdicts
            .upsert(&Verdict {
                package: PackageId::new(registry, name, version),
                state,
                reason_codes: vec![],
                findings: vec![],
                policy_ref: format!("{registry}/default"),
                available_at: None,
                evaluated_at: Utc::now(),
                last_scanned_at: Some(Utc::now()),
                scanners_done: vec!["osv".into()],
            })
            .await
            .unwrap();
    }

    fn reset(&self) {
        self.counts.jobs_for.store(0, Ordering::SeqCst);
        self.counts.get_many.store(0, Ordering::SeqCst);
        self.counts.writes.store(0, Ordering::SeqCst);
    }
}

/// Explore open to every role on the three registries; `SEC` and `SEC2`
/// behind `[security]`. `shadow` puts an active dry run on `SEC`'s node.
async fn app(
    shadow: bool,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Lab,
) {
    app_with(shadow, false).await
}

/// `delegate`: `role:user` also holds `system:read` at the instance tier,
/// and `SEC2`'s node grants `quarantine:read` to administrators only — the
/// T16 caller, who may open the Scans page but not see `SEC2`'s holds.
async fn app_with(
    shadow: bool,
    delegate: bool,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Lab,
) {
    app_opts(Opts {
        shadow,
        delegate,
        ..Opts::default()
    })
    .await
}

#[derive(Default, Clone, Copy)]
struct Opts {
    shadow: bool,
    delegate: bool,
    /// `role:user` holds `quarantine:read` and `findings:read` on `SEC`'s
    /// registry node, and `SEC` seals the namespace `@sealed` — the T3/T3c
    /// registry-scoped findings reader.
    sec_findings_reader: bool,
}

async fn app_opts(
    Opts {
        shadow,
        delegate,
        sec_findings_reader,
    }: Opts,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Lab,
) {
    let repo = InMemoryRepo::new();
    let repo_dyn: Arc<dyn PackageRepository> = repo.clone();
    let storage: Arc<dyn StorageBackend> = InMemoryStorageBackend::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let names = [SEC, SEC2, PLAIN];
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
    let grants: HashMap<String, Arc<batlehub_core::entities::RegistryGrants>> = names
        .iter()
        .map(|n| {
            let mut g = fixture_grants_with_explore(
                n,
                "npm",
                &batlehub_config::schema::RegistryMode::Proxy,
                &rbac_policy_perms(),
                true,
            );
            if shadow && *n == SEC {
                g.registry.dry_run = Some(batlehub_core::entities::DryRun {
                    until: Utc::now().date_naive() + chrono::Days::new(30),
                });
            }
            // `gates:exempt` goes to nobody by default (RFC 0015 §10): the
            // admin holds it on `SEC` only, so [Rescan] has both sides.
            if *n == SEC {
                let mut node = g.registry.grants.take().unwrap_or_default();
                node = node.grant(SubjectMatcher::Role(Role::Admin), [Action::GatesExempt]);
                if sec_findings_reader {
                    node = node.grant(
                        SubjectMatcher::Role(Role::User),
                        [Action::QuarantineRead, Action::FindingsRead],
                    );
                    g.namespaces.push((
                        "@sealed".to_owned(),
                        batlehub_core::entities::Node::new(
                            batlehub_core::entities::Tier::Namespace,
                            "namespace:@sealed",
                            Some(GrantMap::sealed()),
                        ),
                    ));
                }
                g.registry.grants = Some(node);
            }
            if delegate && *n == SEC2 {
                g.registry.grants = Some(GrantMap::new().grant(
                    SubjectMatcher::Role(Role::Admin),
                    [Action::QuarantineRead, Action::FindingsRead],
                ));
            }
            (n.to_string(), Arc::new(g))
        })
        .collect();
    let counts = Arc::new(Counts::default());
    let queue = InMemoryScanQueue::new();
    let verdicts = InMemoryVerdictRepository::new();
    let hot = new_hot_lock(HotConfig {
        grants,
        instance: Some(Arc::new(
            batlehub_core::services::authz::translate::instance_node(
                delegate
                    .then(|| {
                        GrantMap::new()
                            .grant(SubjectMatcher::Role(Role::User), [Action::SystemRead])
                    })
                    .as_ref(),
            ),
        )),
        registries,
        policies,
        security: [SEC, SEC2]
            .iter()
            .map(|n| (n.to_string(), SecurityPolicy::defaults_for(n)))
            .collect(),
        verdicts: Some(Arc::new(Verdicts(verdicts.clone(), counts.clone()))),
        scan_queue: Some(Arc::new(Queue(queue.clone(), counts.clone()))),
        versions_per_page: batlehub_core::services::hot_config::DEFAULT_VERSIONS_PER_PAGE,
        packages_per_page: batlehub_core::services::hot_config::DEFAULT_PACKAGES_PER_PAGE,
        ..Default::default()
    });
    let local_svc = make_local_svc(hot.clone(), storage.clone());
    let proxy_svc = Arc::new(ProxyService {
        hot: hot.clone(),
        storage,
        cache,
        repo: repo_dyn.clone(),
        artifact_meta: NoopArtifactMetaRepository::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn));
    let token_repo: Arc<dyn UserTokenRepository> = Arc::new(NullUserTokenRepository);
    let regs: std::collections::HashSet<String> = names.iter().map(ToString::to_string).collect();
    let access = new_access_lock(batlehub_web::AccessConfig {
        anonymous: regs.clone(),
        user: regs.clone(),
        admin: regs.clone(),
        groups: HashMap::new(),
        explore_anonymous: regs.clone(),
        explore_user: regs.clone(),
        explore_admin: regs.clone(),
    });
    let registry_map = registry_map_for(&[(SEC, "npm"), (SEC2, "npm"), (PLAIN, "npm")]);
    let app = finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access,
        registry_map,
        local_svc,
        RegistryModeMap::default(),
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        test_auth_providers(),
    )
    .await;
    (
        app,
        Lab {
            repo,
            queue,
            verdicts,
            counts,
        },
    )
}

async fn get<S>(app: &S, uri: &str, token: Option<&str>) -> Value
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let mut req = TestRequest::get().uri(uri);
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    let resp = call_service(app, req.to_request()).await;
    assert_eq!(resp.status(), 200, "{uri}");
    read_body_json(resp).await
}

fn row<'a>(body: &'a Value, registry: &str) -> &'a Value {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["registry"] == registry)
        .unwrap_or_else(|| panic!("no {registry} row in {body}"))
}

const LIST: &str = "/api/v1/explore/packages";

// ── the field, on each of the three responses ────────────────────────────────

#[actix_web::test]
async fn list_detail_and_admin_list_carry_scan_status() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "lodash", "1.0.0").await;
    lab.verdict(SEC, "lodash", "1.0.0", VerdictState::Warned)
        .await;

    let list = get(&app, LIST, Some(ADMIN_TOKEN)).await;
    assert_eq!(list["scan_status_unavailable"], false);
    let status = &row(&list, SEC)["scan_status"];
    assert_eq!(status["state"], "scanned", "{list}");
    assert_eq!(status["verdict"], "warned");
    assert!(status["detail"]["last_scanned_at"].is_string());

    let detail = get(
        &app,
        &format!("/api/v1/explore/packages/{SEC}/lodash?upstream=skip"),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(detail["versions"][0]["scan_status"]["state"], "scanned");
    assert_eq!(detail["selected"]["scan_status"]["verdict"], "warned");

    let admin = get(&app, "/api/v1/admin/packages", Some(ADMIN_TOKEN)).await;
    assert_eq!(admin["items"][0]["package_id"]["name"], "lodash");
    assert_eq!(admin["items"][0]["scan_status"]["state"], "scanned");
    assert_eq!(admin["scan_status_unavailable"], false);
}

#[actix_web::test]
async fn an_open_job_reads_queued_and_no_verdict_reads_not_scanned() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "a", "1.0.0").await;
    lab.seed(SEC2, "b", "1.0.0").await;
    lab.queue
        .enqueue(
            &PackageId::new(SEC, "a", "1.0.0"),
            None,
            ScanTrigger::FirstSeen,
        )
        .await
        .unwrap();
    let list = get(&app, LIST, Some(USER_TOKEN)).await;
    assert_eq!(row(&list, SEC)["scan_status"]["state"], "queued");
    assert_eq!(row(&list, SEC2)["scan_status"]["state"], "not_scanned");
}

// ── who sees what ────────────────────────────────────────────────────────────

/// T1: anonymous holds no `quarantine:read`, so a held version is not named.
#[actix_web::test]
async fn anonymous_list_has_no_scan_status_on_quarantined_registry() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "evil", "0.0.1").await;
    lab.verdict(SEC, "evil", "0.0.1", VerdictState::Quarantined)
        .await;
    let list = get(&app, LIST, None).await;
    assert!(row(&list, SEC).get("scan_status").is_none(), "{list}");
}

/// T1: an active shadow serves the per-version GET, never the list.
#[actix_web::test]
async fn anonymous_list_under_shadow_has_no_scan_status() {
    let (app, lab) = app(true).await;
    lab.seed(SEC, "evil", "0.0.1").await;
    lab.verdict(SEC, "evil", "0.0.1", VerdictState::Quarantined)
        .await;
    let list = get(&app, LIST, None).await;
    assert!(row(&list, SEC).get("scan_status").is_none(), "{list}");
}

/// T2: a hidden status and a registry without `[security]` are the same row.
#[actix_web::test]
async fn hidden_and_no_security_are_indistinguishable() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "same", "1.0.0").await;
    lab.seed(PLAIN, "same", "1.0.0").await;
    lab.verdict(SEC, "same", "1.0.0", VerdictState::Denied)
        .await;
    let list = get(&app, LIST, None).await;
    let strip = |v: &Value| {
        let mut v = v.clone();
        let o = v.as_object_mut().unwrap();
        o.remove("registry");
        o.remove("last_accessed"); // two seeds, two instants
        v.to_string()
    };
    assert_eq!(strip(row(&list, SEC)), strip(row(&list, PLAIN)));
}

/// T5: `quarantine:read` without `findings:read` gets the state, never the
/// detail.
#[actix_web::test]
async fn scan_detail_withheld_without_findings_read() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "lodash", "1.0.0").await;
    lab.verdict(SEC, "lodash", "1.0.0", VerdictState::Allowed)
        .await;
    let list = get(&app, LIST, Some(USER_TOKEN)).await;
    let status = &row(&list, SEC)["scan_status"];
    assert_eq!(status["verdict"], "allowed");
    assert!(status.get("detail").is_none(), "{status}");
}

/// T5: the worker's error reaches the admin redacted, then cut to 200.
#[actix_web::test]
async fn last_error_with_query_token_is_redacted() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "huge", "1.0.0").await;
    lab.verdict(SEC, "huge", "1.0.0", VerdictState::Allowed)
        .await;
    let coord = PackageId::new(SEC, "huge", "1.0.0");
    lab.queue
        .enqueue(&coord, None, ScanTrigger::Rescan)
        .await
        .unwrap();
    let job = lab.queue.open_jobs().await.remove(0);
    let error = format!(
        "exhausted: GET https://u:SENTINEL@up.test/t.tgz?token=SENTINEL failed {}",
        "x".repeat(300)
    );
    lab.queue.complete_exhausted(job.id, &error).await.unwrap();

    let list = get(&app, LIST, Some(ADMIN_TOKEN)).await;
    let status = &row(&list, SEC)["scan_status"];
    assert_eq!(status["state"], "failed", "rule 5 on an allowed version");
    let shown = status["detail"]["last_error"].as_str().unwrap();
    assert!(
        shown.starts_with("GET https://up.test/t.tgz failed "),
        "{shown}"
    );
    assert!(!shown.contains("SENTINEL"));
    assert_eq!(shown.chars().count(), 200);
}

// ── what it costs, and what it never does ────────────────────────────────────

/// T8b: anonymous on a default config resolves `Hidden` everywhere, so the
/// batch is not run; a `user` runs it once per store, whatever the number of
/// registries on the page.
#[actix_web::test]
async fn anonymous_list_runs_no_status_statement() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "a", "1.0.0").await;
    lab.seed(SEC2, "b", "1.0.0").await;
    lab.seed(PLAIN, "c", "1.0.0").await;

    lab.reset();
    get(&app, LIST, None).await;
    assert_eq!(lab.counts.jobs_for.load(Ordering::SeqCst), 0);
    assert_eq!(lab.counts.get_many.load(Ordering::SeqCst), 0);

    lab.reset();
    get(&app, LIST, Some(USER_TOKEN)).await;
    assert_eq!(lab.counts.jobs_for.load(Ordering::SeqCst), 1);
    assert_eq!(lab.counts.get_many.load(Ordering::SeqCst), 1);
}

/// T15: a page view enqueues no scan and writes no verdict.
#[actix_web::test]
async fn a_list_or_detail_view_enqueues_nothing_and_writes_nothing() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "fresh", "1.0.0").await;
    lab.reset();
    get(&app, LIST, Some(ADMIN_TOKEN)).await;
    get(
        &app,
        &format!("/api/v1/explore/packages/{SEC}/fresh?upstream=skip"),
        Some(ADMIN_TOKEN),
    )
    .await;
    get(&app, "/api/v1/admin/packages", Some(ADMIN_TOKEN)).await;
    assert_eq!(lab.counts.writes.load(Ordering::SeqCst), 0);
    assert!(lab.queue.open_jobs().await.is_empty());
}

/// §10.2: a failing batch drops every status and still serves the list.
#[actix_web::test]
async fn a_failing_batch_sets_scan_status_unavailable_and_serves_the_list() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "a", "1.0.0").await;
    lab.verdict(SEC, "a", "1.0.0", VerdictState::Allowed).await;
    lab.counts.fail.store(true, Ordering::SeqCst);
    let list = get(&app, LIST, Some(ADMIN_TOKEN)).await;
    assert_eq!(list["scan_status_unavailable"], true);
    assert!(row(&list, SEC).get("scan_status").is_none());
    let detail = get(
        &app,
        &format!("/api/v1/explore/packages/{SEC}/a?upstream=skip"),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(detail["scan_status_unavailable"], true);
    assert!(detail["versions"][0].get("scan_status").is_none());
}

/// Decision 42: the verdict GET keeps its `404` with no verdict row, which
/// `batlehub-cli wait` reads as "no verdict yet".
#[actix_web::test]
async fn the_verdict_get_still_answers_404_with_no_verdict_row() {
    let (app, lab) = app(false).await;
    lab.seed(SEC, "a", "1.0.0").await;
    let req = TestRequest::get()
        .uri(&format!("/api/v1/verdicts/{SEC}/a/1.0.0"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── the Scans page (phase 2) ─────────────────────────────────────────────────

const SCANS: &str = "/api/v1/admin/scans";

async fn status_of<S>(app: &S, uri: &str, token: Option<&str>) -> u16
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let mut req = TestRequest::get().uri(uri);
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    call_service(app, req.to_request()).await.status().as_u16()
}

impl Lab {
    /// `registry/failed@1` exhausted with `error`, `registry/running@1`
    /// leased, `registry/waiting@1` queued.
    async fn queue_three(&self, registry: &str, error: &str) {
        let q = &self.queue;
        let pkg = |n: &str| PackageId::new(registry, n, "1");
        q.enqueue(&pkg("failed"), None, ScanTrigger::FirstSeen)
            .await
            .unwrap();
        let job = q.lease("w-0", &[], 1, 60, 3).await.unwrap().remove(0);
        q.complete_exhausted(job.id, &format!("{EXHAUSTED_PREFIX}{error}"))
            .await
            .unwrap();
        q.enqueue(&pkg("running"), None, ScanTrigger::FirstSeen)
            .await
            .unwrap();
        q.enqueue(&pkg("waiting"), None, ScanTrigger::Rescan)
            .await
            .unwrap();
        q.lease("worker-2", &[], 1, 60, 3).await.unwrap();
    }
}

fn names(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}/{}",
                r["registry"].as_str().unwrap(),
                r["name"].as_str().unwrap()
            )
        })
        .collect()
}

#[actix_web::test]
async fn scans_list_shows_the_queue_scanning_first_and_the_failures() {
    let (app, lab) = app(false).await;
    lab.queue_three(SEC, "tar: unexpected EOF").await;

    let open = get(&app, SCANS, Some(ADMIN_TOKEN)).await;
    assert_eq!(
        names(&open),
        [format!("{SEC}/running"), format!("{SEC}/waiting")]
    );
    assert_eq!(open["total"], 2);
    assert_eq!(open["items"][0]["state"], "scanning");
    assert_eq!(open["items"][0]["leased_by"], "worker-2");
    assert_eq!(open["items"][1]["state"], "queued");
    assert!(open["items"][1].get("leased_by").is_none());

    let queued = get(&app, &format!("{SCANS}?state=queued"), Some(ADMIN_TOKEN)).await;
    assert_eq!(names(&queued), [format!("{SEC}/waiting")]);

    let failed = get(&app, &format!("{SCANS}?state=failed"), Some(ADMIN_TOKEN)).await;
    assert_eq!(names(&failed), [format!("{SEC}/failed")]);
    let row = &failed["items"][0];
    assert_eq!(row["state"], "failed");
    assert_eq!(row["last_error"], "tar: unexpected EOF", "prefix stripped");
    assert_eq!(row["attempts"], 1);
    assert_eq!(row["can_rescan"], true);

    // A rescan of the failed version takes it off Failures and onto the queue.
    lab.queue
        .enqueue(
            &PackageId::new(SEC, "failed", "1"),
            None,
            ScanTrigger::Rescan,
        )
        .await
        .unwrap();
    let failed = get(&app, &format!("{SCANS}?state=failed"), Some(ADMIN_TOKEN)).await;
    assert_eq!(failed["total"], 0);
    let open = get(&app, SCANS, Some(ADMIN_TOKEN)).await;
    assert_eq!(open["total"], 3);
}

#[actix_web::test]
async fn scans_list_pages_and_filters_by_registry() {
    let (app, lab) = app(false).await;
    lab.queue_three(SEC, "e").await;
    lab.queue_three(SEC2, "e").await;
    let one = get(&app, &format!("{SCANS}?registry={SEC2}"), Some(ADMIN_TOKEN)).await;
    assert_eq!(
        names(&one),
        [format!("{SEC2}/running"), format!("{SEC2}/waiting")]
    );
    assert_eq!(
        one["items"][0]["can_rescan"], false,
        "no gates:exempt on SEC2"
    );
    let p2 = get(
        &app,
        &format!("{SCANS}?per_page=3&page=2"),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(p2["total"], 4);
    assert_eq!(p2["items"].as_array().unwrap().len(), 1);
    let unknown = get(&app, &format!("{SCANS}?registry=nope"), Some(ADMIN_TOKEN)).await;
    assert_eq!(unknown["total"], 0);
}

/// T7, and a typo is a `400`, not an empty page.
#[actix_web::test]
async fn admin_lists_reject_per_page_over_200() {
    let (app, _lab) = app(false).await;
    for q in ["per_page=201", "per_page=0", "state=scaning"] {
        assert_eq!(
            status_of(&app, &format!("{SCANS}?{q}"), Some(ADMIN_TOKEN)).await,
            400,
            "{q}"
        );
    }
    assert_eq!(
        status_of(&app, &format!("{SCANS}?per_page=200"), Some(ADMIN_TOKEN)).await,
        200
    );
}

#[actix_web::test]
async fn scans_list_needs_system_read() {
    let (app, _lab) = app(false).await;
    assert_eq!(status_of(&app, SCANS, Some(USER_TOKEN)).await, 403);
    assert_eq!(status_of(&app, SCANS, None).await, 403);
}

/// T16: `system:read` opens the page, not every registry's holds, and not
/// the worker's error text without `findings:read`.
#[actix_web::test]
async fn scans_list_omits_registry_without_quarantine_read() {
    let (app, lab) = app_with(false, true).await;
    lab.queue_three(SEC, "SECRET-ERROR").await;
    lab.queue_three(SEC2, "SECRET-ERROR").await;

    let open = get(&app, SCANS, Some(USER_TOKEN)).await;
    assert_eq!(
        names(&open),
        [format!("{SEC}/running"), format!("{SEC}/waiting")]
    );
    let only = get(&app, &format!("{SCANS}?registry={SEC2}"), Some(USER_TOKEN)).await;
    assert_eq!(
        only["total"], 0,
        "an explicit registry is a filter, not a way in"
    );

    let failed = get(&app, &format!("{SCANS}?state=failed"), Some(USER_TOKEN)).await;
    assert_eq!(names(&failed), [format!("{SEC}/failed")]);
    let row = &failed["items"][0];
    assert!(row.get("last_error").is_none(), "{row}");
    assert_eq!(row["error_withheld"], true);
    assert_eq!(row["can_rescan"], false);
    assert!(!failed.to_string().contains("SECRET-ERROR"));
}

/// T9: the bulk rescan the Scans page links to refuses without
/// `system:write`; the UI hiding the button is cosmetic.
#[actix_web::test]
async fn bulk_rescan_without_system_write_is_403() {
    let (app, _lab) = app_with(false, true).await;
    let req = TestRequest::post()
        .uri("/api/v1/admin/verdicts/rescan")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "registry": SEC }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 403);
}

// ── /admin/findings (RFC 0042 phase 3) ───────────────────────────────────────

const FINDINGS: &str = "/api/v1/admin/findings";

impl Lab {
    /// One verdict carrying one `osv` finding.
    async fn finding(
        &self,
        coord: (&str, &str, &str),
        state: VerdictState,
        severity: batlehub_core::entities::Severity,
        reference: &str,
    ) {
        use batlehub_core::entities::{Finding, FindingKind, ReasonCode};
        let (registry, name, version) = coord;
        self.verdicts
            .upsert(&Verdict {
                package: PackageId::new(registry, name, version),
                state,
                reason_codes: vec![],
                findings: vec![Finding {
                    scanner: "osv".into(),
                    kind: FindingKind::Vulnerability,
                    code: ReasonCode::Vulnerability,
                    severity,
                    reference: Some(reference.into()),
                    summary: format!("{reference} in {name}"),
                    confidence: None,
                    available_at: None,
                    raw: serde_json::json!({ "secret": "RAW-SENTINEL" }),
                    judged_as: None,
                }],
                policy_ref: format!("{registry}/default"),
                available_at: None,
                evaluated_at: Utc::now(),
                last_scanned_at: Some(Utc::now()),
                scanners_done: vec!["osv".into()],
            })
            .await
            .unwrap();
    }

    /// The §4.5 mock's rows, across `SEC` and `SEC2`.
    async fn seed_findings(&self) {
        use batlehub_core::entities::Severity::*;
        let f = |r, n, v| (r, n, v);
        self.finding(
            f(SEC, "lodash", "2.4.1"),
            VerdictState::Denied,
            High,
            "GHSA-35jh-r3h4-6jhm",
        )
        .await;
        self.finding(
            f(SEC, "lodash", "2.4.0"),
            VerdictState::Warned,
            High,
            "GHSA-35jh-r3h4-6jhm",
        )
        .await;
        self.finding(
            f(SEC2, "evil", "0.0.1"),
            VerdictState::Denied,
            Critical,
            "MAL-2026-1234",
        )
        .await;
        self.finding(
            f(SEC, "tiny", "1.0.0"),
            VerdictState::Allowed,
            Low,
            "GHSA-aaaa-bbbb-cccc",
        )
        .await;
    }
}

fn coords(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}/{}@{}",
                r["registry"].as_str().unwrap(),
                r["name"].as_str().unwrap(),
                r["version"].as_str().unwrap()
            )
        })
        .collect()
}

#[actix_web::test]
async fn findings_list_filters_orders_and_groups() {
    let (app, lab) = app(false).await;
    lab.seed_findings().await;

    let all = get(&app, FINDINGS, Some(ADMIN_TOKEN)).await;
    assert_eq!(all["total"], 4);
    assert_eq!(
        coords(&all),
        [
            format!("{SEC2}/evil@0.0.1"),
            format!("{SEC}/lodash@2.4.0"),
            format!("{SEC}/lodash@2.4.1"),
            format!("{SEC}/tiny@1.0.0"),
        ],
        "most severe first"
    );
    let first = &all["items"][0];
    assert_eq!(first["severity"], "critical");
    assert_eq!(first["verdict"], "denied");
    assert_eq!(first["reference"], "MAL-2026-1234");
    assert!(
        !all.to_string().contains("RAW-SENTINEL"),
        "raw never served"
    );

    // [Exempt…]: a held verdict where the admin holds gates:exempt (SEC only).
    let exempt: Vec<bool> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["can_exempt"].as_bool().unwrap())
        .collect();
    assert_eq!(exempt, [false, false, true, false]);

    let q = |q: &str| format!("{FINDINGS}?{q}");
    let high = get(&app, &q("severity=high"), Some(ADMIN_TOKEN)).await;
    assert_eq!(high["total"], 3, "at or above high");
    let denied = get(&app, &q("verdict=denied"), Some(ADMIN_TOKEN)).await;
    assert_eq!(denied["total"], 2);
    let prefix = get(&app, &q("reference=GHSA-35"), Some(ADMIN_TOKEN)).await;
    assert_eq!(prefix["total"], 2);
    let none = get(&app, &q("scanner=guarddog"), Some(ADMIN_TOKEN)).await;
    assert_eq!(none["total"], 0);
    let one = get(&app, &q(&format!("registry={SEC2}")), Some(ADMIN_TOKEN)).await;
    assert_eq!(coords(&one), [format!("{SEC2}/evil@0.0.1")]);

    let grouped = get(&app, &q("group=reference"), Some(ADMIN_TOKEN)).await;
    let refs: Vec<&str> = grouped["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reference"].as_str().unwrap())
        .collect();
    assert_eq!(
        refs,
        [
            "GHSA-35jh-r3h4-6jhm",
            "GHSA-35jh-r3h4-6jhm",
            "GHSA-aaaa-bbbb-cccc",
            "MAL-2026-1234"
        ]
    );

    let p2 = get(&app, &q("per_page=3&page=2"), Some(ADMIN_TOKEN)).await;
    assert_eq!(p2["total"], 4);
    assert_eq!(coords(&p2), [format!("{SEC}/tiny@1.0.0")]);
}

/// T3: the registries a caller reads findings on are a ceiling, and an
/// explicit registry outside it is an empty page, not a `403`.
#[actix_web::test]
async fn findings_list_without_registry_is_scoped_to_granted_registries() {
    let (app, lab) = app_opts(Opts {
        sec_findings_reader: true,
        ..Opts::default()
    })
    .await;
    lab.seed_findings().await;
    let mine = get(&app, FINDINGS, Some(USER_TOKEN)).await;
    assert!(
        coords(&mine)
            .iter()
            .all(|c| c.starts_with(&format!("{SEC}/"))),
        "{mine}"
    );
    assert_eq!(mine["total"], 3);
    assert!(!mine.to_string().contains("MAL-2026-1234"));
    assert_eq!(mine["items"][0]["can_exempt"], false, "no gates:exempt");
}

#[actix_web::test]
async fn findings_list_for_ungranted_registry_is_empty_200() {
    let (app, lab) = app_opts(Opts {
        sec_findings_reader: true,
        ..Opts::default()
    })
    .await;
    lab.seed_findings().await;
    let other = get(
        &app,
        &format!("{FINDINGS}?registry={SEC2}"),
        Some(USER_TOKEN),
    )
    .await;
    assert_eq!(other["total"], 0);
    assert_eq!(other["items"], serde_json::json!([]));
}

#[actix_web::test]
async fn findings_list_held_nowhere_is_403() {
    let (app, lab) = app(false).await;
    lab.seed_findings().await;
    assert_eq!(status_of(&app, FINDINGS, Some(USER_TOKEN)).await, 403);
    assert_eq!(status_of(&app, FINDINGS, None).await, 403);
}

/// T3b: an active shadow on `SEC` makes `authorize_grants` answer `Ok`; the
/// list is built on `authorize_control`, so it opens nothing.
#[actix_web::test]
async fn findings_list_ignores_active_shadow() {
    let (app, lab) = app(true).await;
    lab.seed_findings().await;
    assert_eq!(status_of(&app, FINDINGS, Some(USER_TOKEN)).await, 403);
}

/// T3c: the registry grant reaches the list, and the namespace seal still
/// cuts it, exactly as it cuts the per-version GET.
#[actix_web::test]
async fn findings_list_respects_namespace_seal() {
    use batlehub_core::entities::Severity::Critical;
    let (app, lab) = app_opts(Opts {
        sec_findings_reader: true,
        ..Opts::default()
    })
    .await;
    lab.seed_findings().await;
    lab.finding(
        (SEC, "@sealed/core", "1.0.0"),
        VerdictState::Denied,
        Critical,
        "GHSA-seal-seal-seal",
    )
    .await;
    let mine = get(&app, FINDINGS, Some(USER_TOKEN)).await;
    assert!(!mine.to_string().contains("@sealed"), "{mine}");
    assert_eq!(
        coords(&mine).len(),
        3,
        "the control: the rest of SEC is still listed"
    );
}

/// T7 and §4.3: every malformed filter is a `400`, not an empty page.
#[actix_web::test]
async fn findings_list_rejects_malformed_filters() {
    let (app, _lab) = app(false).await;
    let long = "x".repeat(129);
    for q in [
        "per_page=201",
        "per_page=0",
        "severity=hgh",
        "verdict=held",
        "group=package",
        &format!("reference={long}"),
    ] {
        assert_eq!(
            status_of(&app, &format!("{FINDINGS}?{q}"), Some(ADMIN_TOKEN)).await,
            400,
            "{q}"
        );
    }
    let ok = format!("{FINDINGS}?per_page=200&reference={}", "x".repeat(128));
    assert_eq!(status_of(&app, &ok, Some(ADMIN_TOKEN)).await, 200);
}

/// T10: `%` and `_` in `reference` match themselves.
#[actix_web::test]
async fn findings_reference_filter_escapes_like_metacharacters() {
    let (app, lab) = app(false).await;
    lab.seed_findings().await;
    for q in ["GHSA-%25", "GHSA-35jh_", "%25"] {
        let page = get(
            &app,
            &format!("{FINDINGS}?reference={q}"),
            Some(ADMIN_TOKEN),
        )
        .await;
        assert_eq!(page["total"], 0, "{q}");
    }
}
