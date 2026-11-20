//! RFC 0050 — the security console's summary endpoints and the instance's own
//! security: who reads which block, seals, the window fallback, and what is
//! never in a body (§7.1 T1–T4, T6, T7, §14).

mod common;
use common::*;

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use actix_web::test::{call_service, TestRequest};
use chrono::Utc;
use serde_json::Value;

use batlehub_adapters::cache::InMemoryCacheStore;
use batlehub_adapters::in_memory::{
    InMemoryPackageRepository as InMemoryRepo, InMemoryScanQueue, InMemoryScannerStatus,
    InMemorySecurityDaily, InMemoryStorageBackend, InMemoryVerdictRepository,
    InMemoryWorkerRegistry, NoopArtifactMetaRepository, NullUserTokenRepository,
};
use batlehub_core::entities::{
    Action, GrantMap, PackageId, ReasonCode, Role, SecurityPolicy, SubjectMatcher, Verdict,
    VerdictState,
};
use batlehub_core::ports::{
    BlockedIpInfo, CacheStore, DailyState, IpBlockStore, PackageRepository, PolicyRepository,
    RegistryClient, ScannerStatus, ScannerStatusRepository, SecurityDailyRepository,
    StorageBackend, UserTokenRepository, VerdictRepository, WorkerRegistry,
};
use batlehub_core::services::{
    new_hot_lock, AdminService, HotConfig, ProxyMetrics, ProxyService, RegistryPolicy,
};
use batlehub_web::{new_access_lock, RegistryModeMap};

/// Two protected registries — the second sealed for the delegate — and one
/// unprotected proxy.
const SEC: &str = "npm-sec";
const SEALED: &str = "npm-sealed";
const PLAIN: &str = "npm";

/// A static token stored in clear: its value must appear in no body (T4).
const PLAIN_TOKEN_VALUE: &str = "s3cr3t-plain-token-value";

struct Lab {
    verdicts: Arc<InMemoryVerdictRepository>,
    daily: Arc<InMemorySecurityDaily>,
    status: Arc<InMemoryScannerStatus>,
    workers: Arc<InMemoryWorkerRegistry>,
    hot: batlehub_core::services::hot_config::HotConfigLock,
    /// Shared with the app: a test sets a registry's mode here.
    modes: RegistryModeMap,
    /// The access log the flag store's exposure reads.
    repo: Arc<InMemoryRepo>,
    advisories: Arc<batlehub_adapters::in_memory::InMemoryAdvisoryRepository>,
}

impl Lab {
    async fn hold(&self, registry: &str, name: &str, state: VerdictState, codes: &[ReasonCode]) {
        self.hold_at(registry, name, "1.0.0", state, codes, Utc::now())
            .await;
    }

    async fn hold_at(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        state: VerdictState,
        codes: &[ReasonCode],
        evaluated_at: chrono::DateTime<Utc>,
    ) {
        self.verdicts
            .upsert(&Verdict {
                package: PackageId::new(registry, name, version),
                state,
                reason_codes: codes.to_vec(),
                findings: Vec::new(),
                policy_ref: format!("{registry}/default"),
                available_at: None,
                evaluated_at,
                last_scanned_at: Some(Utc::now()),
                scanners_done: vec!["osv".into()],
            })
            .await
            .unwrap();
    }
}

/// The app; `user_verbs` are what a `role:user` caller holds at the instance
/// tier (the delegated security group of RFC 0042).
async fn app(
    user_verbs: &[Action],
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Lab,
) {
    app_with(user_verbs, ConfigureAppDefaults::default(), None).await
}

/// [`app`] with the test app's defaults and a policy store of the test's.
async fn app_with(
    user_verbs: &[Action],
    defaults: ConfigureAppDefaults,
    policy_repo: Option<Arc<dyn PolicyRepository>>,
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
    let advisories = Arc::new(
        batlehub_adapters::in_memory::InMemoryAdvisoryRepository::with_events(repo_dyn.clone()),
    );
    let mut defaults = defaults;
    if defaults.advisory_repo.is_none() {
        defaults.advisory_repo = Some(advisories.clone());
    }
    let storage: Arc<dyn StorageBackend> = InMemoryStorageBackend::new();
    let cache: Arc<dyn CacheStore> = Arc::new(InMemoryCacheStore::new());
    let names = [SEC, SEALED, PLAIN];
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
            // An empty node is the seal: the delegate's instance verbs stop
            // here. Only in a delegate's app, so the admin tests see it.
            if *n == SEALED && !user_verbs.is_empty() {
                g.registry.grants = Some(GrantMap::sealed());
            }
            (n.to_string(), Arc::new(g))
        })
        .collect();
    let verdicts = InMemoryVerdictRepository::new();
    let daily = InMemorySecurityDaily::new();
    let status = InMemoryScannerStatus::new();
    let workers = InMemoryWorkerRegistry::new();
    let hot = new_hot_lock(HotConfig {
        grants,
        instance: Some(Arc::new(
            batlehub_core::services::authz::translate::instance_node(
                (!user_verbs.is_empty())
                    .then(|| {
                        GrantMap::new()
                            .grant(SubjectMatcher::Role(Role::User), user_verbs.iter().copied())
                    })
                    .as_ref(),
            ),
        )),
        registries,
        policies,
        security: [SEC, SEALED]
            .iter()
            .map(|n| (n.to_string(), SecurityPolicy::defaults_for(n)))
            .collect(),
        verdicts: Some(verdicts.clone()),
        scan_queue: Some(InMemoryScanQueue::new()),
        worker_registry: Some(workers.clone()),
        scanner_status: Some(status.clone()),
        security_daily: Some(daily.clone()),
        policy_repo,
        plain_static_tokens: Arc::new(vec![(
            "auth[0].tokens[3]".to_owned(),
            Some("ci-bot".to_owned()),
        )]),
        versions_per_page: batlehub_core::services::hot_config::DEFAULT_VERSIONS_PER_PAGE,
        packages_per_page: batlehub_core::services::hot_config::DEFAULT_PACKAGES_PER_PAGE,
        ..Default::default()
    });
    let hot_for_lab = hot.clone();
    let modes = RegistryModeMap::default();
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
        fills: Default::default(),
        discovery: Default::default(),
        explore_cache: None,
    });
    let admin_svc = Arc::new(AdminService::new(repo_dyn.clone()));
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
    let registry_map = registry_map_for(&[(SEC, "npm"), (SEALED, "npm"), (PLAIN, "npm")]);
    let app = finish_test_app(
        proxy_svc,
        admin_svc,
        token_repo,
        access,
        registry_map,
        local_svc,
        modes.clone(),
        batlehub_web::CargoIndexMap::default(),
        defaults,
        test_auth_providers(),
    )
    .await;
    (
        app,
        Lab {
            verdicts,
            daily,
            status,
            workers,
            hot: hot_for_lab,
            modes,
            repo,
            advisories,
        },
    )
}

