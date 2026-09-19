use super::{
    default_per_page, get, post, web, ActionResponse, AdminService, AppError, Arc, AuthIdentity,
    Deserialize, IntoParams, PackageFilter, PackageId, ProxyService, Responder, ToSchema,
};

use batlehub_core::entities::AccessAction;

use crate::{RegistryMap, RegistryModeMap};

// ── List all packages ─────────────────────────────────────────────────────────

#[derive(Deserialize, IntoParams)]
pub struct AdminPackageQuery {
    pub registry: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub blocked_only: bool,
    #[serde(default)]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
}

/// Paginated envelope for `GET /api/v1/admin/packages`, matching the shape of
/// its sibling list endpoints (`PackageListResponse`/`ExplorePackageListResponse`)
/// instead of returning a bare array with no way to tell if more pages exist.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct AdminPackageListResponse {
    pub items: Vec<batlehub_core::entities::PackageSummary>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

/// List all known packages (admin).
#[utoipa::path(
    get,
    path = "/api/v1/admin/packages",
    tag = "back-office",
    params(AdminPackageQuery),
    responses(
        (status = 200, description = "Full package listing with statuses, paginated", body = AdminPackageListResponse),
        (status = 403, description = "`packages:read` required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/packages")]
pub async fn list_packages(
    query: web::Query<AdminPackageQuery>,
    identity: AuthIdentity,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // `packages:read`, and scoped to the registry when the query names one.
    // A query that names none spans every registry, so it resolves at the
    // instance tier — the only node that can speak for all of them.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::PackagesRead,
        query.registry.as_deref(),
        &hot,
    )
    .await?;

    let (page, per_page) = crate::handlers::clamp_pagination(query.page, query.per_page);
    let filter = PackageFilter {
        registry: query.registry.clone(),
        registries: vec![],
        name_contains: query.name.clone(),
        name_exact: None,
        blocked_only: query.blocked_only,
        limit: per_page,
        offset: page * per_page,
    };
    let count_filter = PackageFilter {
        registry: query.registry.clone(),
        registries: vec![],
        name_contains: query.name.clone(),
        name_exact: None,
        blocked_only: query.blocked_only,
        limit: 0,
        offset: 0,
    };

    let (items, total) = tokio::try_join!(
        admin_svc.list_packages(filter),
        admin_svc.count_packages(count_filter),
    )
    .map_err(AppError::from)?;

    Ok(web::Json(AdminPackageListResponse {
        items,
        total,
        page: query.page,
        per_page: query.per_page,
    }))
}

// ── Block / unblock ───────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct BlockRequest {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
    pub reason: String,
}

/// Rebuild the generated `apk` indexes of `registry` after a block changed.
///
/// A no-op for every other kind — the three other OS kinds do not filter their
/// generated index, and the rest have no generated index at all — so the cost
/// on the ordinary path is one map lookup.
///
/// **Failure is loud and the block still holds.** The status row is already
/// committed and the download gate already refuses the version, so the estate
/// is safe; what goes stale is the *listing*. Rolling the block back to keep
/// the two in step would trade an enforced block with a stale listing for no
/// block at all, which is the worse of the two (RFC 0026 §6.4, decision 11).
async fn refresh_apk_index_after_block(
    registry: &str,
    map: &RegistryMap,
    mode_map: &RegistryModeMap,
    local_svc: &Arc<batlehub_core::services::LocalRegistryService>,
    admin_svc: &Arc<AdminService>,
    signers: &crate::ApkSignerMap,
) {
    if !map.is_type(registry, "apk") {
        return;
    }
    if !matches!(
        mode_map.get(registry),
        batlehub_config::schema::RegistryMode::Local
            | batlehub_config::schema::RegistryMode::Hybrid
    ) {
        return;
    }

    let result = crate::handlers::proxy::repo::publish::refresh_apk_indexes(
        local_svc.storage.as_ref(),
        admin_svc.repo.as_ref(),
        registry,
        signers.get(registry).as_deref(),
    )
    .await;

    if let Err(e) = result {
        metrics::counter!(
            "batlehub_apk_index_regeneration_failures_total",
            "registry" => registry.to_owned()
        )
        .increment(1);
        tracing::error!(
            registry,
            error = %e,
            "apk: the block is enforced at the download, but its index could not be \
             regenerated and still lists the blocked version — republish or reload to retry"
        );
    }
}

