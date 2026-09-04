//! The exposure report (RFC 0002 §4.6, recast by §13): who pulled a flagged
//! version, and was the flag known when they did.
//!
//! One query over the access log joined to the flags, grouped by consumer ×
//! coordinate × flag, newest pull first, paged by keyset (`after` is the
//! cursor the previous page returned). `audit:read`: the report is a view
//! of the same trail the audit log is, and the same reader holds both.
//!
//! Beside the rows, the **coverage** block says what the report could not
//! see: how many registries have an SBOM extractor (so the CVE scan reaches
//! them), when the scan last ran per registry, and which sources have
//! pushed. A report that is silent about its blind spots reads as a fact,
//! and this one is often handed to an auditor.

use std::sync::Arc;

use actix_web::http::header::{ContentDisposition, DispositionParam, DispositionType};
use actix_web::{get, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{
        Action, ExposureCoverage, ExposureCursor, ExposurePage, ExposureQuery, ExposureRow,
        ExposureWhen, FlagEffect,
    },
    ports::AdvisoryRepository,
};

use crate::{error::AppError, extractors::AuthIdentity, handlers::schemas::ProtocolDocument};

/// What the coverage block needs from the configuration, as app data.
#[derive(Clone, Default)]
pub struct ExposureConfig {
    /// Registries with `[registries.sbom]`.
    pub sbom_registries: u64,
}

#[derive(Deserialize, IntoParams)]
pub struct ExposureQueryParams {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub registry: Option<String>,
    pub package_name: Option<String>,
    pub source: Option<String>,
    /// Keep flags at this effect or stronger: `inform | warn | gate | hard_block`.
    pub min_effect: Option<String>,
    /// `any` (default), `before_flag` (pulled before the flag was known),
    /// `after_flag`.
    pub when: Option<String>,
    /// The `next` cursor of the previous page.
    pub after: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u64,
}

fn default_limit() -> u64 {
    100
}

#[derive(Serialize, ToSchema)]
pub struct ExposureResponse {
    pub rows: Vec<ExposureRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    pub coverage: ExposureCoverage,
}

fn parse_query(q: &ExposureQueryParams) -> Result<ExposureQuery, AppError> {
    let min_effect = match q.min_effect.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(FlagEffect::parse(s).ok_or_else(|| {
            AppError::bad_request(format!(
                "unknown min_effect '{s}' (expected inform, warn, gate or hard_block)"
            ))
        })?),
        None => None,
    };
    let when = match q.when.as_deref().unwrap_or("any") {
        "any" | "" => ExposureWhen::Any,
        "before_flag" | "before" => ExposureWhen::BeforeFlag,
        "after_flag" | "after" => ExposureWhen::AfterFlag,
        other => {
            return Err(AppError::bad_request(format!(
                "unknown when '{other}' (expected any, before_flag or after_flag)"
            )))
        }
    };
    let after =
        match q.after.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => Some(ExposureCursor::decode(s).ok_or_else(|| {
                AppError::bad_request("after is not a cursor this endpoint issued")
            })?),
            None => None,
        };
    if let (Some(from), Some(to)) = (q.from, q.to) {
        if from > to {
            return Err(AppError::bad_request("from is after to"));
        }
    }
    Ok(ExposureQuery {
        from: q.from,
        to: q.to,
        registry: q.registry.clone(),
        package_name: q.package_name.clone(),
        source: q.source.clone(),
        min_effect,
        when,
        after,
        limit: q.limit.clamp(1, 1000),
    })
}

async fn coverage(
    repo: &Arc<dyn AdvisoryRepository>,
    hot: &batlehub_core::services::hot_config::HotConfigLock,
    cfg: &ExposureConfig,
) -> Result<ExposureCoverage, AppError> {
    let (registries_total, security_profiles) = {
        let hot = hot.read().await;
        (hot.registries.len() as u64, hot.security.len() as u64)
    };
    let (last_scan, flag_sources) =
        tokio::try_join!(repo.list_scan_state(), repo.source_coverage(Utc::now()))
            .map_err(AppError::from)?;
    Ok(ExposureCoverage {
        registries_total,
        sbom_configured: cfg.sbom_registries,
        security_profiles,
        last_scan,
        flag_sources,
        window_truncated_at: None,
    })
}

