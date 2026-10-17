use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{Action, PackageId, SbomFormat},
    error::CoreError,
    services::{authz, LocalRegistryService, SbomService},
};

use super::require_authenticated;
use crate::{error::AppError, extractors::AuthIdentity};

/// An SPDX 2.3 or CycloneDX 1.5 document.
///
/// The shape is the SBOM standard's, not this API's — which format you get is
/// decided by the `format` query parameter, and each is specified elsewhere.
#[derive(ToSchema)]
#[schema(value_type = Object)]
pub struct SbomDocument(pub serde_json::Value);

fn default_sbom_format() -> String {
    "spdx".to_owned()
}

// ── Per-artifact SBOM ─────────────────────────────────────────────────────────

#[derive(Deserialize, IntoParams)]
pub struct SbomQuery {
    /// SBOM format: `spdx` (default) or `cyclonedx`.
    #[serde(default = "default_sbom_format")]
    pub format: String,
}

/// Retrieve the SBOM for a specific artifact version.
#[utoipa::path(
    get,
    path = "/api/v1/sbom/{registry}/{name}/{version}",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Package name"),
        ("version"  = String, Path, description = "Package version"),
        SbomQuery,
    ),
    responses(
        (status = 200, description = "SBOM document (JSON)", body = SbomDocument),
        (status = 400, description = "Unknown format"),
        (status = 403, description = "Authentication required"),
        (status = 404, description = "No SBOM found for this artifact, or the caller cannot read it"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/sbom/{registry}/{name}/{version}")]
pub async fn get_artifact_sbom(
    path: web::Path<(String, String, String)>,
    query: web::Query<SbomQuery>,
    identity: AuthIdentity,
    sbom_svc: web::Data<Arc<SbomService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
    local_svc: web::Data<Arc<LocalRegistryService>>,
) -> Result<impl Responder, AppError> {
    // Requires at least an authenticated user (not anonymous).
    require_authenticated(&identity)?;

    let (registry, name, version) = path.into_inner();

    // An SBOM is the artifact's inventory, so it takes the artifact's read
    // gates: the grant the download path checks, then package visibility.
    // Both refuse as the `404` below would, so a hidden package is not
    // confirmed by the status code.
    let not_found = || {
        AppError::not_found(format!(
            "no SBOM found for {registry}/{name}/{version} (format: {})",
            query.format
        ))
    };
    // Only a refusal becomes the 404; a store failure stays a 5xx.
    let refused = |e: CoreError| match e {
        CoreError::NotFound(_) | CoreError::NotFoundWithheld(_) | CoreError::AccessDenied(_) => {
            not_found()
        }
        e => e.into(),
    };
    let pkg = PackageId::new(&registry, &name, &version);
    authz::authorize_grants_public(&hot, &pkg, &identity, Action::ReleasesRead)
        .await
        .map_err(refused)?;
    local_svc
        .check_visibility(&registry, &name, &identity)
        .await
        .map_err(refused)?;

    let format = SbomFormat::parse(&query.format)
        .ok_or_else(|| AppError::bad_request(format!("unknown SBOM format '{}'", query.format)))?;

    // Proxy artifact keys carry a registry-specific suffix (`/tarball`, `/dl`, …)
    // that we can't predict here, so look up by coordinates instead of key.
    let sbom = sbom_svc
        .get_artifact_sbom_by_coordinates(&registry, &name, &version, &format)
        .await
        .map_err(AppError::from)?;

    match sbom {
        Some(s) => Ok(HttpResponse::Ok()
            .content_type("application/json")
            .json(s.document)),
        None => Err(not_found()),
    }
}

// ── Org-level SBOM export ─────────────────────────────────────────────────────

#[derive(Deserialize, IntoParams)]
pub struct SbomExportQuery {
    /// Filter to a specific registry name.
    pub registry: Option<String>,
    /// Earliest artifact creation timestamp (inclusive).
    pub from: Option<DateTime<Utc>>,
    /// Latest artifact creation timestamp (inclusive).
    pub to: Option<DateTime<Utc>>,
    /// SBOM format: `spdx` (default) or `cyclonedx`.
    #[serde(default = "default_sbom_format")]
    pub format: String,
}

/// Export an org-level SBOM covering all artifacts served in a time range.
#[utoipa::path(
    get,
    path = "/api/v1/sbom/export",
    tag = "back-office",
    params(SbomExportQuery),
    responses(
        (status = 200, description = "Merged SBOM document (JSON attachment)", body = SbomDocument),
        (status = 400, description = "Unknown format"),
        (status = 403, description = "`system:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/sbom/export")]
pub async fn export_org_sbom(
    query: web::Query<SbomExportQuery>,
    identity: AuthIdentity,
    sbom_svc: web::Data<Arc<SbomService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::SystemRead,
        None,
        &hot,
    )
    .await?;

    let format = SbomFormat::parse(&query.format)
        .ok_or_else(|| AppError::bad_request(format!("unknown SBOM format '{}'", query.format)))?;

    let ext = match format {
        SbomFormat::Spdx => "spdx.json",
        SbomFormat::CycloneDx => "cyclonedx.json",
    };

    let ts = Utc::now().format("%Y%m%d%H%M%S");
    let registry_label = query.registry.as_deref().unwrap_or("all");
    let filename = format!("sbom-export-{registry_label}-{ts}.{ext}");

    let document = sbom_svc
        .export_org_sbom(query.registry.as_deref(), query.from, query.to, &format)
        .await
        .map_err(AppError::from)?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{filename}\""),
        ))
        .json(document))
}
