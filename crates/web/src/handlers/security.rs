//! The verdict on the wire (RFC 0018 §4.2, phase 2): what a refused or
//! warned artifact request answers with, and the endpoint `batlehub why`
//! and `batlehub wait` read.
//!
//! # A hold is answered in the registry's own language
//!
//! There is no uniform way to say "this version must not be selected"
//! (§4.4): npm prints `{"error": …}`, cargo prints whatever body it got under
//! its own status line, `go` and `mvn` never read the body at all. So one
//! message — [`Verdict::short_message`], the line that names the coordinate,
//! the state, the codes and `batlehub why` — is put into each registry's
//! native error shape by [`native_body`], and the structured facts travel in
//! headers every client can be made to log:
//!
//! | Header                    | Value                                    |
//! | ------------------------- | ---------------------------------------- |
//! | `X-BatleHub-Verdict`      | `quarantined` / `denied` / `warned`      |
//! | `X-BatleHub-Reason`       | the reason codes, comma-separated        |
//! | `X-BatleHub-Available-At` | RFC 3339, on a time-bound hold           |
//! | `X-BatleHub-Details`      | the short message                        |
//! | `Retry-After`             | seconds, when waiting can help           |
//!
//! # Who sees what
//!
//! A caller without `quarantine:read` gets the registry's native *not found*
//! — a held version answers exactly like a missing one, so quarantined
//! versions cannot be enumerated (§7). Two kinds are the exception and get a
//! `403` regardless: with `GOPROXY=<proxy>,direct` a `404` sends `go` to the
//! origin behind the proxy's back, and `mvn` treats a missing pom as absent
//! and fails later on the jar with a message that names the wrong problem
//! (§4.4, decisions 32 and the Maven row). With `quarantine:read` the answer
//! is the `403` above; with `findings:read` the body also carries a findings
//! summary line.

use std::sync::Arc;

use actix_web::{get, post, web, HttpResponse, HttpResponseBuilder, Responder};
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_core::{
    entities::{Action, Identity, PackageId, RegistryKind, ScanTrigger, Verdict, VerdictState},
    services::{authz, ProxyService},
};

use crate::{error::AppError, extractors::AuthIdentity};

pub const HEADER_VERDICT: &str = "X-BatleHub-Verdict";
pub const HEADER_REASON: &str = "X-BatleHub-Reason";
pub const HEADER_AVAILABLE_AT: &str = "X-BatleHub-Available-At";
pub const HEADER_DETAILS: &str = "X-BatleHub-Details";

/// What a caller may learn about a hold (§4.2 *Direct artifact request*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldVisibility {
    /// Neither permission: the native not-found, or a bare `403` on the
    /// two kinds a `404` misleads.
    Hidden,
    /// `quarantine:read`: the `403`, the headers, the message.
    Reason,
    /// `findings:read` too: the same, plus a findings summary line.
    Findings,
}

/// Put the verdict's facts on a response, whatever its status.
pub fn verdict_headers(
    builder: &mut HttpResponseBuilder,
    verdict: &Verdict,
    now: chrono::DateTime<chrono::Utc>,
) {
    builder.insert_header((HEADER_VERDICT, verdict.state.as_str()));
    let codes = verdict
        .reason_codes
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    builder.insert_header((HEADER_REASON, codes));
    if let Some(at) = verdict.available_at {
        builder.insert_header((
            HEADER_AVAILABLE_AT,
            at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ));
    }
    builder.insert_header((HEADER_DETAILS, verdict.short_message()));
    if let Some(secs) = verdict.retry_after_secs(now) {
        builder.insert_header(("Retry-After", secs.to_string()));
    }
}

/// `message` in the error shape `kind`'s client surfaces (§4.2 *Native
/// error bodies*): `(content type, body)`.
pub fn native_body(kind: RegistryKind, message: &str) -> (&'static str, String) {
    const JSON: &str = "application/json";
    const TEXT: &str = "text/plain; charset=utf-8";
    match kind {
        RegistryKind::Npm | RegistryKind::Openvsx | RegistryKind::VscodeMarketplace => {
            (JSON, serde_json::json!({ "error": message }).to_string())
        }
        RegistryKind::Cargo => (
            JSON,
            serde_json::json!({ "errors": [{ "detail": message }] }).to_string(),
        ),
        RegistryKind::Composer => (
            JSON,
            serde_json::json!({ "status": "error", "message": message }).to_string(),
        ),
        RegistryKind::Terraform => (JSON, serde_json::json!({ "errors": [message] }).to_string()),
        RegistryKind::Github
        | RegistryKind::Gitlab
        | RegistryKind::Forgejo
        | RegistryKind::JetbrainsMarketplace => {
            (JSON, serde_json::json!({ "message": message }).to_string())
        }
        // gem prints the body; pip, conda, dotnet, go, mvn, apt and dnf read
        // the status and its reason phrase and nothing else, so text is the
        // honest shape — it is what a person sees with `curl`.
        RegistryKind::Rubygems
        | RegistryKind::Pypi
        | RegistryKind::Conda
        | RegistryKind::Nuget
        | RegistryKind::Goproxy
        | RegistryKind::Maven
        | RegistryKind::Deb
        | RegistryKind::Rpm
        | RegistryKind::Pacman
        | RegistryKind::Jetbrains
        | RegistryKind::Generic
        | RegistryKind::Nodedist
        | RegistryKind::Sdkman => (TEXT, format!("{message}\n")),
    }
}

