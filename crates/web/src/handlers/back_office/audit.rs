use std::sync::Arc;

use actix_web::http::header::{ContentDisposition, DispositionParam, DispositionType};
use actix_web::{delete, get, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{AccessAction, AccessEvent, EventFilter},
    error::CoreError,
    services::{csv::field as csv_field, AdminService},
};

use crate::{error::AppError, extractors::AuthIdentity, handlers::schemas::ProtocolDocument};

#[derive(Deserialize, IntoParams)]
pub struct AuditQuery {
    pub registry: Option<String>,
    /// Narrow to one package name, as the registry filter narrows to one
    /// registry.
    pub package_name: Option<String>,
    pub user_id: Option<String>,
    /// Keep only these actions: one wire name, or several comma-separated
    /// (`delete,retention_reclaim`). Absent means every action.
    ///
    /// Both spellings the API emits are accepted — `view_metadata` as the
    /// package-detail timeline and the CSV export write it, `viewmetadata` as
    /// this endpoint's own JSON serialises it.
    pub action: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub denied_only: Option<bool>,
    #[serde(default)]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}

fn default_per_page() -> u64 {
    100
}

/// Parse the `?action=` parameter into a filter set.
///
/// An unknown name is a `400` rather than an empty result, because the two are
/// indistinguishable to the caller and one of them is a typo in an audit query
/// — the worst place for "no rows" to be ambiguous. The error names every
/// action, since there is no other endpoint that lists them.
fn parse_actions(raw: Option<&str>) -> Result<Vec<AccessAction>, AppError> {
    let Some(raw) = raw else {
        return Ok(vec![]);
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|name| {
            AccessAction::from_wire(name).ok_or_else(|| {
                let known: Vec<&str> = AccessAction::ALL.iter().map(|a| a.as_str()).collect();
                AppError::bad_request(format!(
                    "unknown audit action '{name}'. Known actions: {}",
                    known.join(", ")
                ))
            })
        })
        .collect()
}

/// Paginated envelope for `GET /api/v1/admin/audit-log`, matching the shape of
/// its sibling list endpoints (`AdminPackageListResponse`) instead of returning
/// a bare array with no way to tell if more pages exist.
#[derive(Serialize, ToSchema)]
pub struct AuditLogResponse {
    pub items: Vec<AccessEvent>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

/// Query the access audit log (admin).
#[utoipa::path(
    get,
    path = "/api/v1/admin/audit-log",
    tag = "back-office",
    params(AuditQuery),
    responses(
        (status = 200, description = "Paginated access events", body = AuditLogResponse),
        (status = 403, description = "`audit:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/audit-log")]
pub async fn audit_log(
    query: web::Query<AuditQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::AuditRead,
        None,
        &hot,
    )
    .await?;

    let (page, per_page) = crate::handlers::clamp_pagination(query.page, query.per_page);
    let actions = parse_actions(query.action.as_deref())?;
    let filter = EventFilter {
        registry: query.registry.clone(),
        package_name: query.package_name.clone(),
        user_id: query.user_id.clone(),
        actions: actions.clone(),
        from: query.from,
        to: query.to,
        denied_only: query.denied_only.unwrap_or(false),
        limit: per_page,
        offset: page * per_page,
    };
    // The same predicate as the page above, or the total describes a different
    // set than the rows do.
    let count_filter = EventFilter {
        limit: 0,
        offset: 0,
        ..filter.clone()
    };

    let (items, total) = tokio::try_join!(
        admin_svc.list_events(filter),
        admin_svc.count_events(count_filter),
    )
    .map_err(AppError::from)?;

    Ok(web::Json(AuditLogResponse {
        items,
        total,
        page: query.page,
        per_page: query.per_page,
    }))
}

#[derive(Deserialize, IntoParams)]
pub struct ExportQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub registry: Option<String>,
    pub user_id: Option<String>,
    /// Restrict the export to denied events, as the listing's "Denied only"
    /// filter does.
    ///
    /// The listing accepted this and the export did not, so an operator reading
    /// a table of denials downloaded a file containing every allowed event too,
    /// with nothing on screen saying so — on the surface whose whole purpose is
    /// establishing what happened. An export has to be able to describe the
    /// same set the table did.
    #[serde(default)]
    pub denied_only: bool,
    /// Narrow to one package name.
    pub package_name: Option<String>,
    /// Keep only these actions, comma-separated. As the listing's `action`
    /// filter — an export has to be able to describe the same set the table
    /// did, which is the reasoning `denied_only` above already followed.
    pub action: Option<String>,
    /// "json" (default) or "csv"
    #[serde(default)]
    pub format: String,
}

