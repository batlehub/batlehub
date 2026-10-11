use std::sync::Arc;

use actix_web::{delete, get, web, HttpResponse, Responder};
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_core::{
    entities::AccessAction,
    ports::QuotaUsage,
    services::{AdminService, QuotaService},
};

use crate::{error::AppError, extractors::AuthIdentity, handlers::schemas::OkResponse};

#[derive(Serialize, ToSchema)]
pub struct QuotaUsageDto {
    pub user_id: String,
    pub registry: String,
    pub bytes_published: u64,
    pub packages_count: u32,
}

impl From<QuotaUsage> for QuotaUsageDto {
    fn from(u: QuotaUsage) -> Self {
        Self {
            user_id: u.user_id,
            registry: u.registry,
            bytes_published: u.bytes_published,
            packages_count: u.packages_count,
        }
    }
}

/// List quota usage for all users across all registries.
#[utoipa::path(
    get,
    path = "/api/v1/admin/quota",
    tag = "back-office",
    responses(
        (status = 200, description = "All quota usage rows", body = Vec<QuotaUsageDto>),
        (status = 403, description = "`quota:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/quota")]
pub async fn list_quota(
    identity: AuthIdentity,
    quota_svc: web::Data<Arc<QuotaService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::QuotaRead,
        None,
        &hot,
    )
    .await?;
    let rows = quota_svc.list_usage(None).await.map_err(AppError::from)?;
    let dtos: Vec<QuotaUsageDto> = rows.into_iter().map(Into::into).collect();
    Ok(HttpResponse::Ok().json(dtos))
}

/// List quota usage for all users in a specific registry.
#[utoipa::path(
    get,
    path = "/api/v1/admin/quota/{registry}",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Quota usage rows for registry", body = Vec<QuotaUsageDto>),
        (status = 403, description = "`quota:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/quota/{registry}")]
pub async fn list_quota_for_registry(
    path: web::Path<String>,
    identity: AuthIdentity,
    quota_svc: web::Data<Arc<QuotaService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::QuotaRead,
        Some(&registry),
        &hot,
    )
    .await?;
    let rows = quota_svc
        .list_usage(Some(&registry))
        .await
        .map_err(AppError::from)?;
    let dtos: Vec<QuotaUsageDto> = rows.into_iter().map(Into::into).collect();
    Ok(HttpResponse::Ok().json(dtos))
}

/// Get quota usage for a specific user in a registry.
#[utoipa::path(
    get,
    path = "/api/v1/admin/quota/{registry}/{user_id}",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("user_id"  = String, Path, description = "User identifier"),
    ),
    responses(
        (status = 200, description = "Quota usage for the user", body = QuotaUsageDto),
        (status = 403, description = "`quota:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/quota/{registry}/{user_id}")]
pub async fn get_quota_for_user(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    quota_svc: web::Data<Arc<QuotaService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let (registry, user_id) = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::QuotaRead,
        Some(&registry),
        &hot,
    )
    .await?;
    let usage = quota_svc
        .get_usage(&user_id, &registry)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(QuotaUsageDto::from(usage)))
}

/// Reset quota usage for a specific user in a registry.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/quota/{registry}/{user_id}",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("user_id"  = String, Path, description = "User identifier"),
    ),
    responses(
        (status = 200, description = "Quota reset", body = OkResponse),
        (status = 403, description = "`quota:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/quota/{registry}/{user_id}")]
pub async fn reset_quota_for_user(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    quota_svc: web::Data<Arc<QuotaService>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let (registry, user_id) = path.into_inner();
    // **`quota:write`, not `quota:read`.** The three reads above ask for the
    // read verb; this one zeroes the counters. Every other control surface in
    // the vocabulary splits the two — `config:*`, `system:*`, `blocks:*` — and
    // quota was the one that did not, so a support engineer granted the read to
    // inspect usage could also defeat the limit on every user in the registry.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::QuotaWrite,
        Some(&registry),
        &hot,
    )
    .await?;
    quota_svc
        .reset(&user_id, &registry)
        .await
        .map_err(AppError::from)?;

    admin_svc
        .record_account_action(AccessAction::ResetQuota, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

#[cfg(test)]
mod tests {
    use super::QuotaUsageDto;
    use batlehub_core::ports::QuotaUsage;

    #[test]
    fn quota_usage_dto_conversion() {
        let usage = QuotaUsage {
            user_id: "alice".into(),
            registry: "cargo".into(),
            bytes_published: 1024,
            packages_count: 3,
        };
        let dto = QuotaUsageDto::from(usage);
        assert_eq!(dto.user_id, "alice");
        assert_eq!(dto.registry, "cargo");
        assert_eq!(dto.bytes_published, 1024);
        assert_eq!(dto.packages_count, 3);
    }
}
