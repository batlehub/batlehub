use std::sync::Arc;

use actix_web::{get, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{PackageFilter, PackageId, PackageStatus},
    services::AdminService,
};

use crate::{error::AppError, extractors::AuthIdentity, RegistryHostMap};

#[derive(Deserialize, IntoParams)]
pub struct PackageQuery {
    pub registry: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}

fn default_per_page() -> u64 {
    50
}

#[derive(Serialize, ToSchema)]
pub struct PackageListResponse {
    pub items: Vec<PackageSummaryDto>,
    pub total: usize,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Serialize, ToSchema)]
pub struct PackageSummaryDto {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
    pub status: PackageStatusDto,
    pub access_count: u64,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PackageStatusDto {
    Available,
    Blocked { reason: String },
}

/// List packages visible to the current user.
///
/// Only packages from registries the caller's role can access are returned.
/// Blocked packages are shown with their block reason.
#[utoipa::path(
    get,
    path = "/api/v1/packages",
    operation_id = "list_visible_packages",
    tag = "front-office",
    params(PackageQuery),
    responses(
        (status = 200, description = "Package listing", body = PackageListResponse),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/packages")]
pub async fn list_packages(
    query: web::Query<PackageQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    access: web::Data<crate::AccessConfigLock>,
) -> Result<impl Responder, AppError> {
    let accessible = access.read().await.accessible_registries_for(&identity);

    // An empty accessible set is **nothing**, not "no restriction".
    //
    // `PackageFilter::registries` is a scope, and every implementation of it
    // reads an empty vector as unfiltered: `prepare_registries_param` binds
    // `NULL` and the SQL is `$7::text[] IS NULL OR ps.registry = ANY($7)`
    // (`db/packages/crud.rs`), the in-memory repository is
    // `filter.registries.is_empty() || contains(…)`. That overload is load-
    // bearing — the branch below deliberately passes `vec![]` to mean "no list
    // restriction" once `query.registry` has been checked — so a caller who can
    // reach *no* registry produced the same empty vector and was handed the
    // entire catalogue by the endpoint whose whole job is to scope it.
    //
    // Refused here, before the scope is built, rather than by teaching four
    // repositories to tell "all" from "none" apart. Mirrors the identical guard
    // in `explore/list.rs`, which is the same trap on the sibling endpoint.
    let denied_everywhere = accessible.is_empty();
    let named_registry_denied = query
        .registry
        .as_ref()
        .is_some_and(|reg| !accessible.contains(reg));
    if denied_everywhere || named_registry_denied {
        return Ok(web::Json(PackageListResponse {
            items: vec![],
            total: 0,
            page: query.page,
            per_page: query.per_page,
        }));
    }

    // When no specific registry is requested, restrict to accessible registries at the DB level
    // so that pagination and the total count are accurate.
    let registries = if query.registry.is_none() {
        accessible.into_iter().collect()
    } else {
        vec![]
    };

    // Registry access is the coarse gate; this is the per-package one. A local
    // download writes a `package_statuses` row, so without it a `team`/`internal`
    // package's name and version are listed to callers its download refuses —
    // the same leak `explore/list.rs` closes with the same viewer.
    let viewer = Some(crate::handlers::explore_viewer_for(&identity));

    let (page, per_page) = crate::handlers::clamp_pagination(query.page, query.per_page);
    let filter = PackageFilter {
        registry: query.registry.clone(),
        registries: registries.clone(),
        name_contains: query.name.clone(),
        name_exact: None,
        blocked_only: false,
        limit: per_page,
        offset: page * per_page,
        viewer: viewer.clone(),
    };

    let count_filter = PackageFilter {
        registry: query.registry.clone(),
        registries,
        name_contains: query.name.clone(),
        name_exact: None,
        blocked_only: false,
        limit: 0,
        offset: 0,
        viewer,
    };

    let (packages, total) = tokio::try_join!(
        admin_svc.list_packages(filter),
        admin_svc.count_packages(count_filter),
    )
    .map_err(AppError::from)?;

    let items: Vec<PackageSummaryDto> = packages
        .into_iter()
        .map(|p| PackageSummaryDto {
            registry: p.package_id.registry,
            name: p.package_id.name,
            version: p.package_id.version,
            artifact: p.package_id.artifact,
            status: match p.status {
                PackageStatus::Available => PackageStatusDto::Available,
                PackageStatus::Blocked { reason, .. } => PackageStatusDto::Blocked { reason },
            },
            access_count: p.access_count,
        })
        .collect();

    Ok(web::Json(PackageListResponse {
        items,
        total: total as usize,
        page: query.page,
        per_page: query.per_page,
    }))
}

#[derive(Deserialize, IntoParams)]
pub struct AccessQuery {
    registry: String,
    name: String,
    version: String,
    artifact: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AccessCheckResponse {
    pub package: PackageIdentifierDto,
    pub can_access: bool,
    pub reason: Option<String>,
    /// The proxy path for this package (e.g. `/proxy/npm/lodash/4.17.21/tarball`).
    pub proxy_url: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PackageIdentifierDto {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
}

/// Check why a specific package is or isn't accessible for the current user.
#[utoipa::path(
    get,
    path = "/api/v1/packages/access",
    tag = "front-office",
    params(AccessQuery),
    responses(
        (status = 200, description = "Access check result", body = AccessCheckResponse),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/packages/access")]
pub async fn check_access(
    query: web::Query<AccessQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    access: web::Data<crate::AccessConfigLock>,
    hosts: Option<web::Data<RegistryHostMap>>,
) -> Result<impl Responder, AppError> {
    let pkg = PackageId {
        registry: query.registry.clone(),
        name: query.name.clone(),
        version: query.version.clone(),
        artifact: query.artifact.clone(),
    };

    let accessible = access.read().await.accessible_registries_for(&identity);
    if !accessible.contains(&pkg.registry) {
        return Ok(web::Json(AccessCheckResponse {
            package: PackageIdentifierDto {
                registry: pkg.registry,
                name: pkg.name,
                version: pkg.version,
                artifact: pkg.artifact,
            },
            can_access: false,
            reason: Some("registry not accessible".to_string()),
            proxy_url: None,
        }));
    }

    let status = admin_svc
        .get_package_status(&pkg)
        .await
        .map_err(AppError::from)?;

    let (can_access, reason) = match &status {
        PackageStatus::Available => (true, None),
        PackageStatus::Blocked { reason, .. } => (false, Some(reason.clone())),
    };

    let proxy_url = build_proxy_url(
        &registry_link_base(&pkg.registry, hosts.as_ref().map(|h| h.get_ref())),
        &pkg.registry,
        &pkg.name,
        &pkg.version,
        pkg.artifact.as_deref(),
    );

    Ok(web::Json(AccessCheckResponse {
        package: PackageIdentifierDto {
            registry: pkg.registry,
            name: pkg.name,
            version: pkg.version,
            artifact: pkg.artifact,
        },
        can_access,
        reason,
        proxy_url,
    }))
}

/// The prefix package links for `registry` hang off.
///
/// Normally the SPA-relative `/proxy/{registry}`. For a registry with
/// `path_routing = false` that path does not exist, so the link has to be the
/// registry's absolute public URL instead — otherwise every package link in the
/// admin UI 404s for exactly the registries an operator chose to isolate.
fn registry_link_base(registry: &str, hosts: Option<&RegistryHostMap>) -> String {
    hosts
        .filter(|h| h.is_host_only(registry))
        .and_then(|h| h.public_url_for(registry))
        .unwrap_or_else(|| format!("/proxy/{registry}"))
}

fn build_proxy_url(
    base: &str,
    registry: &str,
    name: &str,
    version: &str,
    artifact: Option<&str>,
) -> Option<String> {
    match registry {
        "github" => Some(match (version, artifact) {
            ("releases", _) => format!("{base}/{name}/releases"),
            (v, Some(art)) if art.starts_with("tarball") => {
                format!("{base}/{name}/tarball/{v}")
            }
            (v, Some("zipball")) => format!("{base}/{name}/zipball/{v}"),
            (v, Some(art)) if art.starts_with("raw/") => {
                let path = art.strip_prefix("raw/").unwrap_or("");
                format!("{base}/{name}/raw/{v}/{path}")
            }
            (_, Some(art)) => format!("{base}/{name}/releases/assets/{art}"),
            (v, None) => format!("{base}/{name}/releases/tags/{v}"),
        }),
        "npm" => Some(match artifact {
            Some("tarball") => format!("{base}/{name}/{version}/tarball"),
            _ => format!("{base}/{name}/{version}"),
        }),
        "cargo" => Some(match artifact {
            Some("download") => format!("{base}/{name}/{version}/download"),
            _ => format!("{base}/{name}/{version}"),
        }),
        _ => None,
    }
}
