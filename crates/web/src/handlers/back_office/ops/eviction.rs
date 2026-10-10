use std::collections::HashMap;
use std::sync::Arc;

use actix_web::{delete, post, web, Responder};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::entities::{AccessAction, PackageId};
use batlehub_core::services::{validate_path_safe, AdminService, EvictionService, ProxyService};

use crate::{error::AppError, extractors::AuthIdentity, RegistryMap};

/// Map of registry name → `EvictionService`, injected as app data.
pub type EvictionServiceMap = HashMap<String, Arc<EvictionService>>;

#[derive(Debug, Deserialize, IntoParams)]
pub struct EvictQuery {
    /// Report what the configured strategies would evict, without evicting it.
    ///
    /// Unlike retention's, this has no configured counterpart to override: an
    /// evicted artifact comes back on the next request, so the two-key interlock
    /// that protects a locally published version would be ceremony here.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Serialize, ToSchema)]
pub struct EvictResponse {
    pub total: usize,
    pub evicted_ttl: usize,
    pub evicted_idle: usize,
    pub evicted_old_versions: usize,
    pub evicted_lru: usize,
    /// True when nothing was dropped.
    pub dry_run: bool,
    /// The storage keys evicted, or that would be under `dry_run`.
    pub evicted_keys: Vec<String>,
    /// How many keys were dropped from `evicted_keys`. Non-zero means the list
    /// is a sample, not the answer.
    pub keys_truncated: u64,
    /// Set when the run stopped before answering the whole question — a
    /// `dry_run` size-cap preview that ran out of page while the registry was
    /// still over the cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomplete_because: Option<String>,
}

/// Run the configured eviction strategies for a registry's cache (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/evict",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        EvictQuery,
    ),
    responses(
        (status = 200, description = "Eviction completed, or previewed under `dry_run`",
            body = EvictResponse),
        (status = 403, description = "`cache:evict` required"),
        (status = 404, description = "Registry not found or eviction not configured"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/evict")]
pub async fn evict_registry(
    path: web::Path<String>,
    query: web::Query<EvictQuery>,
    identity: AuthIdentity,
    registry_map: web::Data<RegistryMap>,
    eviction_map: web::Data<EvictionServiceMap>,
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

    // Two ways to have no eviction: no service at all (a deployment that built
    // no map), or a service whose registry configured no strategy. Both are the
    // same `404` to the caller, and the second is the one that matters now that
    // the map holds every registry so `/coherence` can reach it.
    let svc = eviction_map
        .get(&registry)
        .filter(|svc| svc.config.evicts_anything())
        .ok_or_else(|| AppError::not_found("eviction not configured for this registry"))?;

    // The run event is written by the service, not here, so a caller that is
    // not this handler still leaves a trail.
    let report = svc
        .run_all(query.dry_run, &identity.0)
        .await
        .map_err(AppError::from)?;

    Ok(web::Json(EvictResponse {
        total: report.total,
        evicted_ttl: report.evicted_ttl,
        evicted_idle: report.evicted_idle,
        evicted_old_versions: report.evicted_old_versions,
        evicted_lru: report.evicted_lru,
        dry_run: report.dry_run,
        evicted_keys: report.evicted_keys,
        keys_truncated: report.keys_truncated,
        incomplete_because: report.incomplete_because,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct CoherenceResponse {
    /// Artifact blobs found in storage for this registry.
    pub storage_keys: usize,
    /// Rows the artifact-meta table holds for it.
    pub meta_rows: usize,
    /// Orphans deleted, or that would be under `dry_run`.
    pub orphaned_deleted: usize,
    /// The keys deleted, or that would be.
    pub deleted_keys: Vec<String>,
    /// Blobs seen orphaned for the *first* time: carried forward, deletable by
    /// the next run if they are still orphaned then.
    pub first_seen_orphaned: usize,
    /// Those keys — what a second run would take.
    pub first_seen_keys: Vec<String>,
    /// How many keys were dropped from the two lists. Non-zero means they are a
    /// sample, not the answer.
    pub keys_truncated: u64,
    /// True when nothing was deleted and nothing was advanced toward deletion.
    pub dry_run: bool,
}

/// Delete storage blobs this registry's artifact-meta table has no row for
/// (admin).
///
/// **Two passes before anything goes.** A blob is deleted only if it looked
/// orphaned on the previous sweep too, which is what makes this safe next to a
/// cache write in flight: `fetch_and_cache` stores the blob and records its meta
/// row in two steps, and a blob caught in that window has its row by the next
/// sweep. So the first run on a fresh estate deletes nothing and reports
/// `first_seen_orphaned` — run it again to collect them.
///
/// `dry_run` reports without deleting **and without advancing anything toward
/// deletion**: previewing twice is not the same as running twice.
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/coherence",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
        EvictQuery,
    ),
    responses(
        (status = 200, description = "Sweep completed, or previewed under `dry_run`",
            body = CoherenceResponse),
        (status = 403, description = "`cache:evict` required"),
        (status = 404, description = "Registry not found"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/coherence")]
pub async fn coherence_sweep(
    path: web::Path<String>,
    query: web::Query<EvictQuery>,
    identity: AuthIdentity,
    registry_map: web::Data<RegistryMap>,
    eviction_map: web::Data<EvictionServiceMap>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    // `cache:evict`, not a verb of its own: this deletes cached bytes for one
    // registry, which is exactly what that verb already governs across the other
    // three surfaces. A new verb would be one more thing for every existing
    // grant to be silently missing.
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

    // No `evicts_anything` filter here, unlike `/evict`: a registry with no
    // eviction policy still accumulates orphaned blobs from crashed writes, and
    // refusing to collect them because nobody configured a TTL would be reading
    // the wrong config to answer the question.
    let svc = eviction_map
        .get(&registry)
        .ok_or_else(|| AppError::not_found("registry not found"))?;

    let report = svc
        .run_coherence_check(query.dry_run, &identity.0)
        .await
        .map_err(AppError::from)?;

    Ok(web::Json(CoherenceResponse {
        storage_keys: report.storage_keys,
        meta_rows: report.meta_rows,
        orphaned_deleted: report.orphaned_deleted,
        deleted_keys: report.deleted_keys,
        first_seen_orphaned: report.first_seen_orphaned,
        first_seen_keys: report.first_seen_keys,
        keys_truncated: report.keys_truncated,
        dry_run: report.dry_run,
    }))
}

/// Request body for targeted proxy-cache artifact deletion.
#[derive(Deserialize, ToSchema)]
pub struct DeleteCacheRequest {
    /// Package name. Required for package-centric registries.
    pub name: Option<String>,
    /// Package version. Required for package-centric registries.
    pub version: Option<String>,
    /// Artifact path for path-addressed registries (deb/rpm/jetbrains),
    /// e.g. `"idea/idea-2026.1.3.tar.gz"`. Takes precedence over name+version.
    pub path: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct DeleteCacheResponse {
    /// `true` if the artifact was present and removed; `false` if it was not cached.
    pub deleted: bool,
    /// The logical storage key that was targeted.
    pub artifact_key: String,
}

/// Delete a single proxy-cached artifact for a registry (admin).
///
/// Removes the artifact from storage and clears its cache metadata so the next
/// request re-downloads it from upstream. Use `path` for path-addressed registries
/// (deb/rpm/jetbrains); use `name` + `version` for all others.
#[utoipa::path(
    delete,
    path = "/api/v1/admin/registries/{registry}/cache",
    tag = "back-office",
    params(
        ("registry" = String, Path, description = "Registry name"),
    ),
    request_body = DeleteCacheRequest,
    responses(
        (status = 200, description = "Cache entry deleted (or was not present)", body = DeleteCacheResponse),
        (status = 400, description = "Invalid or missing name/version/path"),
        (status = 403, description = "`cache:evict` required"),
        (status = 404, description = "Registry not found"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/registries/{registry}/cache")]
pub async fn delete_cached_artifact(
    path: web::Path<String>,
    identity: AuthIdentity,
    body: web::Json<DeleteCacheRequest>,
    registry_map: web::Data<RegistryMap>,
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

    validate_path_safe("registry", &registry).map_err(|e| AppError::bad_request(e.to_string()))?;

    // The coordinate as well as the key: the key is what storage is addressed
    // by, the coordinate is what the audit trail is read by, and for the
    // path-addressed registries they are not the same shape.
    let (artifact_key, coordinate) = if let Some(p) = &body.path {
        if p.is_empty() {
            return Err(AppError::bad_request("path must not be empty"));
        }
        validate_path_safe("path", p).map_err(|e| AppError::bad_request(e.to_string()))?;
        (
            format!("artifact:{registry}/repo/_/{p}"),
            PackageId::new(&registry, "repo", "_").with_artifact(p),
        )
    } else {
        let name = body
            .name
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::bad_request("name is required for package-centric registries")
            })?;
        let version = body
            .version
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::bad_request("version is required for package-centric registries")
            })?;
        validate_path_safe("name", name).map_err(|e| AppError::bad_request(e.to_string()))?;
        validate_path_safe("version", version).map_err(|e| AppError::bad_request(e.to_string()))?;
        (
            format!("artifact:{registry}/{name}/{version}"),
            PackageId::new(&registry, name, version),
        )
    };

    let deleted = proxy_svc
        .storage
        .delete(&artifact_key)
        .await
        .map_err(AppError::from)?;

    if deleted {
        if let Err(e) = proxy_svc
            .artifact_meta
            .delete_artifact_meta(&artifact_key)
            .await
        {
            tracing::warn!(key = %artifact_key, error = %e, "delete_cached_artifact: artifact_meta cleanup failed");
        }

        // Best-effort metadata cache invalidation so the next request re-resolves
        // versions from upstream instead of returning stale metadata.
        if let Some(meta_key) = artifact_key.strip_prefix("artifact:") {
            let cache_key = format!("meta:{meta_key}");
            if let Err(e) = proxy_svc.cache.invalidate(&cache_key).await {
                tracing::debug!(key = %cache_key, error = %e, "delete_cached_artifact: meta cache clear failed (non-fatal)");
            }
        }

        // Only on a real deletion. A `false` means the artifact was not cached,
        // and an event for it would put a drop in the trail that never
        // happened.
        admin_svc
            .record_cache_eviction(
                Some(coordinate),
                AccessAction::CacheEvict,
                &identity.0,
                &identity.1,
            )
            .await;
    }

    Ok(web::Json(DeleteCacheResponse {
        deleted,
        artifact_key,
    }))
}
