use std::sync::Arc;

use actix_web::{post, web, Responder};

use batlehub_core::entities::AccessAction;
use batlehub_core::ports::StorageAdminRepository;
use batlehub_core::services::{AdminService, ProxyService};

use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

use super::system::ClearCacheResponse;

/// Clear all cached artifacts for a specific registry (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/clear-cache",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
    ),
    responses(
        (status = 200, description = "Artifacts cleared", body = ClearCacheResponse),
        (status = 403, description = "`cache:evict` required"),
        (status = 404, description = "Registry not found"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/clear-cache")]
pub async fn clear_registry_cache(
    path: web::Path<String>,
    identity: AuthIdentity,
    registry_map: web::Data<RegistryMap>,
    storage_admin_repo: Option<web::Data<Arc<dyn StorageAdminRepository>>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::CacheEvict,
        Some(&registry),
        &hot,
    )
    .await?;

    if !registry_map.contains(&registry) {
        return Err(AppError::not_found("registry not found"));
    }

    let prefix = format!("artifact:{}/", registry);

    tracing::info!(registry = %registry, prefix = %prefix, "clear_registry_cache: starting");

    // Delete every cached artifact for the registry.
    //
    // This does *not* sweep the object store by prefix, despite the shape of the
    // call: `proxy_svc.storage` is always a `StorageRouter` (`initialize_storage`
    // wraps even a single backend), and the router answers `delete_by_prefix`
    // from the database, deleting each logical key it finds. Physical blobs are
    // content-addressed under `blob/<sha256>`, so a prefix sweep of the backend
    // could not find them by registry name anyway.
    //
    // The coverage that matters is therefore the database's, not the bucket's:
    // the router resolves keys from `artifact_dedup_refs` unioned with
    // `artifact_storage`, and every write records both. A row lost to a failed
    // `record_backend` is still reachable through the dedup table, which is
    // committed in the store transaction.
    let cleared = proxy_svc
        .storage
        .delete_by_prefix(&prefix)
        .await
        .map_err(AppError::from)?;

    tracing::info!(registry = %registry, cleared, "clear_registry_cache: done");

    // Clean up any remaining artifact_storage records.
    if let Some(repo) = storage_admin_repo {
        let _ = repo.delete_by_prefix(&prefix).await.inspect_err(
            |e| tracing::warn!(error = %e, prefix = %prefix, "failed to purge artifact_storage records"),
        );
    }

    // Registry-scoped, and recorded even when it cleared nothing: this is the
    // bluntest of the four `cache:evict` surfaces, and "who emptied the cache"
    // is a question that has to survive the answer being "there was nothing in
    // it". `delete_by_prefix` never knew the coordinates, so there is nothing
    // per-artifact to record even in principle — hence `CacheClear` rather than
    // a pile of `CacheEvict`s.
    admin_svc
        .record_cache_eviction(
            Some(batlehub_core::entities::PackageId::new(&registry, "", "")),
            AccessAction::CacheClear,
            &identity.0,
        )
        .await;

    Ok(web::Json(ClearCacheResponse { cleared }))
}