fn default_export_format(fmt: &str) -> &'static str {
    if fmt == "csv" {
        "csv"
    } else {
        "json"
    }
}

/// Export audit-log events for a time range (admin, SOC 2 compliance export).
#[utoipa::path(
    get,
    path = "/api/v1/admin/audit-log/export",
    tag = "back-office",
    params(ExportQuery),
    responses(
        (status = 200, description = "Audit log export; `?format=csv` selects CSV, anything else JSON", content(
            (Vec<AccessEvent> = "application/json"),
            (ProtocolDocument = "text/csv"),
        )),
        (status = 403, description = "`audit:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/audit-log/export")]
pub async fn export_audit_log(
    query: web::Query<ExportQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<HttpResponse, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::AuditRead,
        None,
        &hot,
    )
    .await?;

    let filter = EventFilter {
        registry: query.registry.clone(),
        package_name: query.package_name.clone(),
        user_id: query.user_id.clone(),
        actions: parse_actions(query.action.as_deref())?,
        from: query.from,
        to: query.to,
        denied_only: query.denied_only,
        limit: 100_000,
        offset: 0,
    };

    let events = admin_svc
        .list_events(filter)
        .await
        .map_err(AppError::from)?;

    let fmt = default_export_format(&query.format);
    let filename = format!("audit-log-{}.{fmt}", Utc::now().format("%Y%m%dT%H%M%SZ"));

    let disposition = ContentDisposition {
        disposition: DispositionType::Attachment,
        parameters: vec![DispositionParam::Filename(filename)],
    };

    if fmt == "csv" {
        let mut csv = String::from(
            "id,timestamp,user_id,user_role,registry,package_name,package_version,\
             package_artifact,action,outcome,deny_reason,ip_address,user_agent\n",
        );
        for e in &events {
            let deny_reason = match &e.result {
                batlehub_core::entities::AccessResult::Denied { reason } => reason.as_str(),
                batlehub_core::entities::AccessResult::ProxyError { reason } => reason.as_str(),
                _ => "",
            };
            let outcome = match &e.result {
                batlehub_core::entities::AccessResult::Allowed => "allowed",
                batlehub_core::entities::AccessResult::Denied { .. } => "denied",
                batlehub_core::entities::AccessResult::ProxyError { .. } => "error",
            };
            // Every text column goes through `csv_field`. Four of them are
            // written by the client being audited — `user_agent` and
            // `ip_address` outright, `package_name` and `deny_reason` by way of
            // what it asked for — so an unquoted comma or newline in a
            // `User-Agent` used to shift the columns of the row it appears in,
            // or forge whole rows in an auditor's export, and a leading `=`
            // made the cell a formula. The numeric and timestamp columns are
            // ours and need no escaping.
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                e.id,
                e.timestamp.to_rfc3339(),
                csv_field(e.user_id.as_deref().unwrap_or("")),
                csv_field(&e.user_role.to_string()),
                csv_field(
                    e.package_id
                        .as_ref()
                        .map(|p| p.registry.as_str())
                        .unwrap_or("")
                ),
                csv_field(e.package_id.as_ref().map(|p| p.name.as_str()).unwrap_or("")),
                csv_field(
                    e.package_id
                        .as_ref()
                        .map(|p| p.version.as_str())
                        .unwrap_or("")
                ),
                csv_field(
                    e.package_id
                        .as_ref()
                        .and_then(|p| p.artifact.as_deref())
                        .unwrap_or("")
                ),
                // `as_str`, not `{:?}`: the debug spelling squashes the words
                // together (`viewmetadata`), and this column is the one an
                // auditor pastes back into `?action=`.
                e.action.as_str(),
                outcome,
                csv_field(deny_reason),
                csv_field(e.ip_address.as_deref().unwrap_or("")),
                csv_field(e.user_agent.as_deref().unwrap_or("")),
            ));
        }
        Ok(HttpResponse::Ok()
            .insert_header(disposition)
            .content_type("text/csv")
            .body(csv))
    } else {
        let body = serde_json::to_string(&events)
            .map_err(|e| CoreError::Other(anyhow::anyhow!("serialize: {e}")))?;
        Ok(HttpResponse::Ok()
            .insert_header(disposition)
            .content_type("application/json")
            .body(body))
    }
}

// ── What one identity pulled (RFC 0018 §4.2) ─────────────────────────────────

/// The default window when the caller names none.
///
/// `verdicts pullers` takes its default from the registry's own
/// `pullers_window_days`, which this cannot: the question is scoped to an
/// identity and spans every registry, so there is no per-registry policy to
/// read. Thirty days matches that policy's own default, so the two reports
/// answer over the same span unless an operator has said otherwise.
const DEFAULT_PULLS_WINDOW: std::time::Duration = std::time::Duration::from_secs(30 * 86_400);

