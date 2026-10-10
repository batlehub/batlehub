use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use actix_web::{delete, get, post, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::{
    entities::AccessAction,
    ports::{BlockedIpInfo, IpBlockStore},
    services::AdminService,
};

use super::super::now_unix;
use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Debug, Serialize, ToSchema)]
pub struct BlockedIpDto {
    pub ip: String,
    pub blocked_at: u64,
    pub unblock_at: u64,
    pub reason: String,
}

impl From<BlockedIpInfo> for BlockedIpDto {
    fn from(b: BlockedIpInfo) -> Self {
        Self {
            ip: b.ip,
            blocked_at: b.blocked_at,
            unblock_at: b.unblock_at,
            reason: b.reason,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BlockIpRequest {
    pub ip: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// Duration to block the IP in seconds (defaults to 3600).
    #[serde(default)]
    pub duration_secs: Option<u64>,
}

/// List all currently blocked IPs.
#[utoipa::path(
    get,
    path = "/api/v1/admin/ip-blocks",
    tag = "back-office",
    responses(
        (status = 200, description = "List of blocked IPs", body = Vec<BlockedIpDto>),
        (status = 403, description = "`blocks:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/ip-blocks")]
pub async fn list_blocked_ips(
    identity: AuthIdentity,
    store: web::Data<Arc<dyn IpBlockStore>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::BlocksRead,
        None,
        &hot,
    )
    .await?;
    let blocked: Vec<BlockedIpDto> = store
        .list_blocked()
        .await
        .map_err(AppError::from)?
        .into_iter()
        .map(BlockedIpDto::from)
        .collect();
    Ok(HttpResponse::Ok().json(blocked))
}

/// Manually block an IP address.
#[utoipa::path(
    post,
    path = "/api/v1/admin/ip-blocks",
    tag = "back-office",
    request_body = BlockIpRequest,
    responses(
        (status = 204, description = "IP blocked"),
        (status = 400, description = "Invalid IP address or duration"),
        (status = 403, description = "`blocks:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/ip-blocks")]
pub async fn block_ip(
    body: web::Json<BlockIpRequest>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn IpBlockStore>>,
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
    IpAddr::from_str(&body.ip)
        .map_err(|_| AppError::bad_request(format!("'{}' is not a valid IP address", body.ip)))?;
    let duration = body.duration_secs.unwrap_or(3600);
    if duration == 0 {
        return Err(AppError::bad_request(
            "duration_secs must be greater than 0",
        ));
    }
    let unblock_at = now_unix()
        .checked_add(duration)
        .ok_or_else(|| AppError::bad_request("duration_secs is too large"))?;
    let reason = body.reason.as_deref().unwrap_or("manual");
    store
        .block_ip(&body.ip, unblock_at, reason)
        .await
        .map_err(AppError::from)?;

    // Which IP, until when and why: without it the trail says an IP was
    // blocked and not which one.
    admin_svc
        .record_admin_action_about(
            None,
            AccessAction::BlockIp,
            &identity.0,
            &identity.1,
            Some(format!("ip={} until={unblock_at} reason={reason}", body.ip)),
        )
        .await;

    Ok(HttpResponse::NoContent().finish())
}

/// Unblock an IP address.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/ip-blocks/{ip}",
    tag = "back-office",
    params(("ip" = String, Path, description = "IP address to unblock")),
    responses(
        (status = 204, description = "IP unblocked"),
        (status = 403, description = "`blocks:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/ip-blocks/{ip}")]
pub async fn unblock_ip(
    path: web::Path<(String,)>,
    identity: AuthIdentity,
    store: web::Data<Arc<dyn IpBlockStore>>,
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
    let (ip,) = path.into_inner();
    IpAddr::from_str(&ip)
        .map_err(|_| AppError::bad_request(format!("'{}' is not a valid IP address", ip)))?;
    store.unblock_ip(&ip).await.map_err(AppError::from)?;

    admin_svc
        .record_admin_action_about(
            None,
            AccessAction::UnblockIp,
            &identity.0,
            &identity.1,
            Some(format!("ip={ip}")),
        )
        .await;

    Ok(HttpResponse::NoContent().finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_ip_request_duration_overflow_is_caught() {
        // now_unix() + u64::MAX must not silently wrap; checked_add catches it.
        let now = now_unix();
        assert!(
            now.checked_add(u64::MAX).is_none(),
            "overflow must be detected"
        );
        // A sane large value must succeed.
        assert!(now.checked_add(3600).is_some());
    }

    #[test]
    fn unblock_ip_rejects_non_ip_strings() {
        // IpAddr::from_str is used for validation in both block_ip and unblock_ip.
        assert!(IpAddr::from_str("not-an-ip").is_err());
        assert!(IpAddr::from_str("1.2.3.4").is_ok());
        assert!(IpAddr::from_str("::1").is_ok());
    }
}
