//! The flags an administrator can see (RFC 0002 §4.5): which source said
//! what about which version, and whether it still stands. `flags:read`.

use std::sync::Arc;

use actix_web::{get, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::{
    entities::{Action, FlagEffect, FlagFilter, PackageFlag},
    ports::AdvisoryRepository,
};

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Deserialize, IntoParams)]
pub struct FlagsQuery {
    pub registry: Option<String>,
    pub package_name: Option<String>,
    pub source: Option<String>,
    /// `inform | warn | gate | hard_block`.
    pub effect: Option<String>,
    /// Include revoked and expired flags.
    #[serde(default)]
    pub include_dead: bool,
    #[serde(default)]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}

fn default_per_page() -> u64 {
    50
}

#[derive(Serialize, ToSchema)]
pub struct FlagsResponse {
    pub items: Vec<PackageFlag>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

/// List pushed flags.
#[utoipa::path(
    get,
    path = "/api/v1/admin/flags",
    tag = "back-office",
    params(FlagsQuery),
    responses(
        (status = 200, description = "Flags, newest update first", body = FlagsResponse),
        (status = 403, description = "`flags:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/flags")]
pub async fn list_flags(
    query: web::Query<FlagsQuery>,
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn AdvisoryRepository>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // Instance-wide, as `audit:read` is: the listing spans every registry
    // and the filter narrows what is shown, not who may look.
    crate::handlers::back_office::require_verb(&identity, Action::FlagsRead, None, &hot).await?;
    let effect = match query.effect.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(FlagEffect::parse(s).ok_or_else(|| {
            AppError::bad_request(format!(
                "unknown effect '{s}' (expected inform, warn, gate or hard_block)"
            ))
        })?),
        None => None,
    };
    let (page, per_page) = crate::handlers::clamp_pagination(query.page, query.per_page);
    let filter = FlagFilter {
        registry: query.registry.clone(),
        package_name: query.package_name.clone(),
        source: query.source.clone(),
        effect,
        include_dead: query.include_dead,
        limit: per_page,
        offset: page * per_page,
    };
    let count_filter = FlagFilter {
        limit: 0,
        offset: 0,
        ..filter.clone()
    };
    let (items, total) =
        tokio::try_join!(repo.list_flags(&filter), repo.count_flags(&count_filter))
            .map_err(AppError::from)?;
    Ok(web::Json(FlagsResponse {
        items,
        total,
        page: query.page,
        per_page: query.per_page,
    }))
}
