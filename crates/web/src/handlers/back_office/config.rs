use std::sync::Arc;

use actix_web::{delete, get, post, put, web, HttpResponse, Responder};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::io;
use utoipa::{IntoParams, ToSchema};

use batlehub_config::schema::ConfigWarning;
use batlehub_core::{
    entities::{BannerLevel, GlobalBanner},
    error::CoreError,
};

use crate::{
    error::AppError,
    extractors::AuthIdentity,
    handlers::schemas::OkResponse,
    services::{
        BannerService, ConfigChangeRow, ConfigReloadService, PendingReloadSnapshot,
        ReloadApplyError, ReloadDiff,
    },
};

// ── Shared guards ─────────────────────────────────────────────────────────────

fn require_hot_reload(svc: &ConfigReloadService) -> Result<(), AppError> {
    if svc.hot_reload_enabled {
        Ok(())
    } else {
        Err(AppError::service_unavailable(
            "hot reload is disabled on this instance (BATLEHUB_DISABLE_HOT_RELOAD=1)",
        ))
    }
}

// ── Config reload ─────────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct ReloadResponse {
    pub diff: ReloadDiff,
    /// Non-fatal problems with the config this response describes — the one in
    /// force for `reload`/`apply`, the candidate one for `validate`/`from-content`.
    /// Empty for a clean config.
    pub warnings: Vec<ConfigWarning>,
    /// Whether this call left a pending reload staged for approval.
    ///
    /// Only `from-content` can set it: `reload` and `apply` consume the pending
    /// rather than leave one, and `validate` is a dry run. It exists because
    /// `from-content` returns `200` with an empty diff in two very different
    /// situations — a pending was staged, or the submitted content was identical
    /// to the last load attempt so there was nothing to stage. Without this flag
    /// the caller only discovers the difference when the subsequent apply fails
    /// with `404 No pending reload`.
    pub pending_created: bool,
}

