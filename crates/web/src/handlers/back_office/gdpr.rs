//! RFC 0036 §6.3–6.4 over HTTP: erasing and exporting a data subject, and
//! verifying the sealed audit trail.

use std::sync::Arc;

use actix_web::{get, post, web, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::entities::Action;
use batlehub_core::services::audit_trail::{AuditTrailService, EraseReport, VerifyReport};

use crate::error::AppError;
use crate::extractors::AuthIdentity;
use crate::handlers::back_office::require_verb;

type Hot = web::Data<batlehub_core::services::hot_config::HotConfigLock>;

/// The service, or the `503` an instance without one answers. The server
/// always wires it; a test app that does not ask for it gets this.
fn trail(
    svc: Option<web::Data<Arc<AuditTrailService>>>,
) -> Result<web::Data<Arc<AuditTrailService>>, AppError> {
    svc.ok_or_else(|| AppError::service_unavailable("the audit trail service is not configured"))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EraseRequest {
    /// The subject, exactly as the audit log names them.
    pub user_id: String,
    /// Erase even while an open decision — an account block, a package block
    /// they placed — names the subject.
    #[serde(default)]
    pub force: bool,
}

/// Pseudonymise a data subject (RFC 0036 §4.2): their audit rows, tokens
/// (revoked) and blocks. Publication and ownership rows are exempt.
#[utoipa::path(
    post,
    path = "/api/v1/admin/gdpr/erase",
    tag = "back-office",
    request_body = EraseRequest,
    responses(
        (status = 200, description = "Erased; the pseudonym and the counts", body = EraseReport),
        (status = 403, description = "`gdpr:erase` required"),
        (status = 409, description = "An open decision names the subject; retry with `force`"),
        (status = 501, description = "No `[audit] erasure_key` is configured"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/gdpr/erase")]
pub async fn gdpr_erase(
    body: web::Json<EraseRequest>,
    identity: AuthIdentity,
    svc: Option<web::Data<Arc<AuditTrailService>>>,
    hot: Hot,
) -> Result<impl Responder, AppError> {
    require_verb(&identity, Action::GdprErase, None, &hot).await?;
    let report = trail(svc)?
        .erase(&body.user_id, body.force, &identity.0, identity.1.clone())
        .await?;
    Ok(web::Json(report))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ExportQuery {
    /// The subject, exactly as the audit log names them.
    pub user_id: String,
}

/// Everything the instance holds about a data subject (Art. 15).
#[derive(Debug, Serialize, ToSchema)]
pub struct GdprExport {
    /// One section per store: `access_events`, `tokens` (never their hashes),
    /// `account_blocks`, `package_blocks_placed`, `quota_usage`, and the rows
    /// erasure keeps — `publications_retained`, `ownership_retained`, `grants`.
    #[serde(flatten)]
    #[schema(value_type = Object)]
    pub record: serde_json::Value,
}

/// Export a data subject's records, answering an access request (RFC 0036
/// §6.3). Recorded as `gdpr_export`.
#[utoipa::path(
    get,
    path = "/api/v1/admin/gdpr/export",
    tag = "back-office",
    params(ExportQuery),
    responses(
        (status = 200, description = "The subject's records", body = GdprExport),
        (status = 403, description = "`audit:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/gdpr/export")]
pub async fn gdpr_export(
    query: web::Query<ExportQuery>,
    identity: AuthIdentity,
    svc: Option<web::Data<Arc<AuditTrailService>>>,
    hot: Hot,
) -> Result<impl Responder, AppError> {
    require_verb(&identity, Action::AuditRead, None, &hot).await?;
    let record = trail(svc)?
        .export(&query.user_id, &identity.0, identity.1.clone())
        .await?;
    Ok(web::Json(GdprExport { record }))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct VerifyRequest {
    /// Check only windows ending after this instant.
    pub from: Option<DateTime<Utc>>,
    /// Check only windows starting before this instant.
    pub to: Option<DateTime<Utc>>,
    /// The digest of the newest `audit_seal` line the SIEM holds. Without it a
    /// truncated tail verifies (RFC 0036 §5.3).
    pub head: Option<String>,
}

/// Replay the seal chain and every sealed window against it (RFC 0036 §6.4).
#[utoipa::path(
    post,
    path = "/api/v1/admin/audit/verify",
    tag = "back-office",
    request_body = VerifyRequest,
    responses(
        (status = 200, description = "What the replay found; `ok` is the verdict", body = VerifyReport),
        (status = 403, description = "`audit:read` required"),
        (status = 501, description = "The trail is not sealed"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/audit/verify")]
pub async fn audit_verify(
    body: Option<web::Json<VerifyRequest>>,
    identity: AuthIdentity,
    svc: Option<web::Data<Arc<AuditTrailService>>>,
    hot: Hot,
) -> Result<impl Responder, AppError> {
    require_verb(&identity, Action::AuditRead, None, &hot).await?;
    let body = body.map(web::Json::into_inner).unwrap_or_default();
    let report = trail(svc)?
        .verify(body.from, body.to, body.head.as_deref())
        .await?;
    Ok(web::Json(report))
}
