use std::sync::Arc;

use actix_web::{get, web, HttpResponse, Responder};
use chrono::DateTime;
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_config::schema::RateLimitEnforcement;

use super::quota::QuotaStateDto;
use super::require_user_id;
use crate::{
    error::AppError,
    extractors::AuthIdentity,
    middleware::{RateLimitScope, RateLimitService, RateLimitUsage},
};

/// The share of a window's ceiling at which a counter reads `warning`.
///
/// `[registries.rate_limit]` has no threshold of its own (a quota has
/// `warn_threshold_pct`); this is the console's, fixed rather than invented per
/// caller, so two people looking at the same counter see the same state.
const WARN_PCT: u64 = 80;

/// Whose counter a row is.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitScopeDto {
    /// The caller's own counter.
    User,
    /// A group's counter, shared by every member — not the caller's alone.
    Group,
}

/// What happens past the ceiling.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitEnforcementDto {
    /// The request is refused with `429` and `Retry-After`.
    Block,
    /// The request is served, with an `X-RateLimit-Warning` header.
    Warn,
}

/// One rate-limit counter that applies to the caller, as it stands in the
/// current window.
#[derive(Debug, Serialize, ToSchema)]
pub struct MyRateLimitDto {
    pub registry: String,
    pub scope: RateLimitScopeDto,
    /// The group's name when `scope` is `group`.
    pub group: Option<String>,
    /// Requests counted in the current window.
    pub used: u64,
    /// The window's ceiling.
    pub limit: u32,
    pub window_secs: u32,
    /// When the current window closes and the counter starts again from zero (RFC 3339).
    pub resets_at: String,
    pub enforcement: RateLimitEnforcementDto,
    /// `at_limit` once `used` reaches `limit`, `warning` from 80 % of it.
    pub state: QuotaStateDto,
}

impl MyRateLimitDto {
    fn from_usage(registry: &str, u: RateLimitUsage) -> Self {
        let (scope, group) = match u.scope {
            RateLimitScope::User => (RateLimitScopeDto::User, None),
            RateLimitScope::Group(g) => (RateLimitScopeDto::Group, Some(g)),
        };
        let limit = u64::from(u.limit);
        let state = if u.used >= limit {
            QuotaStateDto::AtLimit
        } else if u.used.saturating_mul(100) >= limit.saturating_mul(WARN_PCT) {
            QuotaStateDto::Warning
        } else {
            QuotaStateDto::Ok
        };
        Self {
            registry: registry.to_owned(),
            scope,
            group,
            used: u.used,
            limit: u.limit,
            window_secs: u.window_secs,
            resets_at: i64::try_from(u.reset_unix)
                .ok()
                .and_then(|s| DateTime::from_timestamp(s, 0))
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
            enforcement: match u.enforcement {
                RateLimitEnforcement::Block => RateLimitEnforcementDto::Block,
                RateLimitEnforcement::Warn => RateLimitEnforcementDto::Warn,
            },
            state,
        }
    }
}

/// The caller's own rate-limit counters, per registry.
///
/// Read without spending a request: the proxy only ever said the ceiling
/// (`X-RateLimit-Limit`), so a developer learned how close they were by being
/// refused. One row for their own counter in each accessible registry that has
/// a `[registries.rate_limit]`, and one per group of theirs that the registry
/// gives its own ceiling. A registry with no limit, or one the caller cannot
/// reach, does not appear — the same rule as `/api/v1/me/quota`.
///
/// Takes no `user_id`: the subject is the token's.
#[utoipa::path(
    get,
    path = "/api/v1/me/limits",
    tag = "user",
    responses(
        (status = 200, description = "The caller's rate-limit counters in the current window, per registry", body = Vec<MyRateLimitDto>),
        (status = 401, description = "Authentication required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/me/limits")]
pub async fn my_limits(
    identity: AuthIdentity,
    rate_limit: Option<web::Data<Arc<RateLimitService>>>,
    access: web::Data<crate::AccessConfigLock>,
) -> Result<impl Responder, AppError> {
    let user_id = require_user_id(&identity)?;
    let Some(rate_limit) = rate_limit else {
        return Ok(HttpResponse::Ok().json(Vec::<MyRateLimitDto>::new()));
    };

    let mut accessible: Vec<String> = access
        .read()
        .await
        .accessible_registries_for(&identity)
        .into_iter()
        .collect();
    accessible.sort();

    let mut rows = Vec::new();
    for registry in &accessible {
        let Some(usage) = rate_limit
            .usage(registry, user_id, &identity.0.groups)
            .await
        else {
            continue;
        };
        rows.extend(
            usage
                .map_err(AppError::from)?
                .into_iter()
                .map(|u| MyRateLimitDto::from_usage(registry, u)),
        );
    }
    Ok(HttpResponse::Ok().json(rows))
}
