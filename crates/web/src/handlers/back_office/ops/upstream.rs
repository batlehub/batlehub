//! RFC 0014 §4.6 — the upstream audit's admin surface.
//!
//! `recheck` landed with phase 4, because the heavy suite that proves a
//! notification reaches a real receiver needed a probe it could drive: a
//! sweep interval has a five-minute floor, and two of them are the least a
//! confirmation takes. The listing and the per-package status are phase 7's:
//! what stops the audit being a webhook nobody can query afterwards.
//!
//! Every handler here extracts the audit service as `Option<Data<_>>`: it is
//! registered only on a process that runs the sweep, and the others answer
//! `503` naming the reason rather than `500` naming nothing.

use std::sync::Arc;

use actix_web::{get, post, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{Action, UpstreamKey, UpstreamState, UpstreamStatus, UpstreamStatusFilter},
    services::{HotConfigLock, Transition, UpstreamAuditService},
};

use crate::{error::AppError, extractors::AuthIdentity};

/// The audit service, or the `503` a process without it answers.
fn require_audit(
    audit: Option<web::Data<Arc<UpstreamAuditService>>>,
) -> Result<web::Data<Arc<UpstreamAuditService>>, AppError> {
    audit.ok_or_else(|| {
        AppError::service_unavailable(
            "the upstream audit is not running in this process: [upstream_audit] enabled = \
             false, or this process has no worker role",
        )
    })
}

// ── recheck ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct RecheckRequest {
    pub registry: String,
    pub package_name: String,
    /// One cached version; absent means every cached version of the name.
    #[serde(default)]
    pub version: Option<String>,
}

/// What one probe did (RFC 0014 §4.6).
#[derive(Debug, Serialize, ToSchema)]
pub struct RecheckResponse {
    pub registry: String,
    pub package_name: String,
    /// Packages the probe asked upstream about: one, or zero when nothing
    /// is cached under the name any more.
    pub probed: usize,
    /// Packages (or versions of them) upstream denied.
    pub missing: usize,
    /// Packages upstream could not answer for.
    pub inconclusive: usize,
    /// `confirmed` or `reappeared`, one per state change the probe produced.
    /// Empty when the row only moved to — or stayed — `missing`, or when
    /// upstream simply has it.
    pub transitions: Vec<String>,
    /// The row after the probe: absent when upstream has the package and no
    /// miss is on record.
    pub status: Option<UpstreamStatus>,
}