/// Immediately reload the configuration (load, validate, and apply atomically).
#[utoipa::path(
    post,
    path = "/api/v1/admin/config/reload",
    tag = "back-office",
    responses(
        (status = 200, description = "Config reloaded", body = ReloadResponse),
        (status = 400, description = "Validation or probe failure"),
        (status = 403, description = "`config:write` required"),
        (status = 503, description = "Hot reload disabled"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/config/reload")]
pub async fn reload_config(
    identity: AuthIdentity,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigWrite,
        None,
        &hot,
    )
    .await?;
    require_hot_reload(&reload_svc)?;
    let user_id = identity.0.user_id.as_deref().unwrap_or("unknown");
    let diff = reload_svc
        .reload_immediate(user_id)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(web::Json(ReloadResponse {
        diff,
        warnings: reload_svc.warnings(),
        // This call consumed the pending reload; nothing is left staged.
        pending_created: false,
    }))
}

/// Get the current pending reload (loaded by the file watcher or a previous request).
#[utoipa::path(
    get,
    path = "/api/v1/admin/config/pending",
    tag = "back-office",
    responses(
        (status = 200, description = "Pending reload snapshot", body = PendingReloadSnapshot),
        (status = 403, description = "`config:read` required"),
        (status = 404, description = "No pending reload"),
        (status = 503, description = "Hot reload disabled"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/config/pending")]
pub async fn get_pending_reload(
    identity: AuthIdentity,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigRead,
        None,
        &hot,
    )
    .await?;
    require_hot_reload(&reload_svc)?;
    reload_svc
        .pending_snapshot()
        .map(web::Json)
        .ok_or_else(|| AppError::not_found("no pending reload"))
}

/// Apply the current pending reload.
#[utoipa::path(
    post,
    path = "/api/v1/admin/config/pending/apply",
    tag = "back-office",
    responses(
        (status = 200, description = "Pending reload applied", body = ReloadResponse),
        (status = 403, description = "`config:write` required"),
        (status = 404, description = "No pending reload"),
        (status = 409, description = "Pending reload expired"),
        (status = 503, description = "Hot reload disabled"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/config/pending/apply")]
pub async fn apply_pending_reload(
    identity: AuthIdentity,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigWrite,
        None,
        &hot,
    )
    .await?;
    require_hot_reload(&reload_svc)?;
    let user_id = identity.0.user_id.as_deref().unwrap_or("unknown");
    let diff = reload_svc.apply(user_id).await.map_err(|e| {
        match e.downcast_ref::<ReloadApplyError>() {
            Some(ReloadApplyError::NoPendingReload) => AppError::not_found(e.to_string()),
            Some(ReloadApplyError::Expired) => AppError::conflict(e.to_string()),
            _ => AppError::bad_request(e.to_string()),
        }
    })?;
    Ok(web::Json(ReloadResponse {
        diff,
        warnings: reload_svc.warnings(),
        // This call consumed the pending reload; nothing is left staged.
        pending_created: false,
    }))
}

/// Discard the current pending reload without applying.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/config/pending",
    tag = "back-office",
    responses(
        (status = 204, description = "Pending reload discarded"),
        (status = 403, description = "`config:write` required"),
        (status = 404, description = "No pending reload"),
        (status = 503, description = "Hot reload disabled"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/config/pending")]
pub async fn discard_pending_reload(
    identity: AuthIdentity,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigWrite,
        None,
        &hot,
    )
    .await?;
    require_hot_reload(&reload_svc)?;
    if reload_svc.discard_pending() {
        Ok(HttpResponse::NoContent().finish())
    } else {
        Err(AppError::not_found("no pending reload"))
    }
}

#[derive(Deserialize, ToSchema, IntoParams)]
pub struct ChangesQuery {
    #[serde(default)]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}
fn default_per_page() -> u64 {
    50
}

#[derive(Serialize, ToSchema)]
pub struct ConfigChangesResponse {
    pub items: Vec<ConfigChangeRow>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

/// List config change history.
///
/// **No restore endpoint, and deliberately so** (RFC 0004-bis A5, declined —
/// see the RFC's decision log for the recorded row).
///
/// `config_changes` stores a *diff summary* — added, removed and changed
/// registry names — and never the config itself. `POST …/history/{id}/restore`
/// cannot be built on that: a list of registry names does not reconstruct a
/// TOML file. Making it possible means storing the full config content per
/// change, and `config.toml` carries `upstream_auth` credentials, static bearer
/// tokens and OIDC client secrets. That would put every secret this instance
/// has ever been configured with into a table any admin can read through an
/// API, retained indefinitely and surviving the rotation that was supposed to
/// end their life.
///
/// The revert path that exists is the config editor: it shows the live content,
/// accepts a replacement, and validates before staging. What it lacks is the
/// *previous* content to paste, and closing that honestly is a question about
/// where config history should live — a git-backed config, or an encrypted
/// store — not a field on this response.
#[utoipa::path(
    get,
    path = "/api/v1/admin/config/changes",
    tag = "back-office",
    params(ChangesQuery),
    responses(
        (status = 200, description = "Config change history", body = ConfigChangesResponse),
        (status = 403, description = "`config:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/config/changes")]
pub async fn list_config_changes(
    identity: AuthIdentity,
    query: web::Query<ChangesQuery>,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigRead,
        None,
        &hot,
    )
    .await?;
    let (page, per_page) = crate::handlers::clamp_pagination(query.page, query.per_page);
    let (items, total) = tokio::try_join!(
        reload_svc.list_changes(page, per_page),
        reload_svc.count_changes(),
    )
    .map_err(CoreError::Other)?;
    Ok(web::Json(ConfigChangesResponse {
        items,
        total,
        page,
        per_page,
    }))
}

// ── Config content endpoints ──────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct ConfigContentResponse {
    pub content: String,
    /// True when hot reload is disabled (e.g. Kubernetes ConfigMap mount).
    /// Changes submitted via the editor API will be rejected when this is true.
    pub is_readonly: bool,
}

/// Retrieve the raw TOML content of the current config file.
#[utoipa::path(
    get,
    path = "/api/v1/admin/config/content",
    tag = "back-office",
    responses(
        (status = 200, description = "Raw config TOML content", body = ConfigContentResponse),
        (status = 403, description = "`config:read` required"),
        (status = 500, description = "Config file could not be read"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/config/content")]
pub async fn get_config_content(
    identity: AuthIdentity,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigRead,
        None,
        &hot,
    )
    .await?;
    let content = reload_svc
        .config_content()
        .await
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => AppError::not_found("config file not found"),
            _ => AppError::from(CoreError::Other(anyhow::anyhow!(
                "reading config file: {e}"
            ))),
        })?;
    Ok(web::Json(ConfigContentResponse {
        content,
        is_readonly: !reload_svc.hot_reload_enabled,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct ConfigFromContentRequest {
    pub content: String,
}

/// Validate a config TOML string without creating a pending reload.
///
/// Returns the diff that would result from applying the new config.
/// Use `POST /api/v1/admin/config/from-content` to also create the pending reload.
#[utoipa::path(
    post,
    path = "/api/v1/admin/config/validate",
    tag = "back-office",
    request_body = ConfigFromContentRequest,
    responses(
        (status = 200, description = "Config is valid; diff returned", body = ReloadResponse),
        (status = 400, description = "Validation failure"),
        (status = 403, description = "`config:read` required"),
        (status = 503, description = "Hot reload disabled"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/config/validate")]
pub async fn validate_config_content(
    identity: AuthIdentity,
    body: web::Json<ConfigFromContentRequest>,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigRead,
        None,
        &hot,
    )
    .await?;
    require_hot_reload(&reload_svc)?;
    let outcome = reload_svc
        .validate_content(&body.content)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(web::Json(ReloadResponse {
        diff: outcome.diff,
        warnings: outcome.warnings,
        pending_created: outcome.pending_created,
    }))
}

/// Validate a config TOML string and store it as a pending reload.
///
/// The content is parsed and validated exactly like the on-disk file;
/// environment variable placeholders (`${VAR}`) are still expanded.
/// On success a pending reload is created and can be confirmed via
/// `POST /api/v1/admin/config/pending/apply`.
#[utoipa::path(
    post,
    path = "/api/v1/admin/config/from-content",
    tag = "back-office",
    request_body = ConfigFromContentRequest,
    responses(
        (status = 200, description = "Pending reload created", body = ReloadResponse),
        (status = 400, description = "Validation failure"),
        (status = 403, description = "`config:write` required"),
        (status = 503, description = "Hot reload disabled"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/config/from-content")]
pub async fn load_config_from_content(
    identity: AuthIdentity,
    body: web::Json<ConfigFromContentRequest>,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigWrite,
        None,
        &hot,
    )
    .await?;
    require_hot_reload(&reload_svc)?;
    let outcome = reload_svc
        .load_pending_from_content(&body.content, crate::services::ReloadSource::AdminRequest)
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    Ok(web::Json(ReloadResponse {
        diff: outcome.diff,
        warnings: outcome.warnings,
        pending_created: outcome.pending_created,
    }))
}

// ── Config warnings ───────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct ConfigWarningsResponse {
    pub warnings: Vec<ConfigWarning>,
}

/// List the non-fatal problems with the configuration currently in force.
///
/// These are the states `validate` accepts but degrades on — a registry name
/// that cannot become a DNS label, a deprecated key being shadowed, a permissive
/// security default. Each carries a stable `code` and the `path` of the offending
/// config location, verbatim enough to search for in the TOML.
#[utoipa::path(
    get,
    path = "/api/v1/admin/config/warnings",
    tag = "back-office",
    responses(
        (status = 200, description = "Warnings for the active config", body = ConfigWarningsResponse),
        (status = 403, description = "`config:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/config/warnings")]
pub async fn get_config_warnings(
    identity: AuthIdentity,
    reload_svc: web::Data<Arc<ConfigReloadService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigRead,
        None,
        &hot,
    )
    .await?;
    Ok(web::Json(ConfigWarningsResponse {
        warnings: reload_svc.warnings(),
    }))
}

// ── Banner endpoints ──────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct SetBannerRequest {
    pub message: String,
    pub level: BannerLevel,
}

/// Set or replace the global admin banner.
#[utoipa::path(
    put,
    path = "/api/v1/admin/banner",
    tag = "back-office",
    request_body = SetBannerRequest,
    responses(
        (status = 200, description = "Banner set", body = OkResponse),
        (status = 403, description = "`config:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/api/v1/admin/banner")]
pub async fn set_banner(
    identity: AuthIdentity,
    body: web::Json<SetBannerRequest>,
    banner_svc: web::Data<Arc<BannerService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigWrite,
        None,
        &hot,
    )
    .await?;
    let set_by = identity
        .0
        .user_id
        .clone()
        .unwrap_or_else(|| "admin".to_owned());
    banner_svc
        .set(GlobalBanner {
            message: body.message.clone(),
            level: body.level.clone(),
            set_at: Utc::now(),
            set_by,
        })
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// Clear the global admin banner.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/banner",
    tag = "back-office",
    responses(
        (status = 204, description = "Banner cleared"),
        (status = 403, description = "`config:write` required"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/banner")]
pub async fn clear_banner(
    identity: AuthIdentity,
    banner_svc: web::Data<Arc<BannerService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ConfigWrite,
        None,
        &hot,
    )
    .await?;
    banner_svc.clear().await.map_err(AppError::from)?;
    Ok(HttpResponse::NoContent().finish())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_svc() -> Arc<ConfigReloadService> {
        use crate::services::{ConfigReloadParams, HotConfigBuilder};
        use batlehub_core::services::new_hot_lock;
        use std::collections::HashMap;

        let hot = new_hot_lock(batlehub_core::services::HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            ..Default::default()
        });
        let access = crate::new_access_lock(crate::AccessConfig {
            anonymous: Default::default(),
            user: Default::default(),
            admin: Default::default(),
            groups: Default::default(),
            explore_anonymous: Default::default(),
            explore_user: Default::default(),
            explore_admin: Default::default(),
        });
        let builder: HotConfigBuilder =
            Arc::new(|_| anyhow::bail!("builder not used in this test"));
        Arc::new(ConfigReloadService::new(ConfigReloadParams {
            hot,
            access,
            search: crate::new_search_lock(false),
            registry_map: crate::RegistryMap::new(HashMap::new()),
            registry_mode_map: crate::RegistryModeMap::new(HashMap::new()),
            upstream_map: crate::UpstreamMap::new(HashMap::new()),
            cargo_index_map: crate::CargoIndexMap::new(HashMap::new()),
            repo_signer_map: crate::RepoSignerMap::default(),
            vuln_db_map: crate::VulnDbMap::default(),
            sumdb_map: crate::SumDbMap::default(),
            registry_host_map: crate::RegistryHostMap::default(),
            proxy_trust: crate::middleware::ProxyTrust::default(),
            config_path: "config.toml".to_owned(),
            config_overlays: Vec::new(),
            config_change_repo: None,
            hot_reload_enabled: false,
            builder,
            banner: None,
        }))
    }

    #[test]
    fn require_hot_reload_returns_503_when_disabled() {
        let svc = disabled_svc();
        let err = require_hot_reload(&svc).unwrap_err();
        assert_eq!(err.status, actix_web::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn require_hot_reload_passes_when_enabled() {
        use crate::services::{ConfigReloadParams, HotConfigBuilder};
        use batlehub_core::services::new_hot_lock;
        use std::collections::HashMap;

        let hot = new_hot_lock(batlehub_core::services::HotConfig {
            registries: HashMap::new(),
            policies: HashMap::new(),
            ..Default::default()
        });
        let access = crate::new_access_lock(crate::AccessConfig {
            anonymous: Default::default(),
            user: Default::default(),
            admin: Default::default(),
            groups: Default::default(),
            explore_anonymous: Default::default(),
            explore_user: Default::default(),
            explore_admin: Default::default(),
        });
        let builder: HotConfigBuilder = Arc::new(|_| anyhow::bail!("unused"));
        let svc = Arc::new(ConfigReloadService::new(ConfigReloadParams {
            hot,
            access,
            search: crate::new_search_lock(false),
            registry_map: crate::RegistryMap::new(HashMap::new()),
            registry_mode_map: crate::RegistryModeMap::new(HashMap::new()),
            upstream_map: crate::UpstreamMap::new(HashMap::new()),
            cargo_index_map: crate::CargoIndexMap::new(HashMap::new()),
            repo_signer_map: crate::RepoSignerMap::default(),
            vuln_db_map: crate::VulnDbMap::default(),
            sumdb_map: crate::SumDbMap::default(),
            registry_host_map: crate::RegistryHostMap::default(),
            proxy_trust: crate::middleware::ProxyTrust::default(),
            config_path: "config.toml".to_owned(),
            config_overlays: Vec::new(),
            config_change_repo: None,
            hot_reload_enabled: true,
            builder,
            banner: None,
        }));
        assert!(require_hot_reload(&svc).is_ok());
    }
}
