use super::{
    map_bulk_failures, post, web, AdminService, AppError, Arc, AuthIdentity, BulkActionResponse,
    BulkBlockItem, Deserialize, PackageId, ProxyService, Responder, ToSchema,
};

/// Every distinct registry a bulk request names.
///
/// Sorted and deduplicated so the authorization check runs once per registry
/// rather than once per item — a 500-item request naming two registries asks two
/// questions, not five hundred.
macro_rules! registries_named {
    ($body:expr) => {{
        let mut regs: Vec<String> = $body.items.iter().map(|i| i.registry.clone()).collect();
        regs.sort();
        regs.dedup();
        regs
    }};
}

// ── Bulk block / unblock ──────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct BulkBlockRequestItem {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
    pub reason: String,
}

#[derive(Deserialize, ToSchema)]
pub struct BulkBlockRequest {
    pub items: Vec<BulkBlockRequestItem>,
}

#[derive(Deserialize, ToSchema)]
pub struct BulkUnblockRequestItem {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct BulkUnblockRequest {
    pub items: Vec<BulkUnblockRequestItem>,
}

/// Bulk-block packages (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/packages/bulk-block",
    tag = "back-office",
    request_body = BulkBlockRequest,
    responses(
        (status = 200, description = "Bulk block result", body = BulkActionResponse),
        (status = 403, description = "`packages:block` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/packages/bulk-block")]
pub async fn bulk_block_packages(
    identity: AuthIdentity,
    body: web::Json<BulkBlockRequest>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // A bulk request may name several registries, so the verb is checked on
    // **every one of them** rather than on the instance tier alone. Checking
    // once at the top would let a delegate holding `{verb}` on one registry
    // mutate packages in another by putting both in the same request — the
    // bulk endpoint's version of a predicate that is vacuous rather than
    // absent. All-or-nothing, before anything is written: a partial bulk
    // mutation is worse than a refused one.
    // An **empty** request must still be authorized. The first version of
    // this looped over the registries the body named and checked nothing
    // when it named none — so `{"items": []}` reached the handler with no
    // authorization at all, which the pre-existing `non_admin_returns_403`
    // row caught. A check that a caller can skip by sending less is not a
    // check; the empty case falls back to the instance tier, which is the
    // node that speaks for "any registry".
    let named = registries_named!(body);
    if named.is_empty() {
        crate::handlers::back_office::require_verb(
            &identity,
            batlehub_core::entities::Action::PackagesBlock,
            None,
            &hot,
        )
        .await?;
    }
    for registry in named {
        crate::handlers::back_office::require_verb(
            &identity,
            batlehub_core::entities::Action::PackagesBlock,
            Some(&registry),
            &hot,
        )
        .await?;
    }

    let items = body
        .into_inner()
        .items
        .into_iter()
        .map(|i| BulkBlockItem {
            package_id: PackageId {
                registry: i.registry,
                name: i.name,
                version: i.version,
                artifact: i.artifact,
            },
            reason: i.reason,
        })
        .collect();

    let result = admin_svc
        .bulk_block_packages(items, &identity.0, &identity.1)
        .await;

    Ok(web::Json(BulkActionResponse {
        succeeded_count: result.succeeded.len(),
        failed_count: result.failed.len(),
        failures: map_bulk_failures(result.failed),
    }))
}

/// Bulk-unblock packages (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/packages/bulk-unblock",
    tag = "back-office",
    request_body = BulkUnblockRequest,
    responses(
        (status = 200, description = "Bulk unblock result", body = BulkActionResponse),
        (status = 403, description = "`packages:block` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/packages/bulk-unblock")]
pub async fn bulk_unblock_packages(
    identity: AuthIdentity,
    body: web::Json<BulkUnblockRequest>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // A bulk request may name several registries, so the verb is checked on
    // **every one of them** rather than on the instance tier alone. Checking
    // once at the top would let a delegate holding `{verb}` on one registry
    // mutate packages in another by putting both in the same request — the
    // bulk endpoint's version of a predicate that is vacuous rather than
    // absent. All-or-nothing, before anything is written: a partial bulk
    // mutation is worse than a refused one.
    // An **empty** request must still be authorized. The first version of
    // this looped over the registries the body named and checked nothing
    // when it named none — so `{"items": []}` reached the handler with no
    // authorization at all, which the pre-existing `non_admin_returns_403`
    // row caught. A check that a caller can skip by sending less is not a
    // check; the empty case falls back to the instance tier, which is the
    // node that speaks for "any registry".
    let named = registries_named!(body);
    if named.is_empty() {
        crate::handlers::back_office::require_verb(
            &identity,
            batlehub_core::entities::Action::PackagesBlock,
            None,
            &hot,
        )
        .await?;
    }
    for registry in named {
        crate::handlers::back_office::require_verb(
            &identity,
            batlehub_core::entities::Action::PackagesBlock,
            Some(&registry),
            &hot,
        )
        .await?;
    }

    let items = body
        .into_inner()
        .items
        .into_iter()
        .map(|i| PackageId {
            registry: i.registry,
            name: i.name,
            version: i.version,
            artifact: i.artifact,
        })
        .collect();

    let result = admin_svc
        .bulk_unblock_packages(items, &identity.0, &identity.1)
        .await;

    Ok(web::Json(BulkActionResponse {
        succeeded_count: result.succeeded.len(),
        failed_count: result.failed.len(),
        failures: map_bulk_failures(result.failed),
    }))
}

// ── Bulk delete ───────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct BulkDeleteRequestItem {
    pub registry: String,
    pub name: String,
    pub version: String,
    pub artifact: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct BulkDeleteRequest {
    pub items: Vec<BulkDeleteRequestItem>,
}

/// Bulk-delete package records and purge their cached artifacts (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/packages/bulk-delete",
    tag = "back-office",
    request_body = BulkDeleteRequest,
    responses(
        (status = 200, description = "Bulk delete result", body = BulkActionResponse),
        (status = 403, description = "`releases:delete` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/packages/bulk-delete")]
pub async fn bulk_delete_packages(
    identity: AuthIdentity,
    body: web::Json<BulkDeleteRequest>,
    admin_svc: web::Data<Arc<AdminService>>,
    proxy_svc: web::Data<Arc<ProxyService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    // A bulk request may name several registries, so the verb is checked on
    // **every one of them** rather than on the instance tier alone. Checking
    // once at the top would let a delegate holding `{verb}` on one registry
    // mutate packages in another by putting both in the same request — the
    // bulk endpoint's version of a predicate that is vacuous rather than
    // absent. All-or-nothing, before anything is written: a partial bulk
    // mutation is worse than a refused one.
    // An **empty** request must still be authorized. The first version of
    // this looped over the registries the body named and checked nothing
    // when it named none — so `{"items": []}` reached the handler with no
    // authorization at all, which the pre-existing `non_admin_returns_403`
    // row caught. A check that a caller can skip by sending less is not a
    // check; the empty case falls back to the instance tier, which is the
    // node that speaks for "any registry".
    // **Instance tier, not the registry.** §10 rule 5 grants `releases:delete` to
    // `role:user` on every local and hybrid registry, because that is what
    // `has_role_at_least(&Role::User)` meant on the per-package lifecycle path.
    // This is not that path: it is the administrative bulk surface, which mutates
    // many packages at once and bypasses the ownership check the per-package
    // route applies. Resolving it against the registry tier would hand every
    // `role:user` an endpoint `require_admin` reserved — the widening §7 calls
    // the migration's central risk, and a pre-existing test caught it.
    //
    // One check rather than one per named registry: the instance node is the same
    // node whatever the body names, so asking it repeatedly asks nothing new.
    crate::handlers::back_office::require_verb(
        &identity,
        batlehub_core::entities::Action::ReleasesDelete,
        None,
        &hot,
    )
    .await?;

    let items: Vec<PackageId> = body
        .into_inner()
        .items
        .into_iter()
        .map(|i| PackageId {
            registry: i.registry,
            name: i.name,
            version: i.version,
            artifact: i.artifact,
        })
        .collect();

    let result = admin_svc
        .bulk_delete_packages(items, &identity.0, &identity.1)
        .await;

    // Best-effort: purge cached artifacts only for packages successfully removed from the DB.
    for pkg in &result.succeeded {
        let storage_key = format!("artifact:{}", pkg.cache_key());
        let meta_key = format!("meta:{}", pkg.cache_key());
        let _ = proxy_svc.storage.delete(&storage_key).await.inspect_err(
            |e| tracing::warn!(error = %e, key = %storage_key, "failed to purge cached artifact"),
        );
        let _ = proxy_svc
            .artifact_meta
            .delete_artifact_meta(&storage_key)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, key = %storage_key, "failed to purge artifact metadata"));
        let _ = proxy_svc.cache.invalidate(&meta_key).await.inspect_err(
            |e| tracing::warn!(error = %e, key = %meta_key, "failed to invalidate metadata cache"),
        );
    }

    Ok(web::Json(BulkActionResponse {
        succeeded_count: result.succeeded.len(),
        failed_count: result.failed.len(),
        failures: map_bulk_failures(result.failed),
    }))
}
