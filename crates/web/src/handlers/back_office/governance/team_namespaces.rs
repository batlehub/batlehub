use std::sync::Arc;

use actix_web::{delete, get, post, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{AccessAction, Role, TeamNamespace, Visibility},
    ports::TeamNamespacePort,
    services::AdminService,
};

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Debug, Serialize, ToSchema)]
pub struct TeamNamespaceDto {
    pub registry: String,
    pub prefix: String,
    pub group_id: String,
    pub claimed_by: Option<String>,
    /// How many packages the namespace currently holds (RFC 0004-bis A6).
    ///
    /// `count_packages_in_namespace` has been on `TeamNamespacePort` since the
    /// port was written and had exactly one caller — the *delete* confirmation.
    /// The list showed a row per claim with no way to tell a namespace holding
    /// four hundred packages from an abandoned one, which is the question an
    /// operator opens this page with.
    pub package_count: u64,
}

impl TeamNamespaceDto {
    fn new(ns: TeamNamespace, package_count: u64) -> Self {
        Self {
            registry: ns.registry,
            prefix: ns.prefix,
            group_id: ns.group_id,
            claimed_by: ns.claimed_by,
            package_count,
        }
    }
}

/// Attach a package count to every namespace, counting concurrently.
///
/// A claim list is a handful of rows, so this is the same shape the health
/// endpoint uses for its per-registry storage stats. A count that fails becomes
/// `0` rather than failing the listing: an operator who cannot see their claims
/// at all is worse off than one seeing a count they can refresh.
async fn with_counts(
    store: &Arc<dyn TeamNamespacePort>,
    namespaces: Vec<TeamNamespace>,
) -> Vec<TeamNamespaceDto> {
    let counts = futures::future::join_all(
        namespaces
            .iter()
            .map(|ns| store.count_packages_in_namespace(&ns.registry, &ns.prefix)),
    )
    .await;

    namespaces
        .into_iter()
        .zip(counts)
        .map(|(ns, count)| TeamNamespaceDto::new(ns, count.unwrap_or(0)))
        .collect()
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ClaimNamespaceRequest {
    pub prefix: String,
    pub group_id: String,
    pub claimed_by: Option<String>,
}

/// List team namespace claims for a registry.
#[utoipa::path(
    get,
    path = "/api/v1/admin/registries/{registry}/namespaces",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Namespace list", body = Vec<TeamNamespaceDto>),
        (status = 403, description = "`owners:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/registries/{registry}/namespaces")]
pub async fn list_namespaces(
    path: web::Path<(String,)>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn TeamNamespacePort>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let (registry,) = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::OwnersRead,
        Some(&registry),
        &hot,
    )
    .await?;
    let namespaces = store
        .list_namespaces(&registry)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(with_counts(&store, namespaces).await))
}

/// Claim a namespace prefix for a team group.
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/namespaces",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = ClaimNamespaceRequest,
    responses(
        (status = 204, description = "Namespace claimed"),
        (status = 403, description = "`owners:write` required"),
        (status = 409, description = "Prefix already claimed"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/namespaces")]
pub async fn claim_namespace(
    path: web::Path<(String,)>,
    body: web::Json<ClaimNamespaceRequest>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn TeamNamespacePort>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let (registry,) = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::OwnersWrite,
        Some(&registry),
        &hot,
    )
    .await?;
    if body.prefix.is_empty() {
        return Err(AppError::bad_request("prefix must not be empty"));
    }
    if body.group_id.is_empty() {
        return Err(AppError::bad_request("group_id must not be empty"));
    }
    let group_id = body.group_id.replace(' ', "");
    // RFC 0015 §4.1 — the claim records the ecosystem's separator, taken from the
    // registry it is being made on.
    //
    // The claim is where it is stored (see `TeamNamespace::separator`), and this
    // is the only place it is decided. A registry with no configured hierarchy
    // falls back to `/`, which is what every claim matched before the column
    // existed — the conservative answer for a name this server cannot classify.
    let separator = {
        let hot = hot.read().await;
        hot.grants
            .get(&registry)
            .map(|g| batlehub_core::entities::namespace_separator(g.kind))
            .unwrap_or('/')
    };
    store
        .claim_namespace(TeamNamespace {
            registry,
            prefix: body.prefix.clone(),
            group_id,
            claimed_by: body.claimed_by.clone(),
            separator,
        })
        .await
        .map_err(AppError::from)?;

    admin_svc
        .record_account_action(AccessAction::ClaimNamespace, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::NoContent().finish())
}

/// Release a team namespace claim.
///
/// The prefix may contain slashes (e.g. `"frontend/libs"`).
#[utoipa::path(
    delete,
    path = "/api/v1/admin/registries/{registry}/namespaces/{prefix}",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("prefix"   = String, Path, description = "Namespace prefix (may contain slashes)"),
    ),
    responses(
        (status = 204, description = "Namespace released (or did not exist)"),
        (status = 403, description = "`owners:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/registries/{registry}/namespaces/{prefix:.*}")]
