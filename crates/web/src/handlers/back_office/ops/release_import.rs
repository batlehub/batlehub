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

use actix_web::{get, post, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::entities::ImportRun;
use batlehub_core::ports::ImportHistory;
use batlehub_core::services::{ImportReport, ReleaseImportService};

use crate::{error::AppError, extractors::AuthIdentity};

/// Target registry name → the imports configured into it.
///
/// Keyed by the **target**, because that is what the route names and what an
/// operator is looking at when they ask for this: "make this registry hold what
/// its repositories have released".
pub type ReleaseImportMap = HashMap<String, Vec<Arc<ReleaseImportService>>>;

/// Where a run's history goes, when a database is configured.
///
/// `Option` because every in-process web test builds an app without one, and a
/// history that is not recorded must not turn a working import into a failed
/// request.
pub type ImportHistoryHandle = Option<Arc<dyn ImportHistory>>;

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

// ── What is configured, and when it last ran (RFC 0021 §6.5) ────────────────

/// One configured import, and its last run.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConfiguredImport {
    /// The source repository, `owner/name`.
    pub repo: String,
    /// The asset globs a release is matched against.
    pub assets: Vec<String>,
    /// The last completed run, or `null` when none has happened.
    ///
    /// The distinction the console exists to show: "ran and found nothing new"
    /// is a run with `imported: 0`, "has not run" is this being absent, and a
    /// page that could not tell them apart would be worse than no page.
    pub last_run: Option<batlehub_core::entities::ImportRun>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegistryImportsResponse {
    pub registry: String,
    pub imports: Vec<ConfiguredImport>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AllImportsResponse {
    pub registries: Vec<RegistryImports>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegistryImports {
    pub registry: String,
    pub imports: Vec<ConfiguredImport>,
}

/// Every configured import, with each one's last run (admin).
///
/// Unscoped, like `GET /api/v1/admin/warming` and for the same reason: the
/// console's first question is *which registries have imports at all*, and a
/// page that had to ask per registry would be N requests to render one table.
#[utoipa::path(
    get,
    path = "/api/v1/admin/imports",
    tag = "back-office",
    responses(
        (status = 200, description = "Every configured import and its last run", body = AllImportsResponse),
        (status = 403, description = "`cache:warm` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/imports")]
pub async fn list_all_imports(
    identity: AuthIdentity,
    imports: web::Data<ReleaseImportMap>,
    history: web::Data<ImportHistoryHandle>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // Instance tier: this spans registries, so there is no one registry to
    // scope the check to.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::CacheWarm,
        None,
        &hot,
    )
    .await?;

    let mut registries: Vec<RegistryImports> = Vec::with_capacity(imports.len());
    for (registry, configured) in imports.iter() {
        let latest = match history.as_ref().as_ref() {
            Some(h) => h.latest_for(registry).await.unwrap_or_default(),
            None => Vec::new(),
        };
        registries.push(RegistryImports {
            registry: registry.clone(),
            imports: configured
                .iter()
                .map(|svc| ConfiguredImport {
                    repo: svc.repo.clone(),
                    assets: svc.assets.clone(),
                    last_run: latest.iter().find(|r| r.repo == svc.repo).cloned(),
                })
                .collect(),
        });
    }
    // A stable order, or the table reshuffles on every refresh.
    registries.sort_by(|a, b| a.registry.cmp(&b.registry));
    Ok(web::Json(AllImportsResponse { registries }))
}

/// The imports configured into a registry, and each one's last run (admin).
#[utoipa::path(
    get,
    path = "/api/v1/admin/registries/{registry}/imports",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "The registry imported into"),
    ),
    responses(
        (status = 200, description = "Configured imports and their last runs", body = RegistryImportsResponse),
        (status = 403, description = "`cache:warm` required"),
        (status = 404, description = "No import is configured into this registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/registries/{registry}/imports")]
pub async fn list_registry_imports(
    path: web::Path<String>,
    identity: AuthIdentity,
    imports: web::Data<ReleaseImportMap>,
    history: web::Data<ImportHistoryHandle>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    // The same verb the POST asks for: reading what is configured and asking
    // for a run are the same operator decision, one step apart.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::CacheWarm,
        Some(&registry),
        &hot,
    )
    .await?;

    let configured = imports.get(&registry).ok_or_else(|| {
        AppError::not_found(format!(
            "no [[release_imports]] is configured into registry '{registry}'"
        ))
    })?;

    // One query for the registry rather than one per repo: a registry with
    // several imports would otherwise be N round trips to render one page.
    let latest = match history.as_ref().as_ref() {
        Some(h) => h.latest_for(&registry).await.unwrap_or_else(|e| {
            // A history that cannot be read is not a reason to refuse the
            // page: what is configured is still worth showing, and the last-run
            // column says "unknown" by being absent.
            tracing::warn!(error = %e, registry = %registry, "release import: history read failed");
            Vec::new()
        }),
        None => Vec::new(),
    };

    let imports = configured
        .iter()
        .map(|svc| ConfiguredImport {
            repo: svc.repo.clone(),
            assets: svc.assets.clone(),
            last_run: latest.iter().find(|r| r.repo == svc.repo).cloned(),
        })
        .collect();

    Ok(web::Json(RegistryImportsResponse { registry, imports }))
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
    history: web::Data<ImportHistoryHandle>,
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

    // Per repo, not per request: a registry with two imports configured into it
    // has two histories, and one row covering both would hide the one that has
    // been failing behind the one that has not.
    let mut report = ImportReport::default();
    for svc in selected {
        let started_at = chrono::Utc::now();
        let one = match req.tag.as_deref() {
            Some(tag) => svc.import_tag(tag).await,
            None => svc.import().await,
        };
        if let Some(history) = history.as_ref() {
            let run = ImportRun::from_report(
                &registry,
                &svc.repo,
                started_at,
                &one,
                identity.0.user_id.clone(),
            );
            // Fire-and-forget, as the scheduled path is: the versions are
            // published either way, and failing the request over a history row
            // would be the tail wagging the dog.
            if let Err(e) = history.record(&run).await {
                tracing::warn!(error = %e, "release import: history write failed");
            }
        }
        report += one;
    }
    tracing::info!(
        registry = %registry,
        imported = report.imported, skipped = report.skipped, errors = report.errors,
        asked_by = ?identity.0.user_id,
        "release import: run finished"
    );
    Ok(web::Json(ImportResponse::from(report)))
}