/// Whether a hold on `kind` must be a `403` even for a caller who may not
/// learn why (§4.4): a `404` makes `go` fall through to `direct` and makes
/// `mvn` go on to the jar.
pub fn hold_is_always_403(kind: RegistryKind) -> bool {
    matches!(kind, RegistryKind::Goproxy | RegistryKind::Maven)
}

/// The response for a refused artifact request under `verdict`.
pub fn hold_response(
    kind: RegistryKind,
    verdict: &Verdict,
    visibility: HoldVisibility,
    now: chrono::DateTime<chrono::Utc>,
) -> HttpResponse {
    let coord = format!(
        "{}:{}@{}",
        verdict.package.registry, verdict.package.name, verdict.package.version
    );
    match visibility {
        HoldVisibility::Hidden => {
            if hold_is_always_403(kind) {
                let (ct, body) = native_body(kind, &format!("{coord} is not available"));
                HttpResponse::Forbidden().content_type(ct).body(body)
            } else {
                let (ct, body) = native_body(kind, &format!("{coord} not found"));
                HttpResponse::NotFound().content_type(ct).body(body)
            }
        }
        HoldVisibility::Reason | HoldVisibility::Findings => {
            let mut message = verdict.short_message();
            if visibility == HoldVisibility::Findings && !verdict.findings.is_empty() {
                let summary = verdict
                    .findings
                    .iter()
                    .map(|f| {
                        format!(
                            "{}: {} {} {}{}",
                            f.scanner,
                            f.code,
                            f.severity.as_str(),
                            f.summary,
                            f.reference
                                .as_deref()
                                .map(|r| format!(" ({r})"))
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                message.push_str(" Findings: ");
                message.push_str(&summary);
            }
            let (ct, body) = native_body(kind, &message);
            let mut builder = HttpResponse::Forbidden();
            builder.content_type(ct);
            verdict_headers(&mut builder, verdict, now);
            builder.body(body)
        }
    }
}

/// What `identity` may learn about a hold on `pkg`, from the grants alone.
///
/// Grants, not the rule chain: the chain is what produced the verdict, and
/// asking it again would run the gate on the gate.
pub async fn hold_visibility(
    hot: &batlehub_core::services::hot_config::HotConfigLock,
    pkg: &PackageId,
    identity: &Identity,
) -> HoldVisibility {
    if authz::authorize_grants_public(hot, pkg, identity, Action::QuarantineRead)
        .await
        .is_err()
    {
        return HoldVisibility::Hidden;
    }
    if authz::authorize_grants_public(hot, pkg, identity, Action::FindingsRead)
        .await
        .is_ok()
    {
        HoldVisibility::Findings
    } else {
        HoldVisibility::Reason
    }
}

/// The registry's kind, for the body shape. `Generic` when the registry is
/// unknown, which cannot happen after a handler's `require_registry_type`.
pub async fn registry_kind(svc: &ProxyService, registry: &str) -> RegistryKind {
    let hot = svc.hot.read().await;
    hot.registries
        .get(registry)
        .and_then(|c| c.registry_type().parse().ok())
        .unwrap_or(RegistryKind::Generic)
}

/// A `ProxyResponse::Denied` that carries a verdict, answered per §4.2.
pub async fn hold_http_response(
    svc: &ProxyService,
    pkg: &PackageId,
    identity: &Identity,
    verdict: &Verdict,
) -> HttpResponse {
    let kind = registry_kind(svc, &pkg.registry).await;
    let visibility = hold_visibility(&svc.hot, pkg, identity).await;
    metrics::counter!(
        "batlehub_verdict_denials_served_total",
        "registry" => pkg.registry.clone(),
        "code" => "response",
        "status" => match (visibility, hold_is_always_403(kind)) {
            (HoldVisibility::Hidden, false) => "404",
            _ => "403",
        },
    )
    .increment(1);
    hold_response(kind, verdict, visibility, chrono::Utc::now())
}

// ── the endpoint ─────────────────────────────────────────────────────────────

/// A verdict as the endpoint serves it: the stored row, findings only for a
/// caller with `findings:read`.
#[derive(Debug, Serialize, ToSchema)]
pub struct VerdictResponse {
    #[serde(flatten)]
    pub verdict: Verdict,
    /// Whether `findings` was withheld for lack of `findings:read`, so a
    /// client can tell "no findings" from "not allowed to see them".
    pub findings_withheld: bool,
    /// RFC 0019 phase 2 — on a forge, what the ref in the request resolved
    /// to. `batlehub why github:cli/cli@main` asks about a branch; the
    /// verdict is keyed on the commit, and these three fields are how the
    /// answer says which one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_commit: Option<String>,
}

/// The current verdict for one version (RFC 0018 §4.2 *Verdict endpoint*).
///
/// Re-judged against the clock when the version's metadata is cached — the
/// case for any version a refused request just resolved — so a time-bound
/// hold whose clock has run out answers as served rather than as the stale
/// row. A version this instance has never looked at answers `404`, as does a
/// caller without `quarantine:read`: a held version cannot be enumerated.
#[utoipa::path(
    get,
    path = "/api/v1/verdicts/{registry}/{name}/{version}",
    tag = "security",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "Package name"),
        ("version" = String, Path, description = "Version"),
    ),
    responses(
        (status = 200, description = "The verdict; `findings` present only with findings:read", body = VerdictResponse),
        (status = 404, description = "No verdict, no security profile on the registry, or no quarantine:read"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/verdicts/{registry}/{name}/{version}")]
pub async fn get_verdict(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<impl Responder, AppError> {
    let (registry, name, version) = path.into_inner();
    let requested = PackageId::new(&registry, &name, &version);
    batlehub_core::services::validate_coordinate(&name, &version, None).map_err(AppError::from)?;

    // Gate on the coordinate as asked *before* resolving it, the way the proxy
    // read path does (`ProxyService::handle`, RFC 0019 §6.1): resolution spends
    // the shared rate-limit budget and sends the operator's forge credential
    // upstream, and neither should happen for a caller who may not read the
    // answer. The resolved coordinate is checked again below — this only
    // decides whether the resolution is worth doing.
    if hold_visibility(&svc.hot, &requested, &identity.0).await == HoldVisibility::Hidden {
        return Err(AppError::not_found(format!(
            "no verdict for {registry}:{name}@{version}"
        )));
    }

    // RFC 0019 phase 2: on a forge the caller names a ref and the verdict is
    // keyed on the commit. Resolving here is what lets `batlehub why
    // github:cli/cli@main` answer at all — and it is the same resolution the
    // read path would do, through the same cache, so it costs a forge call
    // only when the ref's TTL has run out.
    let resolution = resolve_ref_for(&svc, &requested).await;
    let pkg = match &resolution {
        Some(r) => PackageId::new(&registry, &name, &r.sha),
        None => requested.clone(),
    };

    let visibility = hold_visibility(&svc.hot, &pkg, &identity.0).await;
    if visibility == HoldVisibility::Hidden {
        return Err(AppError::not_found(format!(
            "no verdict for {registry}:{name}@{version}"
        )));
    }
    let (policy, verdicts, queue) = {
        let hot = svc.hot.read().await;
        (
            hot.security.get(&registry).cloned(),
            hot.verdicts.clone(),
            hot.scan_queue.clone(),
        )
    };
    let (Some(policy), Some(verdicts), Some(queue)) = (policy, verdicts, queue) else {
        return Err(AppError::not_found(format!(
            "registry '{registry}' has no security profile"
        )));
    };
    let service = batlehub_core::services::VerdictService::new(verdicts, queue);

    // The clock: with the metadata at hand the verdict is re-derived, which
    // is what a refused request did a moment ago and what the next one will
    // do. Without it, the stored row is the answer.
    let verdict = match svc.cached_metadata_for(&pkg).await {
        Some(meta) => service
            .current(&meta, &policy, chrono::Utc::now())
            .await
            .map_err(AppError::from)?,
        None => match service.verdicts.get(&pkg).await.map_err(AppError::from)? {
            Some(v) => v,
            None => {
                return Err(AppError::not_found(format!(
                    "no verdict for {registry}:{name}@{version}"
                )))
            }
        },
    };
    let findings_withheld = visibility != HoldVisibility::Findings;
    let mut verdict = verdict;
    if findings_withheld {
        verdict.findings.clear();
    }
    Ok(web::Json(VerdictResponse {
        verdict,
        findings_withheld,
        requested_ref: resolution.as_ref().map(|r| r.requested.clone()),
        ref_kind: resolution.as_ref().map(|r| r.kind.as_str().to_owned()),
        resolved_commit: resolution.as_ref().map(|r| r.sha.clone()),
    }))
}

/// What a rescan request did.
#[derive(Debug, Serialize, ToSchema)]
pub struct RescanResponse {
    /// `false` when an open job for the coordinate already existed.
    pub queued: bool,
    pub trigger: String,
}

/// Queue a rescan of one version (RFC 0018 §4.2): `gates:exempt`, which
/// `role:admin` holds, on the registry.
#[utoipa::path(
    post,
    path = "/api/v1/verdicts/{registry}/{name}/{version}/rescan",
    tag = "security",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "Package name"),
        ("version" = String, Path, description = "Version"),
    ),
    responses(
        (status = 202, description = "A rescan job is queued (or was already open)", body = RescanResponse),
        (status = 403, description = "Requires gates:exempt on the registry"),
        (status = 404, description = "The registry has no security profile"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/verdicts/{registry}/{name}/{version}/rescan")]
pub async fn rescan_verdict(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<impl Responder, AppError> {
    let (registry, name, version) = path.into_inner();
    batlehub_core::services::validate_coordinate(&name, &version, None).map_err(AppError::from)?;
    let requested = PackageId::new(&registry, &name, &version);
    // The same grant the exemption endpoints read: `gates:exempt` resolved
    // against the coordinate, which `role:admin` holds everywhere.
    //
    // Checked on the coordinate as asked before it is resolved, then again on
    // the resolved one: resolution spends the shared rate-limit budget and
    // sends the operator's forge credential upstream, which a caller without
    // the grant must not be able to trigger. Same ordering as the proxy read
    // path (`ProxyService::handle`).
    let require_grant = |pkg: &PackageId| {
        let pkg = pkg.clone();
        let svc = svc.clone();
        let identity = identity.0.clone();
        async move {
            authz::authorize_grants_public(&svc.hot, &pkg, &identity, Action::GatesExempt)
                .await
                .map_err(|_| {
                    AppError::forbidden("this endpoint requires the 'gates:exempt' permission")
                })
        }
    };
    require_grant(&requested).await?;
    // The queue is keyed on the commit, as the verdict is (RFC 0019 phase 2).
    let pkg = match resolve_ref_for(&svc, &requested).await {
        Some(r) => PackageId::new(&registry, &name, &r.sha),
        None => requested,
    };
    require_grant(&pkg).await?;
    let (has_profile, queue) = {
        let hot = svc.hot.read().await;
        (hot.security.contains_key(&registry), hot.scan_queue.clone())
    };
    let (true, Some(queue)) = (has_profile, queue) else {
        return Err(AppError::not_found(format!(
            "registry '{registry}' has no security profile"
        )));
    };
    let published_at = svc
        .cached_metadata_for(&pkg)
        .await
        .and_then(|m| m.published_at);
    let queued = queue
        .enqueue(&pkg, published_at, ScanTrigger::Rescan)
        .await
        .map_err(AppError::from)?;
    tracing::info!(
        package = %pkg,
        by = ?identity.0.user_id,
        queued,
        "security: rescan requested"
    );
    Ok(HttpResponse::Accepted().json(RescanResponse {
        queued,
        trigger: ScanTrigger::Rescan.as_str().to_owned(),
    }))
}

/// `?since=30d&format=csv` on the pullers report.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct PullersQuery {
    /// A window back from now — `30d`, `12h`, `90m`, `3600s`, or a bare
    /// number of seconds — or an RFC 3339 instant. Default: the registry's
    /// `pullers_window_days`.
    pub since: Option<String>,
    /// `json` (default) or `csv`.
    pub format: Option<String>,
}

/// Who pulled one version (RFC 0018 §4.2 *Who pulled what*).
#[derive(Debug, Serialize, ToSchema)]
pub struct PullersResponse {
    pub registry: String,
    pub package_name: String,
    pub version: String,
    /// The start of the window the rows cover.
    pub since: chrono::DateTime<chrono::Utc>,
    pub pullers: Vec<batlehub_core::services::Puller>,
}

/// `30d` / `12h` / `90m` / `3600s` / `3600` back from `now`, or RFC 3339.
///
/// `pub(crate)` because the identity-scoped `audit pulls` reader takes the same
/// windows and must reject the same strings the same way. Two copies of a
/// parser whose 400s are part of the API is two places for them to diverge.
pub(crate) fn parse_since(
    raw: Option<&str>,
    default_window: std::time::Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<chrono::DateTime<chrono::Utc>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(now - chrono::Duration::from_std(default_window).unwrap_or_default());
    };
    if let Ok(at) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok(at.with_timezone(&chrono::Utc));
    }
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (num, unit) = raw.split_at(split);
    let n: i64 = num.parse().map_err(|_| {
        AppError::bad_request(format!(
            "since '{raw}' is not a window (30d, 12h, 90m, 3600s) or an RFC 3339 instant"
        ))
    })?;
    let secs = match unit.trim() {
        "" | "s" => Some(n),
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        "d" => n.checked_mul(86_400),
        other => {
            return Err(AppError::bad_request(format!(
                "since: unknown unit '{other}' (s, m, h, d)"
            )))
        }
    };
    // `checked_mul` and `try_seconds`, because both overflow on a
    // caller-supplied integer: `Duration::seconds` *panics* above ~9.2e15
    // seconds, so `?since=9999999999999999` took the request down with no
    // response at all instead of answering the 400 this function documents.
    let window = secs
        .and_then(chrono::Duration::try_seconds)
        .ok_or_else(|| AppError::bad_request(format!("since '{raw}' is too large a window")))?;
    now.checked_sub_signed(window)
        .ok_or_else(|| AppError::bad_request(format!("since '{raw}' is too large a window")))
}

/// Who pulled this version inside a window — the incident question,
/// answered from `access_events` through the same query the flip alert
/// carried (RFC 0018 decision 28: one who-pulled query, not two).
///
/// `audit:read` on the registry: the report is a view of the access log,
/// and the same reader holds both. Exposure only — allowed downloads; a
/// refused request delivered no bytes (RFC 0002 decision 5). Anonymous
/// pulls are kept under `ip:<addr>`.
#[utoipa::path(
    get,
    path = "/api/v1/verdicts/{registry}/{name}/{version}/pullers",
    tag = "security",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "Package name"),
        ("version" = String, Path, description = "Version"),
        PullersQuery,
    ),
    responses(
        (status = 200, description = "Who pulled the version in the window; `?format=csv` selects CSV, anything else JSON", content(
            (PullersResponse = "application/json"),
            (crate::handlers::schemas::ProtocolDocument = "text/csv"),
        )),
        (status = 400, description = "An unparseable `since`"),
        (status = 403, description = "`audit:read` on the registry required"),
        (status = 404, description = "The registry has no security profile"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/verdicts/{registry}/{name}/{version}/pullers")]
pub async fn list_pullers(
    path: web::Path<(String, String, String)>,
    query: web::Query<PullersQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    admin_svc: web::Data<Arc<batlehub_core::services::AdminService>>,
) -> Result<HttpResponse, AppError> {
    let (registry, name, version) = path.into_inner();
    batlehub_core::services::validate_coordinate(&name, &version, None).map_err(AppError::from)?;
    crate::handlers::back_office::require_verb(
        &identity,
        Action::AuditRead,
        Some(&registry),
        &svc.hot,
    )
    .await?;
    let policy = svc.hot.read().await.security.get(&registry).cloned();
    let Some(policy) = policy else {
        return Err(AppError::not_found(format!(
            "registry '{registry}' has no security profile"
        )));
    };
    let now = chrono::Utc::now();
    let since = parse_since(query.since.as_deref(), policy.pullers_window, now)?;
    let requested = PackageId::new(&registry, &name, &version);
    // On a forge the pulls were logged against the commit (RFC 0019 §13.1).
    let pkg = match resolve_ref_for(&svc, &requested).await {
        Some(r) => PackageId::new(&registry, &name, &r.sha),
        None => requested,
    };
    let pullers = batlehub_core::services::pullers_for(admin_svc.repo.as_ref(), &pkg, since)
        .await
        .map_err(AppError::from)?;
    if query.format.as_deref() == Some("csv") {
        let filename = format!(
            "pullers-{}-{}-{}.csv",
            registry,
            name.replace('/', "_"),
            version
        );
        return Ok(HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                actix_web::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ))
            .body(batlehub_core::services::pullers::to_csv(&pullers)));
    }
    Ok(HttpResponse::Ok().json(PullersResponse {
        registry,
        package_name: name,
        version,
        since,
        pullers,
    }))
}

