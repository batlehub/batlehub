use std::sync::Arc;

use actix_web::{get, put, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::{
    entities::{AccessAction, PackageId, Role, Visibility},
    ports::TeamNamespacePort,
    services::AdminService,
};

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetVisibilityRequest {
    pub visibility: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct VisibilityResponse {
    /// The three-valued enum, not a free string: the console narrowed this by
    /// hand until RFC 0004, and a narrowing the client invents is one the
    /// server can break without noticing.
    pub visibility: Visibility,
}

/// Passes when the caller is an admin, or is a member of the group that owns
/// the namespace covering `package` in `registry`.
async fn require_admin_or_namespace_member(
    identity: &AuthIdentity,
    store: &Arc<dyn TeamNamespacePort>,
    registry: &str,
    package: &str,
) -> Result<(), AppError> {
    if identity.role == Role::Admin {
        return Ok(());
    }
    if !identity.has_role_at_least(&Role::User) {
        return Err(AppError::forbidden("authentication required"));
    }
    match store
        .find_namespace(registry, package)
        .await
        .map_err(AppError::from)?
    {
        Some(ns)
            if identity
                .groups
                .iter()
                .any(|g| g.replace(' ', "") == ns.group_id.replace(' ', "")) =>
        {
            Ok(())
        }
        Some(ns) => Err(AppError::forbidden(format!(
            "package namespace '{}' is owned by group '{}'; you are not a member",
            ns.prefix, ns.group_id
        ))),
        None => Err(AppError::forbidden("admin role required")),
    }
}

/// Get the visibility of a package.
///
/// Accessible by admins and members of the team that owns the package namespace.
/// The package name may contain slashes (e.g. `"frontend/utils"`).
#[utoipa::path(
    get,
    path = "/api/v1/admin/registries/{registry}/packages/{name}/visibility",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Package name (may contain slashes)"),
    ),
    responses(
        (status = 200, description = "Current visibility", body = VisibilityResponse),
        (status = 403, description = "Admin role or namespace membership required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/registries/{registry}/packages/{name:.*}/visibility")]
pub async fn get_package_visibility(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn TeamNamespacePort>>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    require_admin_or_namespace_member(&identity, &store, &registry, &name).await?;
    let vis = store
        .get_visibility(&registry, &name)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(VisibilityResponse { visibility: vis }))
}

/// Set the visibility of a package (all versions simultaneously).
///
/// Accessible by admins and members of the team that owns the package namespace.
/// Accepted values: `"public"`, `"internal"`, `"team"`.
/// The package name may contain slashes (e.g. `"frontend/utils"`).
#[utoipa::path(
    put,
    path = "/api/v1/admin/registries/{registry}/packages/{name}/visibility",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name"     = String, Path, description = "Package name (may contain slashes)"),
    ),
    request_body = SetVisibilityRequest,
    responses(
        (status = 204, description = "Visibility updated"),
        (status = 400, description = "Invalid visibility value"),
        (status = 403, description = "Admin role or namespace membership required"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/api/v1/admin/registries/{registry}/packages/{name:.*}/visibility")]
pub async fn set_package_visibility(
    path: web::Path<(String, String)>,
    body: web::Json<SetVisibilityRequest>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn TeamNamespacePort>>,
    admin_svc: web::Data<Arc<AdminService>>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    require_admin_or_namespace_member(&identity, &store, &registry, &name).await?;
    let vis: Visibility = body
        .visibility
        .parse()
        .map_err(|e: String| AppError::bad_request(e))?;
    store
        .set_visibility(&registry, &name, vis)
        .await
        .map_err(AppError::from)?;

    // Visibility applies to all versions at once, so — like ownership — the
    // audit event carries an empty version to mean "the package as a whole".
    let pkg = PackageId {
        registry,
        name,
        version: String::new(),
        artifact: None,
    };
    admin_svc
        .record_package_action(&pkg, AccessAction::SetVisibility, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::NoContent().finish())
}
