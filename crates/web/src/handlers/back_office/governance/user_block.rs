use std::sync::Arc;

use actix_web::{delete, get, post, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::{
    entities::AccessAction,
    ports::{UserBlock, UserBlockRepository},
    services::AdminService,
};

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Debug, Serialize, ToSchema)]
pub struct UserBlockDto {
    pub user_id: String,
    pub blocked_at: DateTime<Utc>,
    pub blocked_by: String,
    pub reason: Option<String>,
}

impl From<UserBlock> for UserBlockDto {
    fn from(b: UserBlock) -> Self {
        Self {
            user_id: b.user_id,
            blocked_at: b.blocked_at,
            blocked_by: b.blocked_by,
            reason: b.reason,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BlockUserRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// List all blocked users.
#[utoipa::path(
    get,
    path = "/api/v1/admin/users/blocked",
    tag = "back-office",
    responses(
        (status = 200, description = "List of blocked users", body = Vec<UserBlockDto>),
        (status = 403, description = "`blocks:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/users/blocked")]
pub async fn list_blocked_users(
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn UserBlockRepository>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::BlocksRead,
        None,
        &hot,
    )
    .await?;
    let blocked: Vec<UserBlockDto> = repo
        .list()
        .await
        .map_err(AppError::from)?
        .into_iter()
        .map(UserBlockDto::from)
        .collect();
    Ok(HttpResponse::Ok().json(blocked))
}

/// Block a user by their user ID.
#[utoipa::path(
    post,
    path = "/api/v1/admin/users/{user_id}/block",
    tag = "back-office",
    params(("user_id" = String, Path, description = "User ID to block")),
    request_body = BlockUserRequest,
    responses(
        (status = 204, description = "User blocked"),
        (status = 403, description = "`blocks:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/users/{user_id}/block")]
pub async fn block_user(
    path: web::Path<(String,)>,
    body: web::Json<BlockUserRequest>,
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn UserBlockRepository>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::BlocksWrite,
        None,
        &hot,
    )
    .await?;
    let (raw_id,) = path.into_inner();
    let user_id = raw_id.trim().to_owned();
    if user_id.is_empty() {
        return Err(AppError::bad_request("user_id cannot be empty"));
    }
    let blocked_by = identity.0.user_id.as_deref().unwrap_or("admin");
    repo.block(&user_id, blocked_by, body.reason.as_deref())
        .await
        .map_err(AppError::from)?;

    admin_svc
        .record_account_action(AccessAction::BlockUser, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::NoContent().finish())
}

/// Unblock a user.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/users/{user_id}/block",
    tag = "back-office",
    params(("user_id" = String, Path, description = "User ID to unblock")),
    responses(
        (status = 204, description = "User unblocked"),
        (status = 403, description = "`blocks:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/users/{user_id}/block")]
pub async fn unblock_user(
    path: web::Path<(String,)>,
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn UserBlockRepository>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::BlocksWrite,
        None,
        &hot,
    )
    .await?;
    let (raw_id,) = path.into_inner();
    let user_id = raw_id.trim().to_owned();
    if user_id.is_empty() {
        return Err(AppError::bad_request("user_id cannot be empty"));
    }
    repo.unblock(&user_id).await.map_err(AppError::from)?;

    admin_svc
        .record_account_action(AccessAction::UnblockUser, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::NoContent().finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_request_reason_is_optional() {
        let req = BlockUserRequest { reason: None };
        assert!(req.reason.is_none());
        let req2 = BlockUserRequest {
            reason: Some("spam".into()),
        };
        assert_eq!(req2.reason.as_deref(), Some("spam"));
    }
}