/// Block a package (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/packages/block",
    tag = "back-office",
    request_body = BlockRequest,
    responses(
        (status = 200, description = "Package blocked", body = ActionResponse),
        (status = 403, description = "`packages:block` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/packages/block")]
#[allow(clippy::too_many_arguments)]
pub async fn block_package(
    identity: AuthIdentity,
    body: web::Json<BlockRequest>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<batlehub_core::services::LocalRegistryService>>,
    apk_signers: web::Data<crate::ApkSignerMap>,
) -> Result<impl Responder, AppError> {
    // The registry is named in the body rather than the path, so the
    // check waits for it: a control verb resolves against the registry
    // it is about, which is what makes it delegable per registry.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::PackagesBlock,
        Some(&body.registry),
        &hot,
    )
    .await?;

    let pkg = PackageId {
        registry: body.registry.clone(),
        name: body.name.clone(),
        version: body.version.clone(),
        artifact: body.artifact.clone(),
    };

    admin_svc
        .block_package(&pkg, body.reason.clone(), &identity.0)
        .await
        .map_err(AppError::from)?;

    // An `apk` registry's generated index is filtered, so the block has to
    // reach the document as well as the gate.
    refresh_apk_index_after_block(
        &body.registry,
        &map,
        &mode_map,
        &local_svc,
        &admin_svc,
        &apk_signers,
    )
    .await;

    Ok(web::Json(ActionResponse {
        success: true,
        message: format!("package '{}' has been blocked", pkg),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct UnblockRequest {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
}

/// Unblock a package (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/packages/unblock",
    tag = "back-office",
    request_body = UnblockRequest,
    responses(
        (status = 200, description = "Package unblocked", body = ActionResponse),
        (status = 403, description = "`packages:block` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/packages/unblock")]
#[allow(clippy::too_many_arguments)]
pub async fn unblock_package(
    identity: AuthIdentity,
    body: web::Json<UnblockRequest>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
    map: web::Data<RegistryMap>,
    mode_map: web::Data<RegistryModeMap>,
    local_svc: web::Data<Arc<batlehub_core::services::LocalRegistryService>>,
    apk_signers: web::Data<crate::ApkSignerMap>,
) -> Result<impl Responder, AppError> {
    // The registry is named in the body rather than the path, so the
    // check waits for it: a control verb resolves against the registry
    // it is about, which is what makes it delegable per registry.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::PackagesBlock,
        Some(&body.registry),
        &hot,
    )
    .await?;

    let pkg = PackageId {
        registry: body.registry.clone(),
        name: body.name.clone(),
        version: body.version.clone(),
        artifact: body.artifact.clone(),
    };

    admin_svc
        .unblock_package(&pkg, &identity.0)
        .await
        .map_err(AppError::from)?;

    // The entry reappears in the generated index, re-signed with it.
    refresh_apk_index_after_block(
        &body.registry,
        &map,
        &mode_map,
        &local_svc,
        &admin_svc,
        &apk_signers,
    )
    .await;

    Ok(web::Json(ActionResponse {
        success: true,
        message: format!("package '{}' has been unblocked", pkg),
    }))
}

// ── Delete package record + cached artifact ───────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct DeletePackageRequest {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
}

/// Delete a package record and purge its cached artifact (admin).
///
/// Removes the entry from the administrative tracking table and deletes the
/// cached artifact from storage so the next request re-downloads from upstream.
#[utoipa::path(
    post,
    path = "/api/v1/admin/packages/delete",
    tag = "back-office",
    request_body = DeletePackageRequest,
    responses(
        (status = 200, description = "Package deleted", body = ActionResponse),
        (status = 403, description = "`releases:delete` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/packages/delete")]
pub async fn delete_package(
    identity: AuthIdentity,
    body: web::Json<DeletePackageRequest>,
    admin_svc: web::Data<Arc<AdminService>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // The registry is named in the body rather than the path, so the
    // check waits for it: a control verb resolves against the registry
    // it is about, which is what makes it delegable per registry.
    // **Instance tier, not the registry.** §10 rule 5 grants `releases:yank`
    // and `releases:delete` to `role:user` on every local and hybrid registry,
    // because that is what `has_role_at_least(&Role::User)` meant on the
    // per-package lifecycle path. This is not that path: it is the
    // administrative bulk surface, which mutates many packages at once and
    // bypasses the ownership check the per-package route applies. Resolving it
    // against the registry tier would hand every `role:user` an endpoint
    // `require_admin` reserved — which a pre-existing test caught, and which is
    // exactly the widening §7 calls the migration's central risk.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ReleasesDelete,
        None,
        &hot,
    )
    .await?;

    let pkg = PackageId {
        registry: body.registry.clone(),
        name: body.name.clone(),
        version: body.version.clone(),
        artifact: body.artifact.clone(),
    };

    let deleted = admin_svc
        .delete_package(&pkg, &identity.0)
        .await
        .map_err(AppError::from)?;

    if !deleted {
        return Ok(web::Json(ActionResponse {
            success: false,
            message: format!("package '{}' not found", pkg),
        }));
    }

    let storage_key = format!("artifact:{}", pkg.cache_key());
    let meta_key = format!("meta:{}", pkg.cache_key());
    // Best-effort: purge cached artifact and metadata cache.
    let _ = proxy_svc.storage.delete(&storage_key).await.inspect_err(
        |e| tracing::warn!(error = %e, key = %storage_key, "failed to purge cached artifact"),
    );
    let _ = proxy_svc
        .artifact_meta
        .delete_artifact_meta(&storage_key)
        .await
        .inspect_err(
            |e| tracing::warn!(error = %e, key = %storage_key, "failed to purge artifact metadata"),
        );
    let _ = proxy_svc.cache.invalidate(&meta_key).await.inspect_err(
        |e| tracing::warn!(error = %e, key = %meta_key, "failed to invalidate metadata cache"),
    );

    Ok(web::Json(ActionResponse {
        success: true,
        message: format!("package '{}' deleted", pkg),
    }))
}

