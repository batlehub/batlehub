use std::sync::Arc;

use actix_web::{post, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::{
    entities::{AccessAction, Identity, LifecycleVerb, RegistryKind},
    ports::BulkResult,
    services::LocalRegistryService,
};

use crate::{error::AppError, extractors::AuthIdentity, handlers::schemas::OkResponse};

/// Flatten a `BulkPackageRequest` into the `(name, version)` pairs expected by
/// `LocalRegistryBackend::bulk_*`.
fn bulk_items(body: web::Json<BulkPackageRequest>) -> Vec<(String, String)> {
    body.into_inner()
        .packages
        .into_iter()
        .map(|p| (p.name, p.version))
        .collect()
}

/// Record one lifecycle audit event per item that `result` reports as
/// succeeded. `LocalRegistryBackend::bulk_*` only returns a success *count*,
/// not which coordinates succeeded, so the succeeded set is `items` minus the
/// ones `result.failed` names — mirrors the single-item yank/unyank/deprecate/
/// unlist handlers, which already audit through `LocalRegistryService::record_lifecycle_action`.
async fn record_bulk_lifecycle_audit(
    local_svc: &LocalRegistryService,
    registry: &str,
    items: &[(String, String)],
    result: &BulkResult,
    action: AccessAction,
    identity: &Identity,
) {
    let failed: std::collections::HashSet<(&str, &str)> = result
        .failed
        .iter()
        .map(|(name, version, _)| (name.as_str(), version.as_str()))
        .collect();
    for (name, version) in items {
        if failed.contains(&(name.as_str(), version.as_str())) {
            continue;
        }
        local_svc
            .record_lifecycle_action(registry, name, version, action, identity)
            .await;
    }
}

/// Who may run a yank-family verb on `bulk-*` (RFC 0040 §6.4).
enum Arm {
    /// The instance-tier grant, as before: straight to the backend.
    Admin,
    /// An explicit owner of every package: each item goes through
    /// `LocalRegistryService`, which re-checks the registry-tier write,
    /// namespace membership and ownership, and audits it.
    Owner,
}

/// The instance-tier `action`, or — when the registry's kind grants `verb` to
/// owners — explicit ownership of **every** package in `items`. Names are
/// canonicalised in place, so the check and the action read the same string.
async fn authorize_lifecycle(
    identity: &AuthIdentity,
    action: batlehub_core::entities::Action,
    verb: LifecycleVerb,
    registry: &str,
    items: &mut [(String, String)],
    local_svc: &LocalRegistryService,
    hot: &batlehub_core::services::hot_config::HotConfigLock,
) -> Result<Arm, AppError> {
    let denied = match crate::handlers::back_office::require_verb(identity, action, None, hot).await
    {
        Ok(()) => return Ok(Arm::Admin),
        Err(e) => e,
    };
    let kind = hot
        .read()
        .await
        .registries
        .get(registry)
        .and_then(|c| c.registry_type().parse::<RegistryKind>().ok());
    let (Some(kind), Some(ownership)) = (kind, local_svc.ownership.as_ref()) else {
        // No ownership store means no owner can be proven: never vacuously true.
        return Err(denied);
    };
    if !kind.owner_verbs().contains(&verb) || items.is_empty() {
        return Err(denied);
    }
    for (name, _) in items.iter_mut() {
        *name = kind.canonical_package_name(name).into_owned();
    }
    let mut names: Vec<String> = items.iter().map(|(n, _)| n.clone()).collect();
    names.sort_unstable();
    names.dedup();
    let unowned = ownership
        .not_owned_by(registry, &names, &identity.0)
        .await
        .map_err(AppError::from)?;
    match unowned.first() {
        // A mixed request is refused whole: a bulk request must not reach a
        // package its sender does not own.
        Some(first) => Err(AppError::forbidden(format!(
            "this endpoint requires the '{action}' permission, or ownership of every package; \
             you do not own '{first}' in '{registry}'"
        ))),
        None => Ok(Arm::Owner),
    }
}

/// The owner arm: one `LocalRegistryService` call per item.
async fn run_as_owner(
    local_svc: &LocalRegistryService,
    registry: &str,
    items: &[(String, String)],
    verb: LifecycleVerb,
    identity: &Identity,
) -> BulkResult {
    let mut result = BulkResult {
        processed: items.len(),
        succeeded: 0,
        failed: vec![],
    };
    for (name, version) in items {
        let outcome = match verb {
            LifecycleVerb::Yank => local_svc.yank(registry, name, version, identity).await,
            LifecycleVerb::Unyank => local_svc.unyank(registry, name, version, identity).await,
        };
        match outcome {
            Ok(()) => result.succeeded += 1,
            Err(e) => result
                .failed
                .push((name.clone(), version.clone(), e.to_string())),
        }
    }
    result
}

/// Convert a `BulkResult` into the API response DTO.
fn bulk_response(result: BulkResult) -> BulkPackageResponse {
    BulkPackageResponse {
        processed: result.processed,
        succeeded: result.succeeded,
        failed: result
            .failed
            .into_iter()
            .map(|(name, version, error)| BulkPackageFailureDto {
                name,
                version,
                error,
            })
            .collect(),
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkPackageItem {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkPackageRequest {
    pub packages: Vec<BulkPackageItem>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BulkPackageFailureDto {
    pub name: String,
    pub version: String,
    pub error: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BulkPackageResponse {
    pub processed: usize,
    pub succeeded: usize,
    pub failed: Vec<BulkPackageFailureDto>,
}

/// Bulk-yank versions in a local/hybrid registry (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/bulk-yank",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = BulkPackageRequest,
    responses(
        (status = 200, description = "Bulk yank result", body = BulkPackageResponse),
        (status = 403, description = "`releases:yank` required, or explicit ownership of \
                                      every package on a kind whose owners may yank"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/bulk-yank")]
pub async fn bulk_yank(
    path: web::Path<String>,
    body: web::Json<BulkPackageRequest>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    // **Instance tier, not the registry.** §10 rule 5 grants `releases:yank`
    // and `releases:delete` to `role:user` on every local and hybrid registry,
    // because that is what `has_role_at_least(&Role::User)` meant on the
    // per-package lifecycle path. This is not that path: it is the
    // administrative bulk surface, which mutates many packages at once and
    // bypasses the ownership check the per-package route applies. Resolving it
    // against the registry tier would hand every `role:user` an endpoint
    // `require_admin` reserved — which a pre-existing test caught, and which is
    // exactly the widening §7 calls the migration's central risk.
    //
    // The owner arm (RFC 0040 §6.4) is narrower than that widening: explicit
    // ownership of every package, on a kind whose native owner route grants
    // the verb, and each item through `LocalRegistryService`.
    let mut items = bulk_items(body);
    let arm = authorize_lifecycle(
        &identity,
        batlehub_core::entities::Action::ReleasesYank,
        LifecycleVerb::Yank,
        &registry,
        &mut items,
        &local_svc,
        &hot,
    )
    .await?;
    let result = match arm {
        Arm::Owner => {
            run_as_owner(
                &local_svc,
                &registry,
                &items,
                LifecycleVerb::Yank,
                &identity.0,
            )
            .await
        }
        Arm::Admin => {
            let result = local_svc
                .backend
                .bulk_yank(&registry, &items)
                .await
                .map_err(AppError::from)?;
            record_bulk_lifecycle_audit(
                &local_svc,
                &registry,
                &items,
                &result,
                AccessAction::Yank,
                &identity.0,
            )
            .await;
            result
        }
    };
    Ok(HttpResponse::Ok().json(bulk_response(result)))
}

/// Bulk-unyank versions in a local/hybrid registry (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/bulk-unyank",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = BulkPackageRequest,
    responses(
        (status = 200, description = "Bulk unyank result", body = BulkPackageResponse),
        (status = 403, description = "`releases:yank` required, or explicit ownership of \
                                      every package on a kind whose owners may yank"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/bulk-unyank")]
pub async fn bulk_unyank(
    path: web::Path<String>,
    body: web::Json<BulkPackageRequest>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    // **Instance tier, not the registry.** §10 rule 5 grants `releases:yank`
    // and `releases:delete` to `role:user` on every local and hybrid registry,
    // because that is what `has_role_at_least(&Role::User)` meant on the
    // per-package lifecycle path. This is not that path: it is the
    // administrative bulk surface, which mutates many packages at once and
    // bypasses the ownership check the per-package route applies. Resolving it
    // against the registry tier would hand every `role:user` an endpoint
    // `require_admin` reserved — which a pre-existing test caught, and which is
    // exactly the widening §7 calls the migration's central risk.
    //
    // The owner arm (RFC 0040 §6.4) is narrower than that widening: explicit
    // ownership of every package, on a kind whose native owner route grants
    // the verb, and each item through `LocalRegistryService`.
    let mut items = bulk_items(body);
    let arm = authorize_lifecycle(
        &identity,
        batlehub_core::entities::Action::ReleasesYank,
        LifecycleVerb::Unyank,
        &registry,
        &mut items,
        &local_svc,
        &hot,
    )
    .await?;
    let result = match arm {
        Arm::Owner => {
            run_as_owner(
                &local_svc,
                &registry,
                &items,
                LifecycleVerb::Unyank,
                &identity.0,
            )
            .await
        }
        Arm::Admin => {
            let result = local_svc
                .backend
                .bulk_unyank(&registry, &items)
                .await
                .map_err(AppError::from)?;
            record_bulk_lifecycle_audit(
                &local_svc,
                &registry,
                &items,
                &result,
                AccessAction::Unyank,
                &identity.0,
            )
            .await;
            result
        }
    };
    Ok(HttpResponse::Ok().json(bulk_response(result)))
}

/// Bulk-delete versions from a local/hybrid registry (admin).
///
/// **The coordinates are spent, not freed** (RFC 0016 §4.4). Each version is
/// tombstoned: the bytes, the README and the listing entry go, and the version
/// number can never be published again. This is the only local-registry delete
/// path in the API, so it is also the only place a coordinate is ever burned.
///
/// Unlike the sibling bulk handlers this does not reach into `backend` directly.
/// A delete is four coordinated effects — tombstone, bytes, README, cache — and
/// they belong together in `LocalRegistryService::delete_version`, next to the
/// ordering constraint that makes them safe to interrupt.
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/bulk-delete",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = BulkPackageRequest,
    responses(
        (status = 200, description = "Bulk delete result; deleted coordinates are \
                                      permanently spent and cannot be republished",
            body = BulkPackageResponse),
        (status = 403, description = "`releases:delete` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/bulk-delete")]
pub async fn bulk_delete(
    path: web::Path<String>,
    body: web::Json<BulkPackageRequest>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
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
    let items = bulk_items(body);

    let mut result = BulkResult {
        processed: items.len(),
        succeeded: 0,
        failed: vec![],
    };
    for (name, version) in &items {
        match local_svc
            .delete_version(&registry, name, version, &identity.0)
            .await
        {
            // `false` — nothing there to delete — counts as succeeded: the caller
            // asked for the coordinate to be gone, and it is. Re-running a partly
            // applied bulk delete should not report failures for the half that
            // already went.
            Ok(_) => result.succeeded += 1,
            Err(e) => result
                .failed
                .push((name.clone(), version.clone(), e.to_string())),
        }
    }

    // No `record_bulk_lifecycle_audit` here, unlike the siblings above:
    // `delete_version` audits each coordinate it actually tombstones, and adding
    // a second pass would double every event. The difference from yank/unyank is
    // deliberate — a delete of a version that was already gone did nothing, and
    // an audit trail that records it anyway makes a burned coordinate look like
    // it was burned twice, by two different people, on two different days.
    Ok(HttpResponse::Ok().json(bulk_response(result)))
}

// ── Deprecation & unlisting (single version) ────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct PackageVersionRequest {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DeprecateRequest {
    pub name: String,
    pub version: String,
    /// Optional human-readable deprecation message (mirrored into npm's native
    /// `deprecated` field for npm registries).
    #[serde(default)]
    pub message: Option<String>,
}

/// Flag a version as deprecated. It stays listed and downloadable (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/deprecate",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = DeprecateRequest,
    responses(
        (status = 200, description = "Version deprecated", body = OkResponse),
        (status = 403, description = "`releases:yank` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/deprecate")]
pub async fn deprecate(
    path: web::Path<String>,
    body: web::Json<DeprecateRequest>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
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
        batlehub_core::entities::Action::ReleasesYank,
        None,
        &hot,
    )
    .await?;
    let body = body.into_inner();
    local_svc
        .deprecate(
            &registry,
            &body.name,
            &body.version,
            body.message.as_deref(),
            &identity.0,
        )
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// Reverse a deprecation (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/undeprecate",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = PackageVersionRequest,
    responses(
        (status = 200, description = "Version undeprecated", body = OkResponse),
        (status = 403, description = "`releases:yank` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/undeprecate")]
pub async fn undeprecate(
    path: web::Path<String>,
    body: web::Json<PackageVersionRequest>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
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
        batlehub_core::entities::Action::ReleasesYank,
        None,
        &hot,
    )
    .await?;
    let body = body.into_inner();
    local_svc
        .undeprecate(&registry, &body.name, &body.version, &identity.0)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// Hide a version from registry-protocol listings; still downloadable by exact
/// coordinate (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/unlist",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = PackageVersionRequest,
    responses(
        (status = 200, description = "Version unlisted", body = OkResponse),
        (status = 403, description = "`releases:yank` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/unlist")]
pub async fn unlist(
    path: web::Path<String>,
    body: web::Json<PackageVersionRequest>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
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
        batlehub_core::entities::Action::ReleasesYank,
        None,
        &hot,
    )
    .await?;
    let body = body.into_inner();
    local_svc
        .unlist(&registry, &body.name, &body.version, &identity.0)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}

/// Make an unlisted version visible in listings again (admin).
#[utoipa::path(
    post,
    path = "/api/v1/admin/registries/{registry}/relist",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = PackageVersionRequest,
    responses(
        (status = 200, description = "Version relisted", body = OkResponse),
        (status = 403, description = "`releases:yank` required"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/admin/registries/{registry}/relist")]
pub async fn relist(
    path: web::Path<String>,
    body: web::Json<PackageVersionRequest>,
    identity: AuthIdentity,
    local_svc: web::Data<Arc<LocalRegistryService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
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
        batlehub_core::entities::Action::ReleasesYank,
        None,
        &hot,
    )
    .await?;
    let body = body.into_inner();
    local_svc
        .relist(&registry, &body.name, &body.version, &identity.0)
        .await
        .map_err(AppError::from)?;
    Ok(HttpResponse::Ok().json(OkResponse::new()))
}