pub async fn release_namespace(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn TeamNamespacePort>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let (registry, prefix) = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::OwnersWrite,
        Some(&registry),
        &hot,
    )
    .await?;
    store
        .release_namespace(&registry, &prefix)
        .await
        .map_err(AppError::from)?;

    admin_svc
        .record_account_action(AccessAction::ReleaseNamespace, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::NoContent().finish())
}

// ── User-facing endpoints ────────────────────────────────────────────────────

/// List all namespace claims owned by the caller's groups (across all registries).
#[utoipa::path(
    get,
    path = "/api/v1/me/namespaces",
    tag = "user",
    responses(
        (status = 200, description = "Namespaces owned by the caller's groups", body = Vec<TeamNamespaceDto>),
        (status = 403, description = "Authentication required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/me/namespaces")]
pub async fn my_namespaces(
    identity: AuthIdentity,
    store: web::Data<Arc<dyn TeamNamespacePort>>,
) -> Result<impl Responder, AppError> {
    if !identity.has_role_at_least(&Role::User) {
        return Err(AppError::forbidden("authentication required"));
    }
    let normalized_groups: Vec<String> =
        identity.groups.iter().map(|g| g.replace(' ', "")).collect();
    let namespaces = store
        .list_namespaces_for_groups(&normalized_groups)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(with_counts(&store, namespaces).await))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct NamespacePackagesQuery {
    #[serde(default)]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}

fn default_per_page() -> u64 {
    50
}

/// Paginated envelope for `GET /api/v1/me/namespaces/{registry}/{prefix}/packages`,
/// matching the shape of its sibling list endpoints (`AdminPackageListResponse`)
/// instead of returning a bare array with no way to tell if more pages exist.
#[derive(Debug, Serialize, ToSchema)]
pub struct NamespacePackageListResponse {
    pub items: Vec<NamespacePackageDto>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NamespacePackageDto {
    pub name: String,
    pub version: String,
    pub visibility: Visibility,
    pub published_by: String,
    pub published_at: DateTime<Utc>,
    pub yanked: bool,
}

/// List published packages under a namespace prefix.
///
/// Accessible by admins and members of the group owning the namespace prefix.
#[utoipa::path(
    get,
    path = "/api/v1/me/namespaces/{registry}/{prefix}/packages",
    tag = "user",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("prefix"   = String, Path, description = "Namespace prefix (may contain slashes)"),
        NamespacePackagesQuery,
    ),
    responses(
        (status = 200, description = "Package list", body = NamespacePackageListResponse),
        (status = 403, description = "Authentication or group membership required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/me/namespaces/{registry}/{prefix:.*}/packages")]
pub async fn my_namespace_packages(
    path: web::Path<(String, String)>,
    query: web::Query<NamespacePackagesQuery>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn TeamNamespacePort>>,
) -> Result<impl Responder, AppError> {
    if !identity.has_role_at_least(&Role::User) {
        return Err(AppError::forbidden("authentication required"));
    }
    let (registry, prefix) = path.into_inner();

    // Admins can query any namespace; regular users must be in the owning group.
    if identity.role != Role::Admin {
        let ns = store
            .find_namespace(&registry, &prefix)
            .await
            .map_err(AppError::from)?;
        match ns {
            Some(ns)
                if identity
                    .groups
                    .iter()
                    .any(|g| g.replace(' ', "") == ns.group_id.replace(' ', "")) => {}
            Some(ns) => {
                return Err(AppError::forbidden(format!(
                    "namespace '{}' is owned by group '{}'; you are not a member",
                    ns.prefix, ns.group_id
                )));
            }
            None => return Err(AppError::forbidden("admin role required")),
        }
    }

    let (page, per_page) = crate::handlers::clamp_pagination(query.page, query.per_page);
    let limit = per_page;
    let offset = page * per_page;
    let (packages, total) = tokio::try_join!(
        store.list_packages_in_namespace(&registry, &prefix, limit, offset),
        store.count_packages_in_namespace(&registry, &prefix),
    )
    .map_err(AppError::from)?;
    let items: Vec<NamespacePackageDto> = packages
        .into_iter()
        .map(|p| NamespacePackageDto {
            name: p.name,
            version: p.version,
            visibility: p.visibility,
            published_by: p.published_by,
            published_at: p.published_at,
            yanked: p.yanked,
        })
        .collect();
    Ok(HttpResponse::Ok().json(NamespacePackageListResponse {
        items,
        total,
        page: query.page,
        per_page: query.per_page,
    }))
}
