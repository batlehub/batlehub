use std::sync::Arc;

use actix_web::{delete, get, post, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::{
    entities::AccessAction,
    ports::{BetaChannelEntry, BetaChannelPort},
    services::AdminService,
};

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Debug, Serialize, ToSchema)]
pub struct BetaChannelMemberDto {
    pub principal_type: String,
    pub principal_id: String,
    pub granted_by: Option<String>,
}

impl From<BetaChannelEntry> for BetaChannelMemberDto {
    fn from(e: BetaChannelEntry) -> Self {
        Self {
            principal_type: e.principal_type,
            principal_id: e.principal_id,
            granted_by: e.granted_by,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AddBetaMemberRequest {
    pub principal_type: String,
    pub principal_id: String,
    pub granted_by: Option<String>,
}

/// List beta-channel members for a registry.
#[utoipa::path(
    get,
    path = "/api/v1/admin/registries/{registry}/beta-channel",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "Member list", body = Vec<BetaChannelMemberDto>),
        (status = 403, description = "`owners:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/registries/{registry}/beta-channel")]
pub async fn list_beta_members(
    path: web::Path<(String,)>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn BetaChannelPort>>,
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
    let members: Vec<BetaChannelMemberDto> = store
        .list_members(&registry)
        .await
        .map_err(AppError::from)?
        .into_iter()
        .map(BetaChannelMemberDto::from)
        .collect();
    Ok(HttpResponse::Ok().json(members))
}

/// Add a member to the beta channel for a registry.
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/beta-channel",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = AddBetaMemberRequest,
    responses(
        (status = 204, description = "Member added"),
        (status = 400, description = "Invalid principal_type"),
        (status = 403, description = "`owners:write` required"),
        (status = 409, description = "Already a member"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/beta-channel")]
pub async fn add_beta_member(
    path: web::Path<(String,)>,
    body: web::Json<AddBetaMemberRequest>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn BetaChannelPort>>,
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
    if body.principal_type != "user" && body.principal_type != "group" {
        return Err(AppError::bad_request(
            "principal_type must be 'user' or 'group'",
        ));
    }
    store
        .add_member(
            &registry,
            BetaChannelEntry {
                principal_type: body.principal_type.clone(),
                principal_id: body.principal_id.clone(),
                granted_by: body.granted_by.clone(),
            },
        )
        .await
        .map_err(AppError::from)?;

    admin_svc
        .record_account_action(AccessAction::AddBetaMember, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::NoContent().finish())
}

/// Remove a member from the beta channel for a registry.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/registries/{registry}/beta-channel/{principal_type}/{principal_id}",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("principal_type" = String, Path, description = "\"user\" or \"group\""),
        ("principal_id" = String, Path, description = "User ID or group name"),
    ),
    responses(
        (status = 204, description = "Member removed"),
        (status = 403, description = "`owners:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/registries/{registry}/beta-channel/{principal_type}/{principal_id}")]
pub async fn remove_beta_member(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn BetaChannelPort>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let (registry, principal_type, principal_id) = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::OwnersWrite,
        Some(&registry),
        &hot,
    )
    .await?;
    store
        .remove_member(&registry, &principal_type, &principal_id)
        .await
        .map_err(AppError::from)?;

    admin_svc
        .record_account_action(AccessAction::RemoveBetaMember, &identity.0, &identity.1)
        .await;

    Ok(HttpResponse::NoContent().finish())
}
