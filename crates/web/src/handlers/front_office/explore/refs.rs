//! The moving refs of a forge repository (RFC 0019 §6.5, phase 2).
//!
//! A package registry's version list is the whole truth about it. A forge's
//! is not: `main` is a version that changes under the same name, and a tag
//! can be moved. This endpoint answers what the *instance* has seen — every
//! ref it resolved for one repository, what it resolved to, when, and what
//! it resolved to before — which is the fact the console's panel shows and
//! the fact `TAG_MOVED` was derived from.
//!
//! Not the forge's branch list: this is deliberately what BatleHub
//! remembers, not what the forge currently has. A branch nobody pulled
//! through the proxy is not here, and that is the honest answer to "what did
//! this instance serve".

use super::{get, web, AppError, Arc, AuthIdentity, ProxyService, Responder, Serialize, ToSchema};

#[derive(Serialize, ToSchema)]
pub struct ForgeRefDto {
    /// The ref as a client spelled it: `main`, `v2.60.0`.
    pub git_ref: String,
    /// `tag` or `branch`; a commit is never recorded — it cannot move.
    pub ref_kind: String,
    pub sha: String,
    pub resolved_at: String,
    /// What it resolved to before, when that differs: a branch that advanced,
    /// or a tag that moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_sha: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ForgeRefsResponse {
    pub registry: String,
    /// `owner/repo`.
    pub package: String,
    pub refs: Vec<ForgeRefDto>,
    /// `false` when this deployment remembers no resolutions at all (no
    /// database): the panel then says so rather than showing an empty list
    /// that reads as "nothing moves here".
    pub remembered: bool,
}

/// Every ref this instance has resolved for a forge repository.
#[utoipa::path(
    get,
    path = "/api/v1/explore/{registry}/{name}/refs",
    tag = "explore",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("name" = String, Path, description = "`owner/repo`, percent-encoded"),
    ),
    responses(
        (status = 200, description = "The remembered refs, newest resolution first", body = ForgeRefsResponse),
        (status = 403, description = "`catalogue:browse` required on the registry"),
        (status = 404, description = "Not a forge registry"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/explore/{registry}/{name}/refs")]
pub async fn explore_forge_refs(
    path: web::Path<(String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
    hot: web::Data<batlehub_core::services::hot_config::HotConfigLock>,
) -> Result<impl Responder, AppError> {
    let (registry, name) = path.into_inner();
    batlehub_core::services::validate_package_name(&name).map_err(AppError::from)?;
    // The same verb the rest of the explorer resolves, on the same registry
    // set: a repository whose catalogue a caller may not browse does not
    // disclose which of its branches this instance has followed.
    let browsable = batlehub_core::services::authz::browsable_registries(&hot, &identity).await;
    if !browsable.contains(&registry) {
        return Err(AppError::forbidden(
            "this endpoint requires the 'catalogue:browse' permission",
        ));
    }
    let (is_forge, store) = {
        let hot = svc.hot.read().await;
        (
            hot.registries
                .get(&registry)
                .and_then(|c| {
                    c.registry_type()
                        .parse::<batlehub_core::entities::RegistryKind>()
                        .ok()
                })
                .is_some_and(|k| k.is_forge()),
            hot.ref_resolutions.clone(),
        )
    };
    if !is_forge {
        return Err(AppError::not_found(format!(
            "registry '{registry}' is not a git forge"
        )));
    }
    let refs = match &store {
        Some(store) => store
            .list_for_repo(&registry, &name)
            .await
            .map_err(AppError::from)?,
        None => Vec::new(),
    };
    Ok(web::Json(ForgeRefsResponse {
        registry,
        package: name,
        remembered: store.is_some(),
        refs: refs
            .into_iter()
            .map(|(git_ref, r)| ForgeRefDto {
                git_ref,
                ref_kind: r.kind.as_str().to_owned(),
                sha: r.sha,
                resolved_at: super::format_dt(r.resolved_at),
                previous_sha: r.previous,
            })
            .collect(),
    }))
}