// ── the admin surface (RFC 0018 phase 5) ─────────────────────────────────────

/// `?registry=&state=&limit=` on the admin listing.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
pub struct VerdictListQuery {
    pub registry: String,
    /// `allowed`, `warned`, `quarantined` or `denied`; absent lists every
    /// state.
    pub state: Option<String>,
    /// Per state; default 100, at most 1000.
    pub limit: Option<u64>,
}

/// The admin listing: the verdicts of one registry, newest evaluation
/// first, per state (RFC 0018 §4.2 *CLI*: `batlehub verdicts list`).
#[derive(Debug, Serialize, ToSchema)]
pub struct VerdictListResponse {
    pub registry: String,
    pub items: Vec<Verdict>,
}

fn parse_states(raw: Option<&str>) -> Result<Vec<VerdictState>, AppError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(vec![
            VerdictState::Denied,
            VerdictState::Quarantined,
            VerdictState::Warned,
            VerdictState::Allowed,
        ]),
        Some(s) => s.parse::<VerdictState>().map(|st| vec![st]).map_err(|_| {
            AppError::bad_request(format!(
                "state '{s}' is not one of: allowed, warned, quarantined, denied"
            ))
        }),
    }
}

/// The verdicts of a registry, by state. `system:read`. Findings are
/// included: the reader is an administrator, and the listing exists to
/// answer "what is held, and why" in one page.
#[utoipa::path(
    get,
    path = "/api/v1/admin/verdicts",
    tag = "security",
    params(VerdictListQuery),
    responses(
        (status = 200, description = "The verdicts, newest first per state", body = VerdictListResponse),
        (status = 400, description = "An unknown `state`"),
        (status = 403, description = "`system:read` required"),
        (status = 404, description = "The registry has no security profile"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/verdicts")]
pub async fn list_verdicts(
    query: web::Query<VerdictListQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<HttpResponse, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        Action::SystemRead,
        Some(&query.registry),
        &svc.hot,
    )
    .await?;
    let verdicts = {
        let hot = svc.hot.read().await;
        if !hot.security.contains_key(&query.registry) {
            return Err(AppError::not_found(format!(
                "registry '{}' has no security profile",
                query.registry
            )));
        }
        hot.verdicts.clone()
    };
    let Some(verdicts) = verdicts else {
        return Err(AppError::not_found("no verdict store in this process"));
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let mut items = Vec::new();
    for state in parse_states(query.state.as_deref())? {
        items.extend(
            verdicts
                .list_by_state(&query.registry, state, limit)
                .await
                .map_err(AppError::from)?,
        );
    }
    Ok(HttpResponse::Ok().json(VerdictListResponse {
        registry: query.registry.clone(),
        items,
    }))
}

/// A bulk rescan or a backfill.
#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct BulkScanRequest {
    pub registry: String,
    /// Rescan only the verdicts in this state; absent rescans every
    /// verdict of the registry. Ignored by `backfill`.
    #[serde(default)]
    pub state: Option<String>,
}

/// What a bulk operation queued.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkScanResponse {
    pub registry: String,
    /// Coordinates considered.
    pub considered: usize,
    /// Jobs created; a coordinate with an open job already is not counted.
    pub queued: usize,
    pub trigger: String,
}