/// The exposure report, one page.
#[utoipa::path(
    get,
    path = "/api/v1/admin/exposure",
    tag = "back-office",
    params(ExposureQueryParams),
    responses(
        (status = 200, description = "Rows newest pull first, a cursor when more follow, and the coverage block", body = ExposureResponse),
        (status = 400, description = "Bad filter or cursor"),
        (status = 403, description = "`audit:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/exposure")]
pub async fn exposure_report(
    query: web::Query<ExposureQueryParams>,
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn AdvisoryRepository>>,
    cfg: web::Data<ExposureConfig>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::AuditRead, None, &hot).await?;
    let q = parse_query(&query)?;
    let (page, coverage) = tokio::try_join!(
        async { repo.list_exposure(&q).await.map_err(AppError::from) },
        coverage(&repo, &hot, &cfg),
    )?;
    let ExposurePage { rows, next } = page;
    Ok(web::Json(ExposureResponse {
        rows,
        next,
        coverage,
    }))
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// The whole report, every page walked, as CSV or JSON.
#[utoipa::path(
    get,
    path = "/api/v1/admin/exposure/export",
    tag = "back-office",
    params(ExposureQueryParams),
    responses(
        (status = 200, description = "Every row of the report; `?format=csv` selects CSV, anything else JSON", content(
            (Vec<ExposureRow> = "application/json"),
            (ProtocolDocument = "text/csv"),
        )),
        (status = 403, description = "`audit:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/exposure/export")]
pub async fn export_exposure(
    query: web::Query<ExposureQueryParams>,
    format: web::Query<ExportFormat>,
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn AdvisoryRepository>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<HttpResponse, AppError> {
    crate::handlers::back_office::require_verb(&identity, Action::AuditRead, None, &hot).await?;
    let mut q = parse_query(&query)?;
    q.limit = 1000;
    // Walk the keyset to the end, bounded so a runaway cursor cannot loop.
    let mut rows: Vec<ExposureRow> = Vec::new();
    for _ in 0..1000 {
        let page = repo.list_exposure(&q).await.map_err(AppError::from)?;
        rows.extend(page.rows);
        match page.next.as_deref().and_then(ExposureCursor::decode) {
            Some(c) => q.after = Some(c),
            None => break,
        }
    }
    let fmt = if format.format == "csv" {
        "csv"
    } else {
        "json"
    };
    let filename = format!("exposure-{}.{fmt}", Utc::now().format("%Y%m%dT%H%M%SZ"));
    let disposition = ContentDisposition {
        disposition: DispositionType::Attachment,
        parameters: vec![DispositionParam::Filename(filename)],
    };
    if fmt == "csv" {
        let mut csv = String::from(
            "consumer,consumer_role,registry,package_name,version,source,external_id,kind,\
             effect,severity,summary,flag_first_seen,pulls,pulls_before_flag,first_pull,last_pull\n",
        );
        for r in &rows {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_field(&r.consumer),
                csv_field(&r.consumer_role),
                csv_field(&r.registry),
                csv_field(&r.package_name),
                csv_field(&r.version),
                csv_field(&r.source),
                csv_field(&r.external_id),
                csv_field(r.kind.as_str()),
                r.effect.as_str(),
                r.severity.map(|s| s.as_str()).unwrap_or(""),
                csv_field(&r.summary),
                r.flag_first_seen.to_rfc3339(),
                r.pulls,
                r.pulls_before_flag,
                r.first_pull.to_rfc3339(),
                r.last_pull.to_rfc3339(),
            ));
        }
        return Ok(HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header(disposition)
            .body(csv));
    }
    Ok(HttpResponse::Ok().insert_header(disposition).json(rows))
}

#[derive(Deserialize, IntoParams)]
pub struct ExportFormat {
    /// "json" (default) or "csv"
    #[serde(default)]
    pub format: String,
}
