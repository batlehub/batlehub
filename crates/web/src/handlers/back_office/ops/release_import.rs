//! `POST /api/v1/admin/registries/{registry}/import` — run a configured import
//! now (RFC 0021 §6.4).
//!
//! Two verbs, and they answer different questions (§11 q2). `cache:warm` is
//! what it takes to *ask* for an import: an operator deciding this instance
//! should hold something is the same decision warming asks for. The publish
//! itself runs as the configured principal and needs that principal's own
//! `releases:publish` — so an operator may trigger an import they could not
//! themselves publish, and if the principal cannot publish, it fails as a
//! publish, in the audit log, under the subject that was configured for it
//! rather than under whoever pressed the button.

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::{post, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::services::{ImportReport, ReleaseImportService};

use crate::{error::AppError, extractors::AuthIdentity};

/// Target registry name → the imports configured into it.
///
/// Keyed by the **target**, because that is what the route names and what an
/// operator is looking at when they ask for this: "make this registry hold what
/// its repositories have released".
pub type ReleaseImportMap = HashMap<String, Vec<Arc<ReleaseImportService>>>;

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ImportRequest {
    /// Import this tag instead of what the configuration selects. The only way
    /// to reach a pre-release deliberately: `latest` will not choose one.
    #[serde(default)]
    pub tag: Option<String>,
    /// Only run the imports whose `repo` is this one. Absent: every import
    /// configured into the registry.
    #[serde(default)]
    pub repo: Option<String>,
}

/// One asset that did not import, named rather than counted.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportFailureDto {
    pub tag: String,
    pub asset: String,
    pub error: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ImportResponse {
    /// Assets fetched and published.
    pub imported: usize,
    /// Assets whose version this registry already holds. Not an error: it is
    /// what makes a scheduled import free to re-run.
    pub skipped: usize,
    pub errors: usize,
    pub failures: Vec<ImportFailureDto>,
}

impl From<ImportReport> for ImportResponse {
    fn from(r: ImportReport) -> Self {
        Self {
            imported: r.imported,
            skipped: r.skipped,
            errors: r.errors,
            failures: r
                .failures
                .into_iter()
                .map(|f| ImportFailureDto {
                    tag: f.tag,
                    asset: f.asset,
                    error: f.error,
                })
                .collect(),
        }
    }
}

/// Import a forge release into this registry now.
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/import",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "The registry imported into"),
    ),
    request_body = ImportRequest,
    responses(
        (status = 200, description = "Import run", body = ImportResponse),
        (status = 403, description = "`cache:warm` required"),
        (status = 404, description = "No import is configured into this registry"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/import")]
pub async fn import_registry(
    path: web::Path<String>,
    identity: AuthIdentity,
    body: Option<web::Json<ImportRequest>>,
    imports: web::Data<ReleaseImportMap>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::CacheWarm,
        Some(&registry),
        &hot,
    )
    .await?;

    let req = body.map(|b| b.into_inner()).unwrap_or_default();
    // A registry with no import configured is a 404 rather than an empty 200:
    // an operator asking for one has a configuration in mind, and "nothing
    // happened" would look like a working import of nothing.
    let configured = imports.get(&registry).ok_or_else(|| {
        AppError::not_found(format!(
            "no [[release_imports]] is configured into registry '{registry}'"
        ))
    })?;
    let selected: Vec<_> = configured
        .iter()
        .filter(|svc| req.repo.as_deref().is_none_or(|r| r == svc.repo))
        .collect();
    if selected.is_empty() {
        return Err(AppError::not_found(format!(
            "no import of '{}' is configured into registry '{registry}'",
            req.repo.unwrap_or_default()
        )));
    }

    let mut report = ImportReport::default();
    for svc in selected {
        report += match req.tag.as_deref() {
            Some(tag) => svc.import_tag(tag).await,
            None => svc.import().await,
        };
    }
    tracing::info!(
        registry = %registry,
        imported = report.imported, skipped = report.skipped, errors = report.errors,
        asked_by = ?identity.0.user_id,
        "release import: run finished"
    );
    Ok(web::Json(ImportResponse::from(report)))
}