/// Queue a `Rescan` for every verdict of a registry, or every verdict in
/// one state (RFC 0018 §4.2 *CLI*). `system:write`.
#[utoipa::path(
    post,
    path = "/api/v1/admin/verdicts/rescan",
    tag = "security",
    request_body = BulkScanRequest,
    responses(
        (status = 202, description = "The rescans are queued", body = BulkScanResponse),
        (status = 400, description = "An unknown `state`"),
        (status = 403, description = "`system:write` required"),
        (status = 404, description = "The registry has no security profile"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/verdicts/rescan")]
pub async fn bulk_rescan(
    body: web::Json<BulkScanRequest>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<HttpResponse, AppError> {
    let req = body.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        Action::SystemWrite,
        Some(&req.registry),
        &svc.hot,
    )
    .await?;
    let (verdicts, queue) = {
        let hot = svc.hot.read().await;
        if !hot.security.contains_key(&req.registry) {
            return Err(AppError::not_found(format!(
                "registry '{}' has no security profile",
                req.registry
            )));
        }
        (hot.verdicts.clone(), hot.scan_queue.clone())
    };
    let (Some(verdicts), Some(queue)) = (verdicts, queue) else {
        return Err(AppError::not_found("no verdict store in this process"));
    };
    let mut considered = 0usize;
    let mut queued = 0usize;
    for state in parse_states(req.state.as_deref())? {
        for v in verdicts
            .list_by_state(&req.registry, state, 1000)
            .await
            .map_err(AppError::from)?
        {
            considered += 1;
            let published_at = svc
                .cached_metadata_for(&v.package)
                .await
                .and_then(|m| m.published_at);
            if queue
                .enqueue(&v.package, published_at, ScanTrigger::Rescan)
                .await
                .map_err(AppError::from)?
            {
                queued += 1;
            }
        }
    }
    tracing::info!(registry = %req.registry, by = ?identity.0.user_id, considered, queued, "security: bulk rescan requested");
    Ok(HttpResponse::Accepted().json(BulkScanResponse {
        registry: req.registry,
        considered,
        queued,
        trigger: ScanTrigger::Rescan.as_str().to_owned(),
    }))
}

/// Queue a `Backfill` — the lowest priority — for every cached version of
/// a registry (RFC 0018 §4.2 *CLI*: `batlehub verdicts backfill`), so an
/// estate that turned `[security]` on with a warm cache judges what it
/// already holds without a user waiting on any of it. `system:write`.
#[utoipa::path(
    post,
    path = "/api/v1/admin/verdicts/backfill",
    tag = "security",
    request_body = BulkScanRequest,
    responses(
        (status = 202, description = "The backfill is queued", body = BulkScanResponse),
        (status = 403, description = "`system:write` required"),
        (status = 404, description = "The registry has no security profile"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/verdicts/backfill")]
pub async fn backfill_verdicts(
    body: web::Json<BulkScanRequest>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    inventory: web::Data<Arc<dyn batlehub_core::ports::ArtifactInventory>>,
) -> Result<HttpResponse, AppError> {
    let req = body.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        Action::SystemWrite,
        Some(&req.registry),
        &svc.hot,
    )
    .await?;
    let queue = {
        let hot = svc.hot.read().await;
        if !hot.security.contains_key(&req.registry) {
            return Err(AppError::not_found(format!(
                "registry '{}' has no security profile",
                req.registry
            )));
        }
        hot.scan_queue.clone()
    };
    let Some(queue) = queue else {
        return Err(AppError::not_found("no scan queue in this process"));
    };
    let cached = inventory
        .list_artifacts(&req.registry)
        .await
        .map_err(AppError::from)?;
    let mut queued = 0usize;
    let mut seen = std::collections::HashSet::new();
    for row in &cached {
        let id = PackageId::new(&req.registry, &row.package_name, &row.version);
        if !seen.insert(id.cache_key()) {
            continue;
        }
        let published_at = svc
            .cached_metadata_for(&id)
            .await
            .and_then(|m| m.published_at);
        if queue
            .enqueue(&id, published_at, ScanTrigger::Backfill)
            .await
            .map_err(AppError::from)?
        {
            queued += 1;
        }
    }
    tracing::info!(registry = %req.registry, by = ?identity.0.user_id, considered = seen.len(), queued, "security: backfill requested");
    Ok(HttpResponse::Accepted().json(BulkScanResponse {
        registry: req.registry,
        considered: seen.len(),
        queued,
        trigger: ScanTrigger::Backfill.as_str().to_owned(),
    }))
}

/// Resolve a forge ref, or `None` for a coordinate that names none — every
/// package registry, and a forge coordinate whose version is already a
/// commit.
///
/// Best-effort: a forge that cannot be reached leaves the coordinate as the
/// caller spelled it, which answers `404` rather than `502`. The endpoint's
/// job is to explain a refusal, and a second failure while explaining the
/// first is not worth a different status.
async fn resolve_ref_for(
    svc: &ProxyService,
    pkg: &PackageId,
) -> Option<batlehub_core::entities::ResolvedRef> {
    use batlehub_core::entities::{is_commit_sha, ForgeCoordinate};
    if is_commit_sha(&pkg.version) {
        return None;
    }
    let coord = ForgeCoordinate::from_package_id(pkg)?;
    let git_ref = coord.git_ref()?.to_owned();
    let (client, store, policy) = {
        let hot = svc.hot.read().await;
        let client = Arc::clone(hot.registries.get(&pkg.registry)?);
        (
            client,
            hot.ref_resolutions.clone(),
            hot.forge_refs
                .get(&pkg.registry)
                .copied()
                .unwrap_or_default(),
        )
    };
    let forge = client.forge()?;
    batlehub_core::services::forge_refs::resolve_ref(
        &pkg.registry,
        forge,
        store.as_ref(),
        policy,
        &coord.owner_repo,
        &git_ref,
    )
    .await
    .ok()
}

/// Present only so the `warned` state reads in one place: a served response
/// with these headers is what §4.2 calls "served with 200 and the same
/// headers, `X-BatleHub-Verdict: warned`".
pub fn is_warned(verdict: &Verdict) -> bool {
    verdict.state == VerdictState::Warned
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::{Finding, FindingKind, ReasonCode, Severity};
    use chrono::Utc;

    fn held() -> Verdict {
        Verdict {
            package: PackageId::new("npm-sec", "left-pad", "1.3.1"),
            state: VerdictState::Quarantined,
            reason_codes: vec![ReasonCode::MinAgeNotMet],
            findings: vec![Finding::new(
                "age",
                FindingKind::Age,
                ReasonCode::MinAgeNotMet,
                Severity::High,
                "younger than min_age",
            )],
            policy_ref: "npm-sec/default".into(),
            available_at: Some(Utc::now() + chrono::Duration::hours(1)),
            evaluated_at: Utc::now(),
            last_scanned_at: None,
            scanners_done: vec![],
        }
    }

    #[test]
    fn every_kind_has_a_native_shape_and_the_message_is_in_it() {
        for kind in RegistryKind::ALL {
            let (ct, body) = native_body(*kind, "the message");
            assert!(body.contains("the message"), "{kind}: {body}");
            assert!(
                ct.starts_with("application/json") || ct.starts_with("text/plain"),
                "{kind}: {ct}"
            );
            if ct.starts_with("application/json") {
                serde_json::from_str::<serde_json::Value>(&body).expect("valid JSON");
            }
        }
        assert_eq!(native_body(RegistryKind::Npm, "m").1, r#"{"error":"m"}"#);
        assert_eq!(
            native_body(RegistryKind::Cargo, "m").1,
            r#"{"errors":[{"detail":"m"}]}"#
        );
    }

    #[test]
    fn a_hidden_hold_is_a_native_not_found_except_where_a_404_misleads() {
        let v = held();
        let now = Utc::now();
        let resp = hold_response(RegistryKind::Npm, &v, HoldVisibility::Hidden, now);
        assert_eq!(resp.status(), 404);
        assert!(
            resp.headers().get(HEADER_VERDICT).is_none(),
            "nothing leaks"
        );
        for kind in [RegistryKind::Goproxy, RegistryKind::Maven] {
            let resp = hold_response(kind, &v, HoldVisibility::Hidden, now);
            assert_eq!(resp.status(), 403, "{kind}");
            assert!(
                resp.headers().get(HEADER_VERDICT).is_none(),
                "{kind}: nothing leaks"
            );
        }
    }

    #[test]
    fn a_visible_hold_carries_the_headers_and_retry_after() {
        let v = held();
        let resp = hold_response(RegistryKind::Npm, &v, HoldVisibility::Reason, Utc::now());
        assert_eq!(resp.status(), 403);
        let h = resp.headers();
        assert_eq!(h.get(HEADER_VERDICT).unwrap(), "quarantined");
        assert_eq!(h.get(HEADER_REASON).unwrap(), "MIN_AGE_NOT_MET");
        assert!(h.get(HEADER_AVAILABLE_AT).is_some());
        assert!(h
            .get(HEADER_DETAILS)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("batlehub why"));
        let retry: u64 = h
            .get("Retry-After")
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((3500..=3600).contains(&retry), "{retry}");
    }

    #[test]
    fn findings_read_adds_the_summary_line_and_a_denial_has_no_retry_after() {
        let mut v = held();
        v.state = VerdictState::Denied;
        v.reason_codes = vec![ReasonCode::Vulnerability];
        v.available_at = None;
        v.findings = vec![Finding::new(
            "osv",
            FindingKind::Vulnerability,
            ReasonCode::Vulnerability,
            Severity::Critical,
            "remote code execution",
        )
        .with_reference("GHSA-xxxx")];
        let resp = hold_response(
            RegistryKind::Rubygems,
            &v,
            HoldVisibility::Findings,
            Utc::now(),
        );
        assert_eq!(resp.status(), 403);
        assert!(resp.headers().get("Retry-After").is_none());
        let body = actix_web::body::to_bytes(resp.into_body());
        let body = futures::executor::block_on(body).unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains(
                "Findings: osv: VULNERABILITY critical remote code execution (GHSA-xxxx)"
            ),
            "{body}"
        );
        let resp = hold_response(
            RegistryKind::Rubygems,
            &v,
            HoldVisibility::Reason,
            Utc::now(),
        );
        let body =
            futures::executor::block_on(actix_web::body::to_bytes(resp.into_body())).unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("Findings:"));
    }
}