async fn call<S>(app: &S, uri: &str, token: &str) -> (u16, Value)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(token)))
        .to_request();
    let resp = call_service(app, req).await;
    let status = resp.status().as_u16();
    let body = actix_web::test::read_body(resp).await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

const OVERVIEW: &str = "/api/v1/admin/security/overview";

// ── the overview ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn the_overview_says_what_is_wrong_per_domain() {
    let (app, lab) = app(&[]).await;
    lab.hold(
        SEC,
        "event-stream",
        VerdictState::Denied,
        &[ReasonCode::SocVerdict],
    )
    .await;
    lab.hold(
        SEC,
        "young",
        VerdictState::Quarantined,
        &[ReasonCode::MinAgeNotMet],
    )
    .await;
    let (status, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    let holds = &body["holds"];
    assert_eq!(holds["state"], "attention", "{body}");
    assert_eq!(holds["held"], 2);
    assert_eq!(holds["malware"], 1);
    assert_eq!(holds["waiting"], 1);
    // No worker, no scanner row: both are attention, never OK.
    assert_eq!(body["scanners"]["state"], "attention");
    assert_eq!(body["scanners"]["never_ran"], 1);
    // The plain proxy has neither a profile nor a CVE gate.
    assert_eq!(body["coverage"]["state"], "attention");
    assert_eq!(body["coverage"]["unprotected"], serde_json::json!([PLAIN]));
    assert!(body["flags"].is_object());
    // The hard-block hold is the decision.
    assert_eq!(body["decisions"]["items"][0]["kind"], "hold_on_hard_block");
    assert_eq!(body["decisions"]["items"][0]["name"], "event-stream");
}

