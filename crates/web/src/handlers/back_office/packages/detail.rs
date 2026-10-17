use std::sync::Arc;

use actix_web::{get, web, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use batlehub_core::{
    entities::{AccessResult, ArtifactVulnerability, EventFilter, PackageFilter, PackageStatus},
    ports::StorageAdminRepository,
    services::{AdminService, ProxyService, SbomService},
};

use crate::{badges::socket_badge_url, error::AppError, extractors::AuthIdentity, RegistryMap};

// ── Vulnerability finding ─────────────────────────────────────────────────────

/// A known vulnerability affecting a package version, surfaced from the periodic
/// SBOM re-scan. Shared by the admin and explore package-detail views.
#[derive(Serialize, ToSchema, Clone)]
pub struct VulnerabilityDto {
    pub osv_id: String,
    /// `unknown` | `low` | `medium` | `high` | `critical`
    pub severity: String,
    pub summary: String,
    pub fixed_version: Option<String>,
    pub purl: String,
}

impl From<ArtifactVulnerability> for VulnerabilityDto {
    fn from(v: ArtifactVulnerability) -> Self {
        Self {
            osv_id: v.osv_id,
            severity: v.severity.as_str().to_owned(),
            summary: v.summary,
            fixed_version: v.fixed_version,
            purl: v.purl,
        }
    }
}

// ── Package detail ────────────────────────────────────────────────────────────

#[derive(Deserialize, IntoParams)]
pub struct PackageDetailQuery {
    pub registry: String,
    pub name: String,
}

#[derive(Serialize, ToSchema)]
pub struct PackageVersionDetail {
    pub id: Uuid,
    pub version: String,
    pub artifact: Option<String>,
    pub status: PackageStatusDetail,
    pub storage_key: String,
    pub cached: bool,
    /// Name of the storage backend holding this artifact (null if not yet cached or pre-migration).
    pub storage_backend: Option<String>,
    /// When the artifact was first stored in the cache (null if not yet cached or pre-migration).
    pub cached_at: Option<DateTime<Utc>>,
    pub access_count: u64,
    pub last_accessed: Option<DateTime<Utc>>,
    pub last_accessed_by: Option<String>,
    /// Known vulnerabilities for this version (from the periodic SBOM re-scan).
    pub vulnerabilities: Vec<VulnerabilityDto>,
    /// The licence this version's own manifest declared, verbatim.
    ///
    /// Null means *unknown*, never "unlicensed": the licence is read out of the
    /// archive when it is cached or published, so it is absent for anything
    /// fetched before extraction existed, and for the sixteen registry types
    /// with no manifest parser. `license_gate.allow_unknown` decides how a
    /// null is treated (RFC 0004-bis §13.1).
    pub license: Option<String>,
    /// socket.dev badge URL when the `socket_badge` feature flag is enabled for
    /// this registry and the registry type is covered by socket.dev; else null.
    pub socket_badge_url: Option<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PackageStatusDetail {
    Available,
    Blocked {
        reason: String,
        blocked_by: String,
        blocked_at: DateTime<Utc>,
    },
}

#[derive(Serialize, ToSchema)]
pub struct PackageEventDto {
    pub id: Uuid,
    pub user_id: Option<String>,
    pub user_role: String,
    pub version: String,
    pub artifact: Option<String>,
    pub action: String,
    pub outcome: String,
    pub deny_reason: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct PackageDetailResponse {
    pub registry: String,
    pub name: String,
    pub versions: Vec<PackageVersionDetail>,
    pub recent_events: Vec<PackageEventDto>,
}

/// What every version row on this page needs, resolved once per request.
///
/// Gathered into one struct rather than passed as seven arguments: they are all
/// per-request context, and a row builder that takes them positionally is one
/// transposition away from reading the wrong registry.
struct VersionDetailCtx<'a> {
    query: &'a PackageDetailQuery,
    admin_svc: &'a Arc<AdminService>,
    proxy_svc: &'a Arc<ProxyService>,
    storage_admin_repo: Option<&'a Arc<dyn StorageAdminRepository>>,
    sbom_svc: Option<&'a Arc<SbomService>>,
    registry_type: Option<String>,
    socket_badge_enabled: bool,
}

/// One version's row: where its bytes are, what it is flagged as, and the
/// decoration the page hangs off it.
async fn version_detail(
    ctx: &VersionDetailCtx<'_>,
    s: batlehub_core::entities::PackageSummary,
) -> PackageVersionDetail {
    let storage_key = format!("artifact:{}", s.package_id.cache_key());
    let cached = ctx
        .proxy_svc
        .storage
        .exists(&storage_key)
        .await
        .unwrap_or(false);
    let (storage_backend, cached_at) = match ctx.storage_admin_repo {
        Some(repo) => match repo.find_by_key(&storage_key).await.ok().flatten() {
            Some(r) => (Some(r.backend_name), Some(r.stored_at)),
            None => (None, None),
        },
        None => (None, None),
    };
    let status = match s.status {
        PackageStatus::Available => PackageStatusDetail::Available,
        PackageStatus::Blocked {
            reason,
            blocked_by,
            blocked_at,
        } => PackageStatusDetail::Blocked {
            reason,
            blocked_by,
            blocked_at,
        },
    };
    let vulnerabilities = ctx
        .admin_svc
        .list_vulnerabilities(&ctx.query.registry, &ctx.query.name, &s.package_id.version)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(VulnerabilityDto::from)
        .collect();
    let socket_badge_url = ctx
        .socket_badge_enabled
        .then(|| {
            ctx.registry_type
                .as_deref()
                .and_then(|t| socket_badge_url(t, &ctx.query.name, &s.package_id.version))
        })
        .flatten();
    // A failed lookup reads as unknown rather than propagating: the licence
    // is decoration on this page, and an SBOM outage should not take the
    // package detail with it. The gate is where a lookup failure matters,
    // and it logs its own.
    let license = match ctx.sbom_svc {
        Some(svc) => svc
            .repo
            .get_license_for_coordinate(&ctx.query.registry, &ctx.query.name, &s.package_id.version)
            .await
            .unwrap_or(None),
        None => None,
    };
    PackageVersionDetail {
        id: s.id,
        version: s.package_id.version,
        artifact: s.package_id.artifact,
        status,
        storage_key,
        cached,
        storage_backend,
        cached_at,
        access_count: s.access_count,
        last_accessed: s.last_accessed,
        last_accessed_by: s.last_accessed_by,
        vulnerabilities,
        license,
        socket_badge_url,
    }
}

/// An event's outcome word, and the reason when there is one.
fn event_outcome(result: AccessResult) -> (String, Option<String>) {
    match result {
        AccessResult::Allowed => ("allowed".to_string(), None),
        AccessResult::Denied { reason } => ("denied".to_string(), Some(reason)),
        AccessResult::ProxyError { reason } => ("error".to_string(), Some(reason)),
    }
}

/// Get detailed information about a specific package (all versions, access history, cache status).
#[utoipa::path(
    get,
    path = "/api/v1/admin/packages/detail",
    tag = "back-office",
    params(PackageDetailQuery),
    responses(
        (status = 200, description = "Package detail", body = PackageDetailResponse),
        (status = 403, description = "`packages:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/packages/detail")]
// Eight extractors: the six this handler already had, plus `hot` for the
// `packages:read` check that replaced `require_admin`. The alternative is a
// bundle struct whose only purpose is to satisfy a lint about a signature actix
// generates the wiring for.
#[allow(clippy::too_many_arguments)]
pub async fn package_detail(
    query: web::Query<PackageDetailQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    registry_map: web::Data<RegistryMap>,
    storage_admin_repo: Option<web::Data<Arc<dyn StorageAdminRepository>>>,
    sbom_svc: Option<web::Data<Arc<SbomService>>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::PackagesRead,
        None,
        &hot,
    )
    .await?;

    // socket.dev badge: enabled per registry via feature flag, mapped by type.
    let socket_badge_enabled = proxy_svc
        .hot
        .read()
        .await
        .feature_flags
        .get(&query.registry)
        .is_none_or(|f| f.socket_badge);
    let registry_type = registry_map.type_of(&query.registry);

    let filter = PackageFilter {
        registry: Some(query.registry.clone()),
        registries: vec![],
        name_exact: Some(query.name.clone()),
        name_contains: None,
        blocked_only: false,
        limit: 200,
        offset: 0,
        viewer: None,
    };
    let summaries = admin_svc
        .list_packages(filter)
        .await
        .map_err(AppError::from)?;

    let ctx = VersionDetailCtx {
        query: &query,
        admin_svc: &admin_svc,
        proxy_svc: &proxy_svc,
        storage_admin_repo: storage_admin_repo.as_deref().map(|v| &**v),
        sbom_svc: sbom_svc.as_deref().map(|v| &**v),
        registry_type,
        socket_badge_enabled,
    };
    let mut versions = Vec::with_capacity(summaries.len());
    for s in summaries {
        versions.push(version_detail(&ctx, s).await);
    }

    let event_filter = EventFilter {
        registry: Some(query.registry.clone()),
        package_name: Some(query.name.clone()),
        user_id: None,
        actions: vec![],
        from: None,
        to: None,
        denied_only: false,
        limit: 50,
        offset: 0,
    };
    let events = admin_svc
        .list_events(event_filter)
        .await
        .map_err(AppError::from)?;

    let recent_events = events
        .into_iter()
        .map(|e| {
            let (outcome, deny_reason) = event_outcome(e.result);
            let action = e.action.as_str();
            // `event_filter` above always sets `registry`/`package_name`, so any
            // event matching it has a package coordinate; the fallback only
            // matters if that invariant ever changes.
            let (version, artifact) = match e.package_id {
                Some(pkg) => (pkg.version, pkg.artifact),
                None => (String::new(), None),
            };
            PackageEventDto {
                id: e.id,
                user_id: e.user_id,
                user_role: e.user_role.to_string(),
                version,
                artifact,
                action: action.to_string(),
                outcome,
                deny_reason,
                timestamp: e.timestamp,
            }
        })
        .collect();

    Ok(web::Json(PackageDetailResponse {
        registry: query.registry.clone(),
        name: query.name.clone(),
        versions,
        recent_events,
    }))
}