#[derive(Deserialize, IntoParams)]
pub struct PullsQuery {
    /// Whose pulls: a user id, or `ip:<addr>` for an anonymous caller.
    pub identity: String,
    /// Narrow to one registry.
    pub registry: Option<String>,
    /// Narrow to one package name.
    pub package: Option<String>,
    /// `30d` / `12h` / `90m` back from now, or an RFC 3339 instant. Absent: 30 days.
    pub since: Option<String>,
    /// `csv` for the export; anything else is JSON.
    pub format: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PullsResponse {
    pub identity: String,
    /// The start of the window the rows cover.
    pub since: DateTime<Utc>,
    pub pulls: Vec<batlehub_core::services::Pull>,
}

/// What one identity pulled inside a window (admin).
///
/// The transpose of `/api/v1/verdicts/{registry}/{name}/{version}/pullers`:
/// that one pins a version and asks who took it, this one pins an identity and
/// asks what it took. Both are `audit:read` at the instance tier and both read
/// `access_events`, so an auditor handed either can reconcile it against the
/// other.
#[utoipa::path(
    get,
    path = "/api/v1/audit/pulls",
    tag = "back-office",
    params(PullsQuery),
    responses(
        (status = 200, description = "What the identity pulled; `?format=csv` selects CSV", content(
            (PullsResponse = "application/json"),
            (ProtocolDocument = "text/csv"),
        )),
        (status = 400, description = "`since` is not a window or an instant"),
        (status = 403, description = "`audit:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/audit/pulls")]
pub async fn audit_pulls(
    query: web::Query<PullsQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<HttpResponse, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::AuditRead,
        None,
        &hot,
    )
    .await?;

    let subject = query.identity.trim();
    if subject.is_empty() {
        return Err(AppError::bad_request(
            "identity is required: a user id, or `ip:<addr>` for an anonymous caller",
        ));
    }

    // The same parser `verdicts pullers` uses, so the two reports accept and
    // refuse the same windows.
    let since = crate::handlers::security::parse_since(
        query.since.as_deref(),
        DEFAULT_PULLS_WINDOW,
        Utc::now(),
    )?;

    let pulls = batlehub_core::services::pulls_for(
        admin_svc.repo.as_ref(),
        subject,
        since,
        query.registry.as_deref(),
        query.package.as_deref(),
    )
    .await
    .map_err(AppError::from)?;

    if query.format.as_deref() == Some("csv") {
        let disposition = ContentDisposition {
            disposition: DispositionType::Attachment,
            parameters: vec![DispositionParam::Filename(format!(
                "pulls-{}.csv",
                subject.replace(['/', '\\', ':'], "-")
            ))],
        };
        return Ok(HttpResponse::Ok()
            .insert_header(disposition)
            .content_type("text/csv; charset=utf-8")
            .body(batlehub_core::services::pulls::to_csv(&pulls)));
    }

    let body = serde_json::to_string(&PullsResponse {
        identity: subject.to_owned(),
        since,
        pulls,
    })
    .map_err(|e| CoreError::Other(anyhow::anyhow!("serialize: {e}")))?;
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(body))
}

#[derive(Deserialize, IntoParams)]
pub struct PurgeQuery {
    pub before: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PurgeResponse {
    pub deleted: u64,
}

/// Purge access-event rows older than `before` (admin).
#[utoipa::path(
    delete,
    path = "/api/v1/admin/audit-log",
    tag = "back-office",
    params(PurgeQuery),
    responses(
        (status = 200, description = "Number of rows deleted", body = PurgeResponse),
        (status = 403, description = "`audit:purge` required"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/audit-log")]
pub async fn purge_audit_log(
    query: web::Query<PurgeQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // **`audit:purge`, not `audit:read`.** The two reads above ask for the read
    // verb and this deletes the table, so sharing one verb would make "let the
    // auditor read the trail" and "let the auditor erase it" the same grant —
    // and the purge writes its own `audit:purge` event, which a second call with
    // the same cutoff removes. This endpoint was `require_admin` before RFC 0015
    // decomposed it, so no estate loses it: §10 rule 5 hands the new verb to
    // `role:admin` at the instance tier, which is the only tier it names.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::AuditPurge,
        None,
        &hot,
    )
    .await?;
    let deleted = admin_svc
        .purge_events_before(query.before, &identity.0)
        .await
        .map_err(AppError::from)?;
    Ok(web::Json(PurgeResponse { deleted }))
}
