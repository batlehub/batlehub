//! The typed read-only JSON routes `[registries.api_reads]` adds (RFC 0019
//! §4.1 and §4.2 *API reads*, phase 3).
//!
//! Three families — `tags`, `commits`, `branches` — each a `GET`, each
//! answering **this proxy's own shape** rather than the forge's. That is the
//! whole of the design decision §11 q11 records as "no wildcard": a typed
//! answer has no upstream URL to rewrite, no field that means something
//! different per forge, and no room to grow into a passthrough. `contents`
//! and `git/blobs` are never accepted, because they are raw content by
//! another door and `[registries.raw]` is where that decision lives.
//!
//! A family the registry did not opt into answers `404`, which is what the
//! route did before it existed.

use std::sync::Arc;

use actix_web::{get, web, Responder};
use serde::Serialize;
use utoipa::ToSchema;

use batlehub_core::{
    entities::{Action, ApiReadFamily, PackageId},
    services::{authz, ProxyService},
};

use crate::{error::AppError, extractors::AuthIdentity};

#[derive(Serialize, ToSchema)]
pub struct ForgeTagDto {
    pub name: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ForgeTagsResponse {
    pub registry: String,
    pub package: String,
    pub tags: Vec<ForgeTagDto>,
}

#[derive(Serialize, ToSchema)]
pub struct ForgeCommitResponse {
    pub registry: String,
    pub package: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committer: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ForgeBranchResponse {
    pub registry: String,
    pub package: String,
    pub branch: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committer: Option<String>,
}

/// The forge client for `registry`, once the family is enabled and the
/// caller may read the repository.
async fn forge_for(
    svc: &web::Data<Arc<ProxyService>>,
    identity: &AuthIdentity,
    registry: &str,
    owner_repo: &str,
    family: ApiReadFamily,
) -> Result<Arc<dyn batlehub_core::ports::RegistryClient>, AppError> {
    batlehub_core::services::validate_package_name(owner_repo).map_err(AppError::from)?;
    let (client, enabled) = {
        let hot = svc.hot.read().await;
        (
            hot.registries.get(registry).cloned(),
            hot.forge_api_reads
                .get(registry)
                .is_some_and(|f| f.contains(&family)),
        )
    };
    let Some(client) = client else {
        return Err(AppError::not_found(format!("unknown registry: {registry}")));
    };
    if !enabled {
        return Err(AppError::not_found(format!(
            "registry '{registry}' does not serve the '{}' family; add it to \
             [registries.api_reads].families to turn it on (RFC 0019 §4.1)",
            family.as_str()
        )));
    }
    if client.forge().is_none() {
        return Err(AppError::not_found(format!(
            "registry '{registry}' is not a git forge"
        )));
    }
    // The same verb the release listing asks for, resolved against the same
    // hierarchy: a repository whose releases a caller may not read does not
    // disclose its tags either.
    let pkg = PackageId::new(registry, owner_repo, "*");
    authz::authorize_grants_public(&svc.hot, &pkg, &identity.0, Action::ReleasesRead)
        .await
        .map_err(|_| {
            AppError::forbidden("this endpoint requires the 'releases:read' permission")
        })?;
    Ok(client)
}

/// List a repository's tags.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{owner}/{repo}/tags",
    tag = "proxy/github",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("owner"    = String, Path, description = "Repository owner"),
        ("repo"     = String, Path, description = "Repository name"),
    ),
    responses(
        (status = 200, description = "The repository's tags", body = ForgeTagsResponse),
        (status = 403, description = "`releases:read` required"),
        (status = 404, description = "Unknown registry, not a forge, or the family is not enabled"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{owner}/{repo}/tags")]
pub async fn forge_tags(
    path: web::Path<(String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<impl Responder, AppError> {
    let (registry, owner, repo) = path.into_inner();
    let owner_repo = format!("{owner}/{repo}");
    let client = forge_for(&svc, &identity, &registry, &owner_repo, ApiReadFamily::Tags).await?;
    let forge = client.forge().expect("checked above");
    let tags = forge
        .tags(&owner_repo)
        .await
        .map_err(AppError::from)?
        .into_iter()
        .map(|t| ForgeTagDto {
            name: t.name,
            sha: t.sha,
            date: t.date.map(|d| d.to_rfc3339()),
        })
        .collect();
    Ok(web::Json(ForgeTagsResponse {
        registry,
        package: owner_repo,
        tags,
    }))
}

/// One commit's date and committer.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{owner}/{repo}/commits/{sha}",
    tag = "proxy/github",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("owner"    = String, Path, description = "Repository owner"),
        ("repo"     = String, Path, description = "Repository name"),
        ("sha"      = String, Path, description = "Commit SHA"),
    ),
    responses(
        (status = 200, description = "The commit", body = ForgeCommitResponse),
        (status = 403, description = "`releases:read` required"),
        (status = 404, description = "Unknown registry, not a forge, or the family is not enabled"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{owner}/{repo}/commits/{sha}")]
pub async fn forge_commit(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<impl Responder, AppError> {
    let (registry, owner, repo, sha) = path.into_inner();
    let owner_repo = format!("{owner}/{repo}");
    batlehub_core::services::validate_path_safe("commit", &sha).map_err(AppError::from)?;
    let client = forge_for(
        &svc,
        &identity,
        &registry,
        &owner_repo,
        ApiReadFamily::Commits,
    )
    .await?;
    let forge = client.forge().expect("checked above");
    let commit = forge
        .commit(&owner_repo, &sha)
        .await
        .map_err(AppError::from)?;
    Ok(web::Json(ForgeCommitResponse {
        registry,
        package: owner_repo,
        sha: commit.sha,
        committed_at: commit.committed_at.map(|d| d.to_rfc3339()),
        committer: commit.committer,
    }))
}

/// A branch head.
#[utoipa::path(
    get,
    path = "/proxy/{registry}/{owner}/{repo}/branches/{branch}",
    tag = "proxy/github",
    params(
        ("registry" = String, Path, description = "Registry name"),
        ("owner"    = String, Path, description = "Repository owner"),
        ("repo"     = String, Path, description = "Repository name"),
        ("branch"   = String, Path, description = "Branch name"),
    ),
    responses(
        (status = 200, description = "The branch head", body = ForgeBranchResponse),
        (status = 403, description = "`releases:read` required"),
        (status = 404, description = "Unknown registry, not a forge, the family is not enabled, or no such branch"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/proxy/{registry}/{owner}/{repo}/branches/{branch}")]
pub async fn forge_branch(
    path: web::Path<(String, String, String, String)>,
    identity: AuthIdentity,
    svc: web::Data<Arc<ProxyService>>,
) -> Result<impl Responder, AppError> {
    let (registry, owner, repo, branch) = path.into_inner();
    let owner_repo = format!("{owner}/{repo}");
    batlehub_core::services::validate_path_safe("branch", &branch).map_err(AppError::from)?;
    let client = forge_for(
        &svc,
        &identity,
        &registry,
        &owner_repo,
        ApiReadFamily::Branches,
    )
    .await?;
    let forge = client.forge().expect("checked above");
    // Through the resolver, so the answer is the same commit the read path
    // would serve for `tarball/{branch}` and the resolution is remembered —
    // one source of truth for "what does this branch point at".
    let (store, policy) = {
        let hot = svc.hot.read().await;
        (
            hot.ref_resolutions.clone(),
            hot.forge_refs.get(&registry).copied().unwrap_or_default(),
        )
    };
    let resolved = batlehub_core::services::forge_refs::resolve_ref(
        &registry,
        forge,
        store.as_ref(),
        policy,
        &owner_repo,
        &branch,
    )
    .await
    .map_err(AppError::from)?;
    if resolved.kind != batlehub_core::entities::RefKind::Branch {
        return Err(AppError::not_found(format!(
            "{owner_repo}: '{branch}' is not a branch"
        )));
    }
    Ok(web::Json(ForgeBranchResponse {
        registry,
        package: owner_repo,
        branch,
        sha: resolved.sha,
        committed_at: resolved.object_date.map(|d| d.to_rfc3339()),
        committer: resolved.publisher,
    }))
}
