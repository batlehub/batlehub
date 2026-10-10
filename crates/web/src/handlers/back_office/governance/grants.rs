//! The grants editor's three routes — RFC 0017 §4.1.
//!
//! A thin translation of HTTP onto [`GrantAdminService`]. Every rule about what
//! a legal grant is lives in that service, because the CLI reaches the same
//! rules through these same routes and a validation implemented here would be
//! one the service's own tests cannot see.

use std::sync::Arc;

use actix_web::{delete, get, put, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_core::{
    entities::{AccessAction, Action, PackageId},
    services::{AdminService, GrantAdminService, GrantTarget},
};

use crate::{error::AppError, extractors::AuthIdentity};

/// One stored grant, as the API reports it.
#[derive(Debug, Serialize, ToSchema)]
pub struct GrantDto {
    /// `package` or `version`.
    pub node_kind: String,
    /// The package name, or `name@version` for a version-tier row — the same
    /// spelling `version_node_key` stores and the CLI's positional argument
    /// takes, so the three never have to be translated between.
    pub node_key: String,
    pub subject: String,
    pub actions: Vec<String>,
    pub granted_by: Option<String>,
    /// Whether this row is the ownership projection's rather than the editor's.
    ///
    /// The console needs to know before it offers an edit control: these rows
    /// are refused by `PUT` and `DELETE` (§4.3), and a UI that discovers that
    /// from a `409` has already let the operator fill in a form.
    pub from_ownership: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GrantListResponse {
    pub grants: Vec<GrantDto>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutGrantRequest {
    pub package: String,
    /// Absent addresses the package node; present, the version node.
    #[serde(default)]
    pub version: Option<String>,
    /// A subject spelling: `user:alice`, `group:oidc1:eng`, `role:user`, `*`.
    pub subject: String,
    /// Action patterns. `releases:*` is expanded here, at write, never at
    /// evaluation (§4.2) — the stored row carries the expanded set.
    pub actions: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DeleteGrantRequest {
    pub package: String,
    #[serde(default)]
    pub version: Option<String>,
    pub subject: String,
}

/// What was written, and what the operator should know about it.
#[derive(Debug, Serialize, ToSchema)]
pub struct PutGrantResponse {
    /// The expanded action set actually stored, which is not always what was
    /// asked for: `releases:*` names one thing and stores six.
    pub actions: Vec<String>,
    /// Legal but probably not intended — a redundant grant, a spent coordinate.
    /// Reported rather than refused (§4.4).
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteGrantResponse {
    /// Whether a row was actually there. `false` is a success: removal is
    /// idempotent, and an operator who ran it twice deserves to know which.
    pub removed: bool,
}

/// Ownership rows carry exactly these three verbs and no others, which is what
/// makes them identifiable without a second column (see `OWNERSHIP_ACTIONS`).
fn looks_like_ownership(actions: &[Action]) -> bool {
    let mut sorted: Vec<&str> = actions.iter().map(|a| a.as_str()).collect();
    sorted.sort_unstable();
    sorted == ["owners:read", "owners:write", "releases:publish"]
}

fn to_dto(g: batlehub_core::ports::StoredGrant) -> GrantDto {
    GrantDto {
        node_kind: g.node_kind.as_str().to_owned(),
        node_key: g.node_key,
        subject: g.subject.to_string(),
        from_ownership: looks_like_ownership(&g.actions),
        actions: g.actions.iter().map(|a| a.as_str().to_owned()).collect(),
        granted_by: g.granted_by,
    }
}

/// `503` when grant storage is not wired, the shape `require_ownership` uses
/// for the ownership port: a deployment without it has no editor, and that is a
/// deployment fact rather than a bad request.
async fn require_grant_storage(svc: &GrantAdminService) -> Result<(), AppError> {
    if svc.is_configured().await {
        Ok(())
    } else {
        Err(AppError::service_unavailable(
            "grant storage is not configured",
        ))
    }
}

/// The audit coordinate for a grant mutation.
///
/// A version-tier row carries its version, a package-tier row the empty string —
/// the "the package as a whole" convention `visibility.rs` and `ownership.rs`
/// already use, so the timeline renders all three the same way.
fn audit_id(target: &GrantTarget) -> PackageId {
    PackageId {
        registry: target.registry.clone(),
        name: target.package.clone(),
        version: target.version.clone().unwrap_or_default(),
        artifact: None,
    }
}

// ── List ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct GrantQuery {
    /// The package to report grants for.
    pub package: String,
    /// When present, only that version node. When absent, the package node
    /// **and** every version node beneath it — which is what an operator asking
    /// "who can reach this package" actually means.
    #[serde(default)]
    pub version: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/registries/{registry}/grants",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name"), GrantQuery),
    responses(
        (status = 200, description = "Grants on the addressed node(s)", body = GrantListResponse),
        (status = 403, description = "`grants:read` required"),
        (status = 503, description = "Grant storage not configured"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/registries/{registry}/grants")]
pub async fn list_grants(
    path: web::Path<String>,
    query: web::Query<GrantQuery>,
    identity: AuthIdentity,
    svc: web::Data<Arc<GrantAdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        Action::GrantsRead,
        Some(&registry),
        &hot,
    )
    .await?;
    require_grant_storage(&svc).await?;

    let grants = match &query.version {
        Some(v) => {
            svc.list(&GrantTarget::version(&registry, &query.package, v))
                .await?
        }
        None => svc.list_for_package(&registry, &query.package).await?,
    };

    Ok(HttpResponse::Ok().json(GrantListResponse {
        grants: grants.into_iter().map(to_dto).collect(),
    }))
}

// ── Write ─────────────────────────────────────────────────────────────────────

#[utoipa::path(
    put,
    path = "/api/v1/admin/registries/{registry}/grants",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = PutGrantRequest,
    responses(
        (status = 200, description = "Grant written", body = PutGrantResponse),
        (status = 400, description = "Unknown action, unparseable subject, empty action set, or a version that does not exist"),
        (status = 403, description = "`grants:write` required"),
        (status = 409, description = "The subject holds these verbs through ownership"),
        (status = 503, description = "Grant storage not configured"),
    ),
    security(("bearer_token" = [])),
)]
#[put("/api/v1/admin/registries/{registry}/grants")]
pub async fn put_grant(
    path: web::Path<String>,
    body: web::Json<PutGrantRequest>,
    identity: AuthIdentity,
    svc: web::Data<Arc<GrantAdminService>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        Action::GrantsWrite,
        Some(&registry),
        &hot,
    )
    .await?;
    require_grant_storage(&svc).await?;

    let target = match &body.version {
        Some(v) => GrantTarget::version(&registry, &body.package, v),
        None => GrantTarget::package(&registry, &body.package),
    };

    let warnings = svc
        .set(&target, &body.subject, &body.actions, &identity.0)
        .await?;

    // Every mutation is audited (§7): "who could read this, and since when" is
    // the question after an incident, and a grant write is the only event that
    // answers it.
    admin_svc
        .record_admin_action_about(
            Some(audit_id(&target)),
            AccessAction::GrantWrite,
            &identity.0,
            &identity.1,
            Some(grant_detail(&body.subject, &body.actions)),
        )
        .await;

    let stored = svc.list(&target).await?;
    let actions = stored
        .into_iter()
        .find(|g| g.subject.to_string() == body.subject)
        .map(|g| g.actions.iter().map(|a| a.as_str().to_owned()).collect())
        .unwrap_or_default();

    Ok(HttpResponse::Ok().json(PutGrantResponse {
        actions,
        warnings: warnings.iter().map(|w| w.to_string()).collect(),
    }))
}

// ── Remove ────────────────────────────────────────────────────────────────────

#[utoipa::path(
    delete,
    path = "/api/v1/admin/registries/{registry}/grants",
    tag = "back-office",
    params(("registry" = String, Path, description = "Registry name")),
    request_body = DeleteGrantRequest,
    responses(
        (status = 200, description = "Grant removed (or was already absent)", body = DeleteGrantResponse),
        (status = 400, description = "Unparseable subject"),
        (status = 403, description = "`grants:write` required"),
        (status = 409, description = "The subject holds this grant through ownership"),
        (status = 503, description = "Grant storage not configured"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/admin/registries/{registry}/grants")]
pub async fn delete_grant(
    path: web::Path<String>,
    body: web::Json<DeleteGrantRequest>,
    identity: AuthIdentity,
    svc: web::Data<Arc<GrantAdminService>>,
    admin_svc: web::Data<Arc<AdminService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let registry = path.into_inner();
    crate::handlers::back_office::require_verb(
        &identity,
        Action::GrantsWrite,
        Some(&registry),
        &hot,
    )
    .await?;
    require_grant_storage(&svc).await?;

    let target = match &body.version {
        Some(v) => GrantTarget::version(&registry, &body.package, v),
        None => GrantTarget::package(&registry, &body.package),
    };

    let removed = svc.remove(&target, &body.subject).await?;

    // Recorded even when nothing was there. A revocation that left no event
    // would make the trail read as "granted, still held" (§7), and "someone
    // tried to revoke this" is itself worth having.
    admin_svc
        .record_admin_action_about(
            Some(audit_id(&target)),
            AccessAction::GrantRevoke,
            &identity.0,
            &identity.1,
            Some(grant_detail(&body.subject, &[])),
        )
        .await;

    Ok(HttpResponse::Ok().json(DeleteGrantResponse { removed }))
}

/// A grant event's `detail`: who it was for and which verbs. The subject is
/// what `grant_to_anonymous.yml` keys on (RFC 0036 §6.2), so its spelling here
/// — `subject=<subject> actions=<verbs>` — is part of the stream's contract.
fn grant_detail(subject: &str, actions: &[String]) -> String {
    if actions.is_empty() {
        return format!("subject={subject}");
    }
    format!("subject={subject} actions={}", actions.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::Action;

    #[test]
    fn ownership_rows_are_recognised_by_their_exact_three_verbs() {
        assert!(looks_like_ownership(&[
            Action::ReleasesPublish,
            Action::OwnersRead,
            Action::OwnersWrite,
        ]));
        // Order does not matter — the projection's order is not a contract.
        assert!(looks_like_ownership(&[
            Action::OwnersWrite,
            Action::OwnersRead,
            Action::ReleasesPublish,
        ]));
    }

    #[test]
    fn a_row_with_a_fourth_verb_is_not_an_ownership_row() {
        assert!(!looks_like_ownership(&[
            Action::ReleasesPublish,
            Action::OwnersRead,
            Action::OwnersWrite,
            Action::ReleasesRead,
        ]));
        assert!(!looks_like_ownership(&[Action::ReleasesRead]));
        assert!(!looks_like_ownership(&[]));
    }

    #[test]
    fn a_version_target_audits_its_version_and_a_package_target_audits_none() {
        let v = audit_id(&GrantTarget::version("npm1", "pkg", "1.0.0"));
        assert_eq!(v.version, "1.0.0");
        let p = audit_id(&GrantTarget::package("npm1", "pkg"));
        assert_eq!(p.version, "", "the package as a whole");
    }
}