// ── Cache invalidation ────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct InvalidateRequest {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
}

/// Purge the cached artifact for a specific package version (admin).
///
/// Deletes the artifact from storage and clears the in-memory metadata cache.
/// The package block/unblock status is not changed.
#[utoipa::path(
    post,
    path = "/api/v1/admin/packages/invalidate",
    tag = "back-office",
    request_body = InvalidateRequest,
    responses(
        (status = 200, description = "Cache purged", body = ActionResponse),
        (status = 403, description = "`cache:evict` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/packages/invalidate")]
pub async fn invalidate_package(
    identity: AuthIdentity,
    body: web::Json<InvalidateRequest>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // The registry is named in the body rather than the path, so the
    // check waits for it: a control verb resolves against the registry
    // it is about, which is what makes it delegable per registry.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::CacheEvict,
        Some(&body.registry),
        &hot,
    )
    .await?;

    let pkg = PackageId {
        registry: body.registry.clone(),
        name: body.name.clone(),
        version: body.version.clone(),
        artifact: body.artifact.clone(),
    };

    let storage_key = format!("artifact:{}", pkg.cache_key());
    let meta_key = format!("meta:{}", pkg.cache_key());

    proxy_svc
        .storage
        .delete(&storage_key)
        .await
        .map_err(AppError::from)?;
    proxy_svc
        .cache
        .invalidate(&meta_key)
        .await
        .map_err(AppError::from)?;

    // The same event `DELETE /registries/{r}/cache` writes: this endpoint is
    // the older spelling of the same operation, and two surfaces for one action
    // must not produce two different trails.
    admin_svc
        .record_cache_eviction(Some(pkg.clone()), AccessAction::CacheEvict, &identity.0)
        .await;

    Ok(web::Json(ActionResponse {
        success: true,
        message: format!("cache purged for '{}'", pkg),
    }))
}