/// Probe one cached package against its upstream now (RFC 0014 §4.6).
///
/// The same ladder and state machine as the periodic sweep — a miss here
/// counts towards confirmation like any other, a success clears the row —
/// so an admin who has heard from upstream does not wait an interval. It
/// is a probe, not an override: it cannot confirm a disappearance the
/// count and age floors would not.
#[utoipa::path(
    post,
    path = "/api/v1/admin/upstream/recheck",
    tag = "back-office",
    request_body = RecheckRequest,
    responses(
        (status = 200, description = "Probed; what the probe found and the row it left", body = RecheckResponse),
        (status = 403, description = "`system:write` required"),
        (status = 404, description = "Not an audited registry, or nothing cached under the name"),
        (status = 503, description = "The upstream audit is not running in this process"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/upstream/recheck")]
pub async fn recheck_upstream(
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    audit: Option<web::Data<Arc<UpstreamAuditService>>>,
    body: web::Json<RecheckRequest>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::SystemWrite, None, &hot).await?;
    let audit = require_audit(audit)?;
    let req = body.into_inner();
    let report = audit
        .recheck(&req.registry, &req.package_name, req.version.as_deref())
        .await
        .map_err(AppError::from)?;
    // The row as asked for; a version-level ask whose package went as a
    // whole is answered by the package-level row.
    let key = UpstreamKey {
        registry: &req.registry,
        package_name: &req.package_name,
        version: req.version.as_deref(),
    };
    let mut status = audit.status.get(&key).await.map_err(AppError::from)?;
    if status.is_none() && req.version.is_some() {
        status = audit
            .status
            .get(&UpstreamKey::package(&req.registry, &req.package_name))
            .await
            .map_err(AppError::from)?;
    }
    Ok(web::Json(RecheckResponse {
        registry: req.registry,
        package_name: req.package_name,
        probed: report.probed,
        missing: report.missing,
        inconclusive: report.inconclusive,
        transitions: report
            .transitions
            .iter()
            .map(|t| match t {
                Transition::Confirmed(..) => "confirmed".to_owned(),
                Transition::Reappeared(..) => "reappeared".to_owned(),
            })
            .collect(),
        status,
    }))
}

// ── the listing ──────────────────────────────────────────────────────────────

/// `?registry=&state=&page=&per_page=` on the listing.
#[derive(Debug, Deserialize, IntoParams)]
pub struct DisappearedQuery {
    /// One registry; absent lists every audited registry.
    pub registry: Option<String>,
    /// `missing` (seen missing, unconfirmed) or `disappeared` (confirmed);
    /// absent lists both.
    pub state: Option<String>,
    #[serde(default)]
    pub page: u64,
    /// Absent (or `0`) takes `[limits].packages_per_page`.
    #[serde(default)]
    pub per_page: u64,
}

/// One `upstream_status` row, as the console shows it (RFC 0014 §4.6
/// `UpstreamStatusSummary`).
#[derive(Debug, Serialize, ToSchema)]
pub struct UpstreamStatusSummary {
    pub registry: String,
    pub package_name: String,
    /// Absent for a whole-package row: every cached version went at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub state: UpstreamState,
    pub first_missed_at: chrono::DateTime<chrono::Utc>,
    pub last_checked_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub consecutive_misses: u32,
    /// The upstream's last error, verbatim and bounded; never parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl From<UpstreamStatus> for UpstreamStatusSummary {
    fn from(r: UpstreamStatus) -> Self {
        Self {
            registry: r.registry,
            package_name: r.package_name,
            version: r.version,
            state: r.state,
            first_missed_at: r.first_missed_at,
            last_checked_at: r.last_checked_at,
            confirmed_at: r.confirmed_at,
            consecutive_misses: r.consecutive_misses,
            last_error: r.last_error,
        }
    }
}

/// One audited registry's row counts.
#[derive(Debug, Serialize, ToSchema)]
pub struct UpstreamRegistryCounts {
    pub registry: String,
    pub missing: u64,
    pub disappeared: u64,
}

/// A page of rows (RFC 0014 §4.6).
#[derive(Debug, Serialize, ToSchema)]
pub struct UpstreamStatusPage {
    pub items: Vec<UpstreamStatusSummary>,
    /// Rows matching the filter, every page included.
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
    /// `audit` or `block`: what a confirmation does on this instance. On the
    /// listing so a console never has to answer "check the config file".
    pub policy: String,
    /// The registries the sweep covers.
    pub registries: Vec<String>,
    /// Per audited registry, how many rows are `missing` and how many
    /// `disappeared` — the health card's numbers, unfiltered.
    pub counts: Vec<UpstreamRegistryCounts>,
}

/// The rows the audit holds: packages seen missing and packages confirmed
/// gone (RFC 0014 §4.6). `system:read`. Pages by
/// `[limits].packages_per_page` when `per_page` is absent.
#[utoipa::path(
    get,
    path = "/api/v1/admin/upstream/disappeared",
    tag = "back-office",
    params(DisappearedQuery),
    responses(
        (status = 200, description = "A page of rows, with the active policy and the per-registry counts", body = UpstreamStatusPage),
        (status = 400, description = "An unknown `state`"),
        (status = 403, description = "`system:read` required"),
        (status = 503, description = "The upstream audit is not running in this process"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/upstream/disappeared")]
pub async fn list_disappeared(
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    audit: Option<web::Data<Arc<UpstreamAuditService>>>,
    query: web::Query<DisappearedQuery>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::SystemRead, None, &hot).await?;
    let audit = require_audit(audit)?;
    let state = match query
        .state
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(s) => Some(s.parse::<UpstreamState>().map_err(|_| {
            AppError::bad_request(format!("state '{s}' is not one of: missing, disappeared"))
        })?),
    };
    let per_page = if query.per_page == 0 {
        hot.read().await.packages_per_page
    } else {
        query.per_page
    };
    let (page, per_page) = crate::handlers::clamp_pagination(query.page, per_page);
    let filter = UpstreamStatusFilter {
        registry: query.registry.clone(),
        state,
        limit: per_page as usize,
        offset: (page * per_page) as usize,
    };
    let count_filter = UpstreamStatusFilter {
        limit: 0,
        offset: 0,
        ..filter.clone()
    };
    let (rows, total) =
        tokio::try_join!(audit.status.list(filter), audit.status.count(count_filter))
            .map_err(AppError::from)?;
    let mut items: Vec<UpstreamStatusSummary> = rows.into_iter().map(Into::into).collect();
    // Newest confirmation first, then the most recently missed: the row an
    // operator came to see is the one that just happened.
    items.sort_by(|a, b| {
        b.confirmed_at
            .cmp(&a.confirmed_at)
            .then(b.first_missed_at.cmp(&a.first_missed_at))
    });
    let mut counts = Vec::with_capacity(audit.registries.len());
    for registry in &audit.registries {
        let (missing, disappeared) = audit.counts(registry).await.map_err(AppError::from)?;
        counts.push(UpstreamRegistryCounts {
            registry: registry.clone(),
            missing,
            disappeared,
        });
    }
    Ok(web::Json(UpstreamStatusPage {
        items,
        total,
        page,
        per_page,
        policy: audit.policy.on_confirmed.as_str().to_owned(),
        registries: audit.registries.clone(),
        counts,
    }))
}

// ── one package ──────────────────────────────────────────────────────────────

/// What the audit knows about one package: its rows, whole-package and
/// per-version (RFC 0014 §4.6).
#[derive(Debug, Serialize, ToSchema)]
pub struct UpstreamPackageStatus {
    pub registry: String,
    pub package_name: String,
    /// `audit` or `block`.
    pub policy: String,
    /// Empty when upstream has it and no miss is on record.
    pub rows: Vec<UpstreamStatusSummary>,
}

/// One package's rows (RFC 0014 §4.6). `system:read`. A package with no row
/// answers `200` with an empty list — "present" is the absence of a row,
/// and a `404` here would read as "no such package".
#[utoipa::path(
    get,
    path = "/api/v1/admin/upstream/status/{registry}/{name}",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "Package name"),
    ),
    responses(
        (status = 200, description = "The package's rows, possibly none", body = UpstreamPackageStatus),
        (status = 403, description = "`system:read` required"),
        (status = 404, description = "Not an audited registry"),
        (status = 503, description = "The upstream audit is not running in this process"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/upstream/status/{registry}/{name}")]
pub async fn get_upstream_status(
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    audit: Option<web::Data<Arc<UpstreamAuditService>>>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        Action::SystemRead,
        Some(&registry),
        &hot,
    )
    .await?;
    let audit = require_audit(audit)?;
    if !audit.registries.contains(&registry) {
        return Err(AppError::not_found(format!(
            "registry '{registry}' is not audited ([upstream_audit] registries)"
        )));
    }
    let rows = audit
        .status
        .list(UpstreamStatusFilter {
            registry: Some(registry.clone()),
            state: None,
            limit: 0,
            offset: 0,
        })
        .await
        .map_err(AppError::from)?
        .into_iter()
        .filter(|r| r.package_name == name)
        .map(UpstreamStatusSummary::from)
        .collect();
    Ok(web::Json(UpstreamPackageStatus {
        registry,
        package_name: name,
        policy: audit.policy.on_confirmed.as_str().to_owned(),
        rows,
    }))
}