/// `[console].accepted = ["coverage:npm"]`: the plain proxy is no longer
/// attention, and is still named — on the line and on its Coverage row.
#[actix_web::test]
async fn a_registry_accepted_as_unprotected_is_named_not_attention() {
    let (app, lab) = app(&[]).await;
    lab.hot
        .write()
        .await
        .accepted_unprotected
        .insert(PLAIN.to_owned());
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(body["coverage"]["state"], "ok", "{}", body["coverage"]);
    assert_eq!(body["coverage"]["unprotected"], serde_json::json!([]));
    assert_eq!(body["coverage"]["accepted"], serde_json::json!([PLAIN]));
    let (_, cov) = call(&app, "/api/v1/admin/security/coverage", ADMIN_TOKEN).await;
    let row = |name: &str| {
        cov["registries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["registry"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(row(PLAIN)["accepted"], true);
    assert_eq!(row(SEC)["accepted"], false);
}

#[actix_web::test]
async fn a_live_worker_and_a_healthy_scanner_read_ok() {
    let (app, lab) = app(&[]).await;
    lab.workers.heartbeat("w1", &[]).await.unwrap();
    lab.status
        .record(&ScannerStatus {
            scanner: "osv".into(),
            last_ok_at: Some(Utc::now()),
            last_error_at: None,
            last_error_class: None,
            unavailable_until: None,
        })
        .await
        .unwrap();
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(body["scanners"]["state"], "ok", "{body}");
    assert_eq!(body["scanners"]["live_workers"], 1);
    // A worker that covers only another registry does not count (§4.2).
    lab.workers
        .beat_at("w1", Utc::now() - chrono::Duration::hours(1), &[])
        .await;
    lab.workers
        .heartbeat("w2", &[PLAIN.to_owned()])
        .await
        .unwrap();
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(body["scanners"]["state"], "attention");
    assert_eq!(
        body["scanners"]["uncovered"],
        serde_json::json!([SEALED, SEC])
    );
}

/// T1: a holder of `flags:read` alone reads the flags block and nothing else.
#[actix_web::test]
async fn flags_read_alone_sees_only_the_flags_block() {
    let (app, lab) = app(&[Action::FlagsRead]).await;
    lab.hold(SEC, "x", VerdictState::Denied, &[ReasonCode::SocVerdict])
        .await;
    let (status, body) = call(&app, OVERVIEW, USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["flags"].is_object());
    for absent in ["holds", "scanners", "coverage"] {
        assert!(body.get(absent).is_none(), "{absent} leaked: {body}");
    }
    assert_eq!(body["decisions"]["total"], 0);
}

#[actix_web::test]
async fn no_security_verb_is_refused() {
    let (app, _) = app(&[]).await;
    let (status, body) = call(&app, OVERVIEW, USER_TOKEN).await;
    assert_eq!(status, 403);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("quarantine:read"),
        "{body}"
    );
    for uri in [
        "/api/v1/admin/security/trends",
        "/api/v1/admin/security/coverage",
        "/api/v1/admin/security/scanners",
        "/api/v1/admin/instance-security",
    ] {
        assert_eq!(call(&app, uri, USER_TOKEN).await.0, 403, "{uri}");
    }
}

/// T2: a registry sealed for the caller is absent everywhere and `404` by
/// name, exactly like an unknown one.
#[actix_web::test]
async fn a_sealed_registry_is_an_unknown_one() {
    let (app, lab) = app(&[Action::QuarantineRead, Action::FlagsRead]).await;
    lab.hold(
        SEALED,
        "hidden",
        VerdictState::Denied,
        &[ReasonCode::SocVerdict],
    )
    .await;
    let (_, body) = call(&app, OVERVIEW, USER_TOKEN).await;
    assert_eq!(body["holds"]["held"], 0, "{body}");
    assert_eq!(body["decisions"]["total"], 0);
    assert!(!body.to_string().contains(SEALED), "{body}");
    let (_, cov) = call(&app, "/api/v1/admin/security/coverage", USER_TOKEN).await;
    let names: Vec<&str> = cov["registries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["registry"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec![PLAIN, SEC]);
    for uri in [
        format!("/api/v1/admin/security/coverage/{SEALED}"),
        "/api/v1/admin/security/coverage/nope".to_owned(),
        format!("/api/v1/admin/security/trends?registry={SEALED}"),
    ] {
        assert_eq!(call(&app, &uri, USER_TOKEN).await.0, 404, "{uri}");
    }
    // Unsealed, the same hold is counted.
    let (app, lab) = crate::app(&[]).await;
    lab.hold(
        SEALED,
        "hidden",
        VerdictState::Denied,
        &[ReasonCode::SocVerdict],
    )
    .await;
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(body["holds"]["held"], 1);
}

// ── coverage and scanners ────────────────────────────────────────────────────

#[actix_web::test]
async fn coverage_detail_and_scanner_names_need_config_read() {
    let (app, _) = app(&[Action::QuarantineRead]).await;
    let (status, row) = call(
        &app,
        &format!("/api/v1/admin/security/coverage/{SEC}"),
        USER_TOKEN,
    )
    .await;
    assert_eq!(status, 200, "{row}");
    assert_eq!(row["protected"], true);
    assert_eq!(row["security_mode"], "block");
    assert_eq!(row["deny_at"], "high");
    assert!(row.get("scanners").is_none(), "{row}");
    let (_, row) = call(
        &app,
        &format!("/api/v1/admin/security/coverage/{SEC}"),
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(row["scanners"][0]["name"], "osv");
    assert_eq!(row["scanners"][0]["required"], true);
    let (_, plain) = call(
        &app,
        &format!("/api/v1/admin/security/coverage/{PLAIN}"),
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(plain["protected"], false);
    assert!(plain.get("deny_at").is_none());
}

#[actix_web::test]
async fn the_error_class_needs_findings_read() {
    let (app, lab) = app(&[Action::QuarantineRead]).await;
    lab.status
        .record(&ScannerStatus {
            scanner: "osv".into(),
            last_ok_at: Some(Utc::now() - chrono::Duration::minutes(5)),
            last_error_at: Some(Utc::now()),
            last_error_class: Some("timeout".into()),
            unavailable_until: None,
        })
        .await
        .unwrap();
    let (_, body) = call(&app, "/api/v1/admin/security/scanners", USER_TOKEN).await;
    let osv = &body["scanners"][0];
    assert_eq!(osv["state"], "attention", "{body}");
    assert!(osv.get("last_error_class").is_none());
    let (_, body) = call(&app, "/api/v1/admin/security/scanners", ADMIN_TOKEN).await;
    assert_eq!(body["scanners"][0]["last_error_class"], "timeout");
}

// ── trends ───────────────────────────────────────────────────────────────────

#[actix_web::test]
async fn trends_fall_back_to_30_days_and_keep_gaps() {
    let (app, lab) = app(&[Action::QuarantineRead]).await;
    let today = Utc::now().date_naive();
    lab.daily
        .upsert_state(
            SEC,
            today,
            &DailyState {
                denied: 4,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (status, body) = call(&app, "/api/v1/admin/security/trends?window=14d", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["window_days"], 30);
    let days = body["days"].as_array().unwrap();
    assert_eq!(days.len(), 30);
    assert_eq!(days[29]["denied"], 4);
    // A day with no row is a gap, never a zero.
    assert!(days[0]["denied"].is_null());
    // The flag series needs flags:read.
    assert!(days[29].get("flags_opened").is_none());
    let (_, body) = call(&app, "/api/v1/admin/security/trends?window=7d", ADMIN_TOKEN).await;
    assert_eq!(body["days"].as_array().unwrap().len(), 7);
}

/// Decision 37: with no cache (the `read_cache = false` shape) every request
/// reads its source again.
#[actix_web::test]
async fn without_a_cache_every_request_reads_the_source() {
    let (app, lab) = app(&[]).await;
    call(&app, "/api/v1/admin/security/trends", ADMIN_TOKEN).await;
    call(&app, "/api/v1/admin/security/trends", ADMIN_TOKEN).await;
    assert_eq!(lab.daily.range_calls.load(Ordering::Relaxed), 2);
}

// ── instance security ────────────────────────────────────────────────────────

/// T3, T4, T6: without `audit:read` no address or user id; without
/// `quarantine:read` no exemptions; the token value nowhere.
#[actix_web::test]
async fn instance_security_withholds_per_verb() {
    let (app, _) = app(&[Action::SystemRead]).await;
    let (status, body) = call(&app, "/api/v1/admin/instance-security", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["static_tokens"]["plain"], 1);
    assert_eq!(
        body["static_tokens"]["paths"],
        serde_json::json!(["auth[0].tokens[3]"])
    );
    assert!(body["static_tokens"].get("user_ids").is_none(), "{body}");
    assert!(body["sign_ins"].get("addresses").is_none());
    assert!(body["token_sources"].get("items").is_none());
    assert!(body.get("exemptions").is_none());
    assert!(!body.to_string().contains(PLAIN_TOKEN_VALUE));
    assert_eq!(body["window_hours"], 24);

    let (_, body) = call(
        &app,
        "/api/v1/admin/instance-security?window=7d",
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(body["window_hours"], 168);
    assert_eq!(
        body["static_tokens"]["user_ids"],
        serde_json::json!(["ci-bot"])
    );
    assert!(body["sign_ins"]["addresses"].is_array());
    assert!(body["exemptions"].is_object());
    assert!(!body.to_string().contains(PLAIN_TOKEN_VALUE));
}

// ── what the review of 2026-11-17 found ──────────────────────────────────────

/// Decision 27: a worker's registry list names only what the caller sees, and
/// a worker serving only sealed registries is not there at all.
#[actix_web::test]
async fn a_worker_names_no_sealed_registry() {
    let (app, lab) = app(&[Action::QuarantineRead]).await;
    lab.workers
        .heartbeat("w-sealed", &[SEALED.to_owned()])
        .await
        .unwrap();
    lab.workers
        .heartbeat("w-both", &[SEALED.to_owned(), SEC.to_owned()])
        .await
        .unwrap();
    let (status, body) = call(&app, "/api/v1/admin/security/scanners", USER_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    assert!(!body.to_string().contains(SEALED), "{body}");
    assert!(!body.to_string().contains("w-sealed"), "{body}");
    assert_eq!(body["workers"][0]["registries"], serde_json::json!([SEC]));
    let (_, body) = call(&app, OVERVIEW, USER_TOKEN).await;
    assert_eq!(body["scanners"]["live_workers"], 1, "{body}");
    // Unsealed, both workers are there, whole.
    let (app, lab) = crate::app(&[]).await;
    lab.workers
        .heartbeat("w-sealed", &[SEALED.to_owned()])
        .await
        .unwrap();
    let (_, body) = call(&app, "/api/v1/admin/security/scanners", ADMIN_TOKEN).await;
    assert_eq!(
        body["workers"][0]["registries"],
        serde_json::json!([SEALED]),
        "{body}"
    );
}

/// `history_starts` is the oldest day of the caller's registries, not of a
/// sealed one.
#[actix_web::test]
async fn history_starts_ignores_a_sealed_registry() {
    let (app, lab) = app(&[Action::QuarantineRead]).await;
    let today = Utc::now().date_naive();
    for (r, day) in [(SEALED, today - chrono::Duration::days(10)), (SEC, today)] {
        lab.daily
            .upsert_state(r, day, &DailyState::default())
            .await
            .unwrap();
    }
    let (_, body) = call(&app, "/api/v1/admin/security/trends", USER_TOKEN).await;
    assert_eq!(body["history_starts"], serde_json::json!(today), "{body}");
    // Unsealed, the older day is the start.
    let (app, lab) = crate::app(&[]).await;
    lab.daily
        .upsert_state(
            SEALED,
            today - chrono::Duration::days(10),
            &DailyState::default(),
        )
        .await
        .unwrap();
    let (_, body) = call(&app, "/api/v1/admin/security/trends", ADMIN_TOKEN).await;
    assert_eq!(
        body["history_starts"],
        serde_json::json!(today - chrono::Duration::days(10))
    );
}

/// A version exempted from two gates is one exemption: one decision, one
/// count, the earlier end.
#[actix_web::test]
async fn an_exemption_over_two_gates_counts_once() {
    let policies = batlehub_adapters::in_memory::InMemoryPolicyRepository::new();
    let soon = Utc::now() + chrono::Duration::days(3);
    let sooner = Utc::now() + chrono::Duration::days(2);
    let rule = |gate: &str, until: chrono::DateTime<Utc>| batlehub_core::entities::RuleOverride {
        gate: gate.to_owned(),
        settings: serde_json::json!({
            "exempt": true, "gate": gate, "exempt_until": until, "reason": "test",
        }),
    };
    policies
        .put_policy(batlehub_core::ports::StoredPolicy {
            registry: SEC.to_owned(),
            node_kind: batlehub_core::ports::NodeKind::Version,
            node_key: batlehub_core::ports::version_node_key("pkg", "1.0.0"),
            visibility: None,
            prerelease_visibility: None,
            versioning: None,
            quota: None,
            // A lapsed gate is history: it neither hides the version nor
            // becomes its end.
            rules: vec![
                rule("security_verdict", soon),
                rule("cve_gate", sooner),
                rule("license_gate", Utc::now() - chrono::Duration::days(1)),
            ],
            set_by: None,
        })
        .await
        .unwrap();
    let (app, _) = app_with(&[], ConfigureAppDefaults::default(), Some(policies)).await;
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    let expiring: Vec<&Value> = body["decisions"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"] == "exemption_expiring")
        .collect();
    assert_eq!(expiring.len(), 1, "{body}");
    assert_eq!(
        expiring[0]["expires_at"].as_str().unwrap()[..10],
        sooner.to_rfc3339()[..10]
    );
    let (_, body) = call(&app, "/api/v1/admin/instance-security", ADMIN_TOKEN).await;
    assert_eq!(body["exemptions"]["expiring_7d"], 1, "{body}");
}

/// A day where one registry has a figure and another none is a gap, not the
/// one registry's figure drawn as the total.
#[actix_web::test]
async fn a_day_missing_one_registrys_figure_is_a_gap() {
    use batlehub_core::ports::DailyEvents;
    let (app, lab) = app(&[]).await;
    let today = Utc::now().date_naive();
    for (r, refusals) in [(SEC, None), (SEALED, Some(3))] {
        lab.daily
            .upsert_events(
                r,
                today,
                &DailyEvents {
                    refusals,
                    jobs_done: Some(2),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let (_, body) = call(&app, "/api/v1/admin/security/trends?window=7d", ADMIN_TOKEN).await;
    let day = &body["days"][6];
    assert!(day["refusals"].is_null(), "{body}");
    assert_eq!(day["jobs_done"], 4, "{body}");
}

/// A ban store that does not answer.
struct DownBans;

#[async_trait::async_trait]
impl IpBlockStore for DownBans {
    async fn record_violation(
        &self,
        _: &str,
        _: u32,
    ) -> Result<(u64, u64), batlehub_core::error::CoreError> {
        Ok((0, 0))
    }
    async fn blocked_until(&self, _: &str) -> Result<Option<u64>, batlehub_core::error::CoreError> {
        Ok(None)
    }
    async fn block_ip(
        &self,
        _: &str,
        _: u64,
        _: &str,
    ) -> Result<(), batlehub_core::error::CoreError> {
        Ok(())
    }
    async fn unblock_ip(&self, _: &str) -> Result<(), batlehub_core::error::CoreError> {
        Ok(())
    }
    async fn list_blocked(&self) -> Result<Vec<BlockedIpInfo>, batlehub_core::error::CoreError> {
        Err(batlehub_core::error::CoreError::Cache(
            "redis is down".into(),
        ))
    }
}

/// One store down is one line unknown, never the whole page.
#[actix_web::test]
async fn a_ban_store_down_is_one_unknown_line() {
    let defaults = ConfigureAppDefaults {
        ip_block_store: Arc::new(DownBans),
        ..Default::default()
    };
    let (app, _) = app_with(&[], defaults, None).await;
    let (status, body) = call(&app, "/api/v1/admin/instance-security", ADMIN_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["bans"]["unknown_reason"].is_string(), "{body}");
    assert_eq!(body["static_tokens"]["plain"], 1);
}

// ── what the review of 2026-11-17 (third round) found ───────────────────────

/// The *Holds* line counts every held row: an old `SCAN_PENDING` and an old
/// `SOC_VERDICT` hold behind a thousand newer ones are still counted, and the
/// hard block is still a decision.
#[actix_web::test]
async fn old_holds_count_past_a_thousand_newer_ones() {
    let (app, lab) = app(&[]).await;
    let now = Utc::now();
    let q = VerdictState::Quarantined;
    for i in 0..1000 {
        lab.hold_at(
            SEC,
            "young",
            &format!("1.0.{i}"),
            q,
            &[ReasonCode::MinAgeNotMet],
            now,
        )
        .await;
    }
    let old = now - chrono::Duration::days(30);
    lab.hold_at(SEC, "stuck", "1.0.0", q, &[ReasonCode::ScanPending], old)
        .await;
    lab.hold_at(SEC, "evil", "1.0.0", q, &[ReasonCode::SocVerdict], old)
        .await;
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    let holds = &body["holds"];
    assert_eq!(holds["state"], "attention", "{holds}");
    assert_eq!(holds["pending_over_hour"], 1, "{holds}");
    assert_eq!(holds["malware"], 1, "{holds}");
    assert_eq!(holds["waiting"], 1001, "{holds}");
    let items = body["decisions"]["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|d| d["kind"] == "hold_on_hard_block" && d["name"] == "evil"),
        "{body}"
    );
}

/// A mass compromise: 1 400 versions held on a SOC's hard block. The Holds
/// line counts them exactly, the decision total says 1 400 (not the 1 000
/// rows read), and the listing walks all of them, page by page, filtered by
/// the reason code and across registries.
#[actix_web::test]
async fn a_mass_compromise_is_counted_and_listed_in_full() {
    let (app, lab) = app(&[]).await;
    let now = Utc::now();
    let d = VerdictState::Denied;
    for i in 0..1400 {
        let registry = if i % 2 == 0 { SEC } else { SEALED };
        lab.hold_at(
            registry,
            &format!("pkg-{i}"),
            "1.0.0",
            d,
            &[ReasonCode::SocVerdict],
            now - chrono::Duration::seconds(i),
        )
        .await;
    }
    lab.hold_at(
        SEC,
        "young",
        "1.0.0",
        VerdictState::Quarantined,
        &[ReasonCode::MinAgeNotMet],
        now,
    )
    .await;
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(body["holds"]["soc_verdict"], 1400, "{}", body["holds"]);
    assert_eq!(body["decisions"]["total"], 1400, "{}", body["decisions"]);
    // The per-kind figure behind the "→ all N" link, and at most ten versions.
    assert_eq!(body["decisions"]["by_kind"]["hold_on_hard_block"], 1400);
    let shown: std::collections::BTreeSet<String> = body["decisions"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| format!("{}/{}@{}", d["registry"], d["name"], d["version"]))
        .collect();
    assert_eq!(shown.len(), 10, "{}", body["decisions"]);

    // Every protected registry, SOC holds only, 500 a page: three pages.
    let mut seen = std::collections::BTreeSet::new();
    for page in 1..=3 {
        let (status, body) = call(
            &app,
            &format!("/api/v1/admin/verdicts?reason=SOC_VERDICT&per_page=500&page={page}"),
            ADMIN_TOKEN,
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["total"], 1400);
        for v in body["items"].as_array().unwrap() {
            assert!(v["reason_codes"]
                .as_array()
                .unwrap()
                .contains(&"SOC_VERDICT".into()));
            seen.insert(format!(
                "{}/{}",
                v["package"]["registry"], v["package"]["name"]
            ));
        }
    }
    assert_eq!(seen.len(), 1400, "every held version is reachable");
    // One registry narrows it.
    let (_, body) = call(
        &app,
        &format!("/api/v1/admin/verdicts?registry={SEC}&reason=SOC_VERDICT&per_page=1"),
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(body["total"], 700, "{body}");
}

/// Without a registry, the listing covers only the protected registries the
/// caller reads: a sealed one is absent, not counted.
#[actix_web::test]
async fn the_cross_registry_listing_leaves_a_sealed_registry_out() {
    let (app, lab) = app(&[Action::QuarantineRead]).await;
    lab.hold(
        SEALED,
        "hidden",
        VerdictState::Denied,
        &[ReasonCode::SocVerdict],
    )
    .await;
    lab.hold(
        SEC,
        "shown",
        VerdictState::Denied,
        &[ReasonCode::SocVerdict],
    )
    .await;
    let (status, body) = call(
        &app,
        "/api/v1/admin/verdicts?reason=SOC_VERDICT",
        USER_TOKEN,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["total"], 1, "{body}");
    assert!(!body.to_string().contains(SEALED), "{body}");
}

/// On a `[security]` registry the builder takes `cve_gate` and
/// `license_gate` out of the chain and runs them as internal scanners: the
/// coverage still reads them as configured.
#[actix_web::test]
async fn gates_run_as_scanners_still_count_as_coverage() {
    use batlehub_core::ports::ArtifactScanner;
    use batlehub_core::services::{RecordedVulnerabilityScanner, RuleAsScanner};
    let (app, lab) = app(&[]).await;
    let uri = format!("/api/v1/admin/security/coverage/{SEC}");
    let (_, row) = call(&app, &uri, ADMIN_TOKEN).await;
    assert_eq!(
        (&row["cve_gate"], &row["license_gate"]),
        (&Value::Bool(false), &Value::Bool(false))
    );
    // The policy's chain is the builder's for a protected registry: no gate
    // rule in it (`rbac_policy`), both gates among the internal scanners.
    let license = RuleAsScanner::for_rule(Box::new(batlehub_core::rules::LicenseGateRule::new(
        batlehub_adapters::in_memory::InMemorySbomRepository::new(),
        Vec::new(),
        Vec::new(),
        true,
        Vec::new(),
        true,
    )))
    .unwrap();
    let scanners: Vec<Arc<dyn ArtifactScanner>> = vec![
        Arc::new(RecordedVulnerabilityScanner {
            repo: Arc::new(batlehub_adapters::in_memory::InMemoryVulnerabilityRepository::new()),
        }),
        Arc::new(license),
    ];
    lab.hot
        .write()
        .await
        .internal_scanners
        .insert(SEC.to_owned(), scanners);
    let (_, row) = call(&app, &uri, ADMIN_TOKEN).await;
    assert_eq!(row["cve_gate"], true, "{row}");
    assert_eq!(row["license_gate"], true, "{row}");
}

fn soc_flag(registry: &str, version: &str) -> batlehub_core::entities::PackageFlag {
    let at = Utc::now() - chrono::Duration::hours(1);
    batlehub_core::entities::PackageFlag {
        id: uuid::Uuid::new_v4(),
        source: "soc".into(),
        external_id: format!("{registry}/{version}"),
        registry: registry.into(),
        package_name: "evil".into(),
        version: version.into(),
        kind: batlehub_core::entities::FlagKind::Malware,
        effect: batlehub_core::entities::FlagEffect::Inform,
        severity: None,
        summary: "steals tokens".into(),
        url: None,
        first_seen: at,
        updated_at: at,
        expires_at: None,
        revoked_at: None,
    }
}

/// A pull after a flag is one decision per version with every consumer's
/// pulls summed, and only while the flag is live.
#[actix_web::test]
async fn pulls_after_a_flag_sum_consumers_and_end_with_the_flag() {
    use batlehub_core::entities::AccessEvent;
    use batlehub_core::ports::AdvisoryRepository;
    let (app, lab) = app(&[]).await;
    let flag = soc_flag(PLAIN, "1.0.0");
    lab.advisories.upsert_flag(flag.clone()).await.unwrap();
    for user in ["alice", "bob", "bob"] {
        lab.repo
            .record_access(AccessEvent::allowed_download(
                PackageId::new(PLAIN, "evil", "1.0.0"),
                Some(user.to_owned()),
                Role::User,
            ))
            .await
            .unwrap();
    }
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(body["flags"]["pulled_after_flag"], 1, "{body}");
    let pulled: Vec<&Value> = body["decisions"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"] == "pulled_after_flag")
        .collect();
    assert_eq!(pulled.len(), 1, "{body}");
    assert_eq!(pulled[0]["pulls_after"], 3, "{body}");
    // Revoked, the flag asks for nothing any more.
    lab.advisories
        .revoke_flag("soc", &flag.external_id, Utc::now())
        .await
        .unwrap();
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(body["flags"]["pulled_after_flag"], 0, "{body}");
    assert_eq!(body["decisions"]["total"], 0, "{body}");
}

/// Hard blocks come before exemptions, and exemptions soonest-ending first:
/// an exemption's `at` is its future end, so newest-first put them on top.
#[actix_web::test]
async fn decisions_are_ordered_by_urgency() {
    let policies = batlehub_adapters::in_memory::InMemoryPolicyRepository::new();
    for (pkg, days) in [("late", 6), ("soon", 1)] {
        policies
            .put_policy(batlehub_core::ports::StoredPolicy {
                registry: SEC.to_owned(),
                node_kind: batlehub_core::ports::NodeKind::Version,
                node_key: batlehub_core::ports::version_node_key(pkg, "1.0.0"),
                visibility: None,
                prerelease_visibility: None,
                versioning: None,
                quota: None,
                rules: vec![batlehub_core::entities::RuleOverride {
                    gate: "security_verdict".to_owned(),
                    settings: serde_json::json!({
                        "exempt": true, "gate": "security_verdict", "reason": "test",
                        "exempt_until": Utc::now() + chrono::Duration::days(days),
                    }),
                }],
                set_by: None,
            })
            .await
            .unwrap();
    }
    let (app, lab) = app_with(&[], ConfigureAppDefaults::default(), Some(policies)).await;
    lab.hold(SEC, "evil", VerdictState::Denied, &[ReasonCode::SocVerdict])
        .await;
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    let names: Vec<&str> = body["decisions"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["evil", "soon", "late"], "{body}");
    assert_eq!(body["decisions"]["total"], 3);
}

/// A source that failed is named: *Needs a decision* is known incomplete.
#[actix_web::test]
async fn a_failed_source_is_named_in_the_decisions() {
    let (app, lab) = app(&[]).await;
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert!(body["decisions"].get("unknown").is_none(), "{body}");
    {
        let mut hot = lab.hot.write().await;
        hot.verdicts = None;
        hot.scan_queue = None;
    }
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    assert_eq!(
        body["decisions"]["unknown"],
        serde_json::json!(["holds", "scanners"]),
        "{body}"
    );
}

/// "All N registries are protected" counts the registries that serve
/// upstream, not the local ones.
#[actix_web::test]
async fn coverage_counts_the_registries_serving_upstream() {
    let (app, lab) = app(&[]).await;
    lab.modes.insert(
        PLAIN.to_owned(),
        batlehub_config::schema::RegistryMode::Local,
    );
    let (_, body) = call(&app, OVERVIEW, ADMIN_TOKEN).await;
    let cov = &body["coverage"];
    assert_eq!(
        (&cov["registries"], &cov["upstream"]),
        (&serde_json::json!(3), &serde_json::json!(2)),
        "{cov}"
    );
    assert_eq!(cov["state"], "ok", "{cov}");
}

/// A registry added later does not blank the others' past (its backfilled
/// rows have no state), and a day missing a started registry's row is a gap;
/// `history_starts` skips the backfill's all-null rows.
#[actix_web::test]
async fn trends_count_a_registry_from_its_first_figure() {
    use batlehub_core::ports::DailyEvents;
    let (app, lab) = app(&[]).await;
    let today = Utc::now().date_naive();
    let d = |n: i64| today - chrono::Duration::days(n);
    let denied = |n| DailyState {
        denied: n,
        ..Default::default()
    };
    for n in [0, 1, 3] {
        lab.daily.upsert_state(SEC, d(n), &denied(2)).await.unwrap();
    }
    // SEALED is new today; the backfill wrote it empty rows before that.
    lab.daily
        .upsert_state(SEALED, d(0), &denied(5))
        .await
        .unwrap();
    for n in [1, 2, 3, 10] {
        lab.daily
            .upsert_events(SEALED, d(n), &DailyEvents::default())
            .await
            .unwrap();
    }
    let (_, body) = call(&app, "/api/v1/admin/security/trends?window=7d", ADMIN_TOKEN).await;
    let days = body["days"].as_array().unwrap();
    assert_eq!(days[6]["denied"], 7, "{body}");
    assert_eq!(
        days[5]["denied"], 2,
        "the new registry did not exist: {body}"
    );
    assert!(
        days[4]["denied"].is_null(),
        "SEC has no row that day: {body}"
    );
    assert_eq!(days[3]["denied"], 2, "{body}");
    assert_eq!(body["history_starts"], serde_json::json!(d(3)), "{body}");
}

// ── the daily rollup (§5.2) ──────────────────────────────────────────────────

mod rollup {
    use super::*;
    use batlehub_core::ports::DailyEvents;
    use batlehub_core::services::{RollupOutcome, SecurityRollupService};
    use chrono::{Duration, NaiveDate, TimeZone};

    fn service(daily: Arc<InMemorySecurityDaily>, retention_days: u32) -> SecurityRollupService {
        let registries: HashMap<String, Arc<dyn RegistryClient>> = [SEC, PLAIN]
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    FixedRegistry::new("npm") as Arc<dyn RegistryClient>,
                )
            })
            .collect();
        SecurityRollupService {
            repo: daily,
            leader: InMemoryScanQueue::new(),
            hot: new_hot_lock(HotConfig {
                registries,
                ..Default::default()
            }),
            retention_days,
        }
    }

    fn at(day: NaiveDate) -> chrono::DateTime<Utc> {
        Utc.from_utc_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
    }

    async fn rows(
        daily: &InMemorySecurityDaily,
        day: NaiveDate,
    ) -> Vec<batlehub_core::ports::SecurityDailyRow> {
        daily.range(day, day).await.unwrap()
    }

    /// Two runs at the same instant leave the same rows (T8's idempotence).
    #[actix_web::test]
    async fn a_run_is_idempotent() {
        let daily = InMemorySecurityDaily::new();
        *daily.state.write().await = vec![(
            SEC.into(),
            DailyState {
                denied: 2,
                ..Default::default()
            },
        )];
        // A three-day window: one gap, filled by the first run.
        let svc = service(daily.clone(), 3);
        let now = Utc::now();
        svc.run_once(now).await.unwrap();
        let first = daily
            .range(now.date_naive() - Duration::days(40), now.date_naive())
            .await
            .unwrap();
        svc.run_once(now).await.unwrap();
        let second = daily
            .range(now.date_naive() - Duration::days(40), now.date_naive())
            .await
            .unwrap();
        assert_eq!(first, second);
    }

    /// The run just after midnight does not rewrite yesterday's state.
    #[actix_web::test]
    async fn a_past_days_state_does_not_drift() {
        let daily = InMemorySecurityDaily::new();
        let svc = service(daily.clone(), 30);
        let d = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        *daily.state.write().await = vec![(
            SEC.into(),
            DailyState {
                denied: 2,
                ..Default::default()
            },
        )];
        svc.run_once(at(d)).await.unwrap();
        *daily.state.write().await = vec![(
            SEC.into(),
            DailyState {
                denied: 9,
                ..Default::default()
            },
        )];
        svc.run_once(at(d + Duration::days(1))).await.unwrap();
        let sec = |rows: Vec<batlehub_core::ports::SecurityDailyRow>| {
            rows.into_iter()
                .find(|r| r.registry == SEC)
                .unwrap()
                .state
                .unwrap()
                .denied
        };
        assert_eq!(sec(rows(&daily, d).await), 2);
        assert_eq!(sec(rows(&daily, d + Duration::days(1)).await), 9);
    }

    /// The gap fill: 14 rows a run, oldest first, NULL beyond the sources'
    /// retention and never zero, resumed by the next run.
    #[actix_web::test]
    async fn the_gap_fill_writes_null_beyond_retention_and_resumes() {
        let daily = InMemorySecurityDaily::new();
        let today = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
        *daily.held_from.write().await = Some(today - Duration::days(3));
        let svc = service(daily.clone(), 10);
        // Window: today-9 … today; yesterday and today are written directly,
        // 8 days × 2 registries = 16 missing rows, 14 the first run.
        match svc.run_once(at(today)).await.unwrap() {
            RollupOutcome::Ran { filled, .. } => assert_eq!(filled, 14),
            other => panic!("{other:?}"),
        }
        let oldest = rows(&daily, today - Duration::days(9)).await;
        assert_eq!(oldest.len(), 2);
        assert!(
            oldest.iter().all(|r| r.events == DailyEvents::default()),
            "beyond retention is NULL"
        );
        let held = rows(&daily, today - Duration::days(3)).await;
        assert!(
            held.iter().all(|r| r.events.refusals == Some(0)),
            "inside retention is a count"
        );
        // The last missing day is filled by the next run.
        assert!(rows(&daily, today - Duration::days(2)).await.len() < 2);
        match svc.run_once(at(today)).await.unwrap() {
            RollupOutcome::Ran { filled, .. } => assert_eq!(filled, 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(rows(&daily, today - Duration::days(2)).await.len(), 2);
        // Pruning keeps exactly the window.
        svc.run_once(at(today + Duration::days(5))).await.unwrap();
        assert!(rows(&daily, today - Duration::days(9)).await.is_empty());
    }
    /// A state read that fails does not stall the backfill or the prune.
    struct NoState(Arc<InMemorySecurityDaily>);

    #[async_trait::async_trait]
    impl SecurityDailyRepository for NoState {
        async fn compute_state(
            &self,
        ) -> Result<Vec<(String, DailyState)>, batlehub_core::error::CoreError> {
            Err(batlehub_core::error::CoreError::Other(anyhow::anyhow!(
                "statement timeout"
            )))
        }
        async fn compute_events(
            &self,
            day: NaiveDate,
            registries: &[String],
        ) -> Result<Vec<DailyEvents>, batlehub_core::error::CoreError> {
            self.0.compute_events(day, registries).await
        }
        async fn upsert_events(
            &self,
            registry: &str,
            day: NaiveDate,
            events: &DailyEvents,
        ) -> Result<(), batlehub_core::error::CoreError> {
            self.0.upsert_events(registry, day, events).await
        }
        async fn upsert_state(
            &self,
            registry: &str,
            day: NaiveDate,
            state: &DailyState,
        ) -> Result<(), batlehub_core::error::CoreError> {
            self.0.upsert_state(registry, day, state).await
        }
        async fn range(
            &self,
            from: NaiveDate,
            to: NaiveDate,
        ) -> Result<Vec<batlehub_core::ports::SecurityDailyRow>, batlehub_core::error::CoreError>
        {
            self.0.range(from, to).await
        }
        async fn prune_before(
            &self,
            day: NaiveDate,
        ) -> Result<u64, batlehub_core::error::CoreError> {
            self.0.prune_before(day).await
        }
    }

    #[actix_web::test]
    async fn one_failed_step_does_not_stall_the_others() {
        let daily = InMemorySecurityDaily::new();
        let today = NaiveDate::from_ymd_opt(2026, 11, 17).unwrap();
        let old = today - Duration::days(20);
        daily
            .upsert_events(SEC, old, &DailyEvents::default())
            .await
            .unwrap();
        let mut svc = service(daily.clone(), 5);
        svc.repo = Arc::new(NoState(daily.clone()));
        assert!(
            svc.run_once(at(today)).await.is_err(),
            "the failure is reported"
        );
        // The events of today, the gaps and the prune all ran.
        assert_eq!(rows(&daily, today).await.len(), 2);
        assert_eq!(rows(&daily, today - Duration::days(4)).await.len(), 2);
        assert!(rows(&daily, old).await.is_empty());
    }

    /// A recompute of a partly purged day never lowers the stored count.
    #[actix_web::test]
    async fn a_stored_count_never_shrinks() {
        let daily = InMemorySecurityDaily::new();
        let day = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        for n in [5, 2] {
            daily
                .upsert_events(
                    SEC,
                    day,
                    &DailyEvents {
                        refusals: Some(n),
                        refusals_by_code: Some([("VULNERABILITY".to_owned(), n)].into()),
                        jobs_done: Some(n),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }
        let row = rows(&daily, day).await.remove(0);
        assert_eq!(row.events.refusals, Some(5));
        assert_eq!(row.events.refusals_by_code.unwrap()["VULNERABILITY"], 5);
        assert_eq!(row.events.jobs_done, Some(5));
    }
}
