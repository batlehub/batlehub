use std::collections::HashSet;
use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{PackageFilter, PackageId},
    services::AdminService,
};

use super::require_user_id;
use crate::{error::AppError, extractors::AuthIdentity};

/// How far back `GET /api/v1/me/downloads` looks when `?days=` is absent.
const DEFAULT_WINDOW_DAYS: i64 = 30;
/// Hard ceiling on `?days=`, so one request cannot scan the whole audit trail.
const MAX_WINDOW_DAYS: i64 = 365;
const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 200;
/// Ceiling on the blocked-package scan used to mark rows. An instance with more
/// blocked coordinates than this marks the overflow as unblocked rather than
/// slowing the home page — stated here because a silent cap is the failure this
/// RFC keeps finding.
const MAX_BLOCKED_SCAN: u64 = 1000;

#[derive(Debug, Deserialize, IntoParams)]
pub struct MyDownloadsQuery {
    /// Maximum rows to return. Clamped to 1–200; defaults to 50.
    pub limit: Option<u64>,
    /// How many days back to look. Clamped to 1–365; defaults to 30.
    pub days: Option<i64>,
}

/// One artifact the caller downloaded.
#[derive(Debug, Serialize, ToSchema)]
pub struct MyDownloadDto {
    pub registry: String,
    pub name: String,
    pub version: String,
    /// The sub-artifact within the coordinate, where the registry has one
    /// (a GitHub asset id, `tarball`, `vsix`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    pub downloaded_at: DateTime<Utc>,
    /// Whether this exact coordinate is blocked **now**.
    ///
    /// The pull succeeded — this endpoint only returns successful downloads —
    /// so this is always retroactive: something you have was refused after you
    /// took it. That is the fact worth surfacing, and it is the miniature of
    /// the question RFC 0002 asks at org scale ("which consumers pulled a
    /// flagged version"), asked about yourself.
    ///
    /// Without it the home page can list a pull it cannot describe, which is
    /// RFC 0004 §2.2's argument reappearing on the one surface a non-admin
    /// actually opens (RFC 0004-bis §5/A9).
    pub blocked: bool,
}

/// What the caller has downloaded, most recent first.
///
/// Only this caller's own rows, only successful downloads. The filter is in
/// `PackageRepository::list_own_downloads` rather than assembled here, so no
/// future caller of that port can forget it (RFC 0004 §6.2).
///
/// Takes no `user_id`. `/api/v1/admin/audit-log` is the endpoint for reading
/// anyone else's, and it is admin-only.
#[utoipa::path(
    get,
    path = "/api/v1/me/downloads",
    tag = "user",
    params(MyDownloadsQuery),
    responses(
        (status = 200, description = "The caller's recent downloads, newest first", body = Vec<MyDownloadDto>),
        (status = 401, description = "Authentication required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/me/downloads")]
pub async fn my_downloads(
    identity: AuthIdentity,
    query: web::Query<MyDownloadsQuery>,
    admin_svc: web::Data<Arc<AdminService>>,
) -> Result<impl Responder, AppError> {
    let user_id = require_user_id(&identity)?;

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let days = query
        .days
        .unwrap_or(DEFAULT_WINDOW_DAYS)
        .clamp(1, MAX_WINDOW_DAYS);
    let since = Utc::now() - Duration::days(days);

    let events = admin_svc
        .list_own_downloads(user_id, since, limit)
        .await
        .map_err(AppError::from)?;

    // One query for every blocked coordinate, not one per row: the caller's
    // recent pulls are capped at 200 and blocked packages are typically a
    // handful, so a set lookup beats N round trips by a wide margin. A failure
    // here reports "not blocked" rather than failing the page — the widget's
    // job is to list your pulls, and losing the block flag is a smaller harm
    // than losing the list.
    let blocked: HashSet<String> = admin_svc
        .list_packages(PackageFilter {
            registry: None,
            registries: vec![],
            name_exact: None,
            name_contains: None,
            blocked_only: true,
            limit: MAX_BLOCKED_SCAN,
            offset: 0,
            viewer: None,
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.package_id.cache_key())
        .collect();

    let rows: Vec<MyDownloadDto> = events
        .into_iter()
        .filter_map(|e| {
            let package_id = e.package_id?;
            // Keyed on the full coordinate including the artifact, so blocking
            // one asset of a path-addressed registry does not mark every other
            // file under the same synthetic package name.
            let is_blocked = blocked.contains(&package_id.cache_key());
            let PackageId {
                registry,
                name,
                version,
                artifact,
            } = package_id;
            Some(MyDownloadDto {
                registry,
                name,
                version,
                artifact,
                downloaded_at: e.timestamp,
                blocked: is_blocked,
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(rows))
}
