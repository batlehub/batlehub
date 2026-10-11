use std::sync::Arc;

use actix_web::{delete, get, post, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use batlehub_adapters::auth::generate_token;
use batlehub_core::{
    entities::{snapshot_pat_groups, Role},
    ports::{TokenOwner, UserTokenRepository},
};

use super::OidcProviderNames;
use batlehub_core::entities::{AccessAction, AccessEvent, AccessResult, Identity};
use batlehub_core::services::AdminService;

use crate::{error::AppError, extractors::AuthIdentity};

// ── Create token ──────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    /// Display name for the token.
    pub name: String,
    /// Lifetime in days (1–90).
    pub expires_in_days: u64,
    /// Role for this token. Must be ≤ the caller's own role.
    /// Accepts "user" or "admin" (admin callers only for "admin").
    pub role: String,
    /// Groups to snapshot onto the token. Each must be one the caller holds;
    /// asking for one they do not is `403`, not a silent drop.
    ///
    /// Omitted or empty means no groups, which is what every token minted
    /// before this field carried. "All of mine" is a client-side convenience
    /// (`batlehub auth token create --all-groups`) that sends the resolved list
    /// here, not a second server-side spelling of the same thing.
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CreateTokenResponse {
    pub id: Uuid,
    pub name: String,
    /// Raw token — displayed exactly once. Store it securely.
    pub token: String,
    pub role: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// The group snapshot this token carries. Shown at creation because it is
    /// the only moment it can be changed — a snapshot is fixed for the life of
    /// the token, so a wrong one is re-created, never edited.
    pub groups: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/api/v1/auth/tokens",
    tag = "front-office",
    request_body = CreateTokenRequest,
    responses(
        (status = 201, description = "Token created", body = CreateTokenResponse),
        (status = 400, description = "Invalid request (bad lifetime or role)"),
        (status = 403, description = "Not an OIDC session, insufficient role, or a requested group the caller does not hold"),
    ),
    security(("bearer_token" = [])),
)]
#[post("/api/v1/auth/tokens")]
pub async fn create_token(
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn UserTokenRepository>>,
    admin_svc: web::Data<Arc<AdminService>>,
    oidc_providers: web::Data<OidcProviderNames>,
    body: web::Json<CreateTokenRequest>,
) -> Result<impl Responder, AppError> {
    let owner = oidc_session_owner(&identity, &oidc_providers)
        .ok_or_else(|| AppError::forbidden("only OIDC sessions can create API tokens"))?;

    if body.expires_in_days == 0 || body.expires_in_days > 90 {
        return Err(AppError::bad_request(
            "expires_in_days must be between 1 and 90",
        ));
    }

    let requested_role = parse_role(&body.role)
        .ok_or_else(|| AppError::bad_request("role must be 'user' or 'admin'"))?;

    if requested_role > identity.role {
        return Err(AppError::forbidden(
            "token role cannot exceed your own role",
        ));
    }

    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("token name cannot be empty"));
    }

    // The subset invariant of RFC 0015 §4.3, enforced at the one moment it can
    // be: a token that outlives its creator's membership is a stale grant, but a
    // token that never had the membership at all is privilege escalation. This
    // is the only place a PAT's groups are chosen, so it is the only place the
    // check has to hold.
    let groups = snapshot_pat_groups(&body.groups, &identity).map_err(|missing| {
        AppError::forbidden(format!(
            "a token cannot carry groups you do not hold: {}",
            missing.join(", ")
        ))
    })?;

    let expires_at = Utc::now() + chrono::Duration::days(body.expires_in_days as i64);

    let (raw_token, token_hash) = generate_token();
    let id = Uuid::new_v4();

    let tok = repo
        .create_token(
            id,
            &owner,
            name,
            &token_hash,
            requested_role.clone(),
            expires_at,
            &groups,
        )
        .await
        .map_err(AppError::from)?;

    // Minting and revoking a long-lived credential are the two events worth
    // reconstructing after an incident. At `info` so they survive the default
    // filter, and without the token or its hash — the id and the owner are what
    // an investigation needs.
    tracing::info!(
        token_id = %tok.id,
        provider = %owner.provider,
        user_id = %owner.user_id,
        role = %tok.role,
        name = %tok.name,
        expires_at = %tok.expires_at,
        groups = %tok.groups.join(","),
        "personal access token created"
    );
    admin_svc
        .record_event(AccessEvent::about_identity(
            AccessAction::TokenCreate,
            identity.user_id.clone(),
            identity.role.clone(),
            AccessResult::Allowed,
            identity.1.clone(),
            Some(token_detail(tok.id, Some(&tok.name))),
        ))
        .await;

    Ok(HttpResponse::Created().json(CreateTokenResponse {
        id: tok.id,
        name: tok.name,
        token: raw_token,
        role: tok.role.to_string(),
        expires_at: tok.expires_at,
        created_at: tok.created_at,
        groups: tok.groups,
    }))
}

// ── List tokens ───────────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct TokenListItem {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// When this token was last presented, or `null` if not since the server
    /// started recording. What tells a user which of their tokens is dormant and
    /// safe to revoke, and an operator which one moved after a leak.
    pub last_used_at: Option<DateTime<Utc>>,
    /// The groups this token resolves to. Listed because a snapshot goes stale
    /// silently: nothing tells its owner that the token still carries a team
    /// they left, and this column is where they can see it.
    pub groups: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/api/v1/auth/tokens",
    tag = "front-office",
    responses(
        (status = 200, description = "List of active tokens", body = Vec<TokenListItem>),
        (status = 403, description = "Not an OIDC or user-token session"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/auth/tokens")]
pub async fn list_tokens(
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn UserTokenRepository>>,
    oidc_providers: web::Data<OidcProviderNames>,
) -> Result<impl Responder, AppError> {
    let owner = oidc_session_owner(&identity, &oidc_providers)
        .ok_or_else(|| AppError::forbidden("only OIDC sessions can list API tokens"))?;

    let tokens = repo.list_for_user(&owner).await?;

    let items: Vec<TokenListItem> = tokens
        .into_iter()
        .map(|t| TokenListItem {
            id: t.id,
            name: t.name,
            role: t.role.to_string(),
            expires_at: t.expires_at,
            created_at: t.created_at,
            last_used_at: t.last_used_at,
            groups: t.groups,
        })
        .collect();

    Ok(HttpResponse::Ok().json(items))
}

// ── Revoke token ──────────────────────────────────────────────────────────────

#[utoipa::path(
    delete,
    path = "/api/v1/auth/tokens/{id}",
    tag = "front-office",
    params(("id" = Uuid, Path, description = "Token ID")),
    responses(
        (status = 204, description = "Token revoked"),
        (status = 404, description = "Token not found or not owned by caller"),
        (status = 403, description = "Not authenticated"),
    ),
    security(("bearer_token" = [])),
)]
#[delete("/api/v1/auth/tokens/{id}")]
pub async fn revoke_token(
    path: web::Path<Uuid>,
    identity: AuthIdentity,
    repo: web::Data<Arc<dyn UserTokenRepository>>,
    admin_svc: web::Data<Arc<AdminService>>,
    oidc_providers: web::Data<OidcProviderNames>,
) -> Result<impl Responder, AppError> {
    let owner = oidc_session_owner(&identity, &oidc_providers)
        .ok_or_else(|| AppError::forbidden("only OIDC sessions can revoke API tokens"))?;

    let id = path.into_inner();
    let revoked = repo.revoke(id, &owner).await?;

    if revoked {
        tracing::info!(
            token_id = %id,
            provider = %owner.provider,
            user_id = %owner.user_id,
            "personal access token revoked"
        );
        admin_svc
            .record_event(AccessEvent::about_identity(
                AccessAction::TokenRevoke,
                identity.user_id.clone(),
                identity.role.clone(),
                AccessResult::Allowed,
                identity.1.clone(),
                Some(token_detail(id, None)),
            ))
            .await;
        Ok(HttpResponse::NoContent().finish())
    } else {
        Err(AppError::not_found("token not found or not owned by you"))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// The principal whose tokens this caller may manage, or `None` if it may
/// manage none.
///
/// Two conditions, and every token endpoint applies both.
///
/// **The session must be an interactive OIDC login.** No machine credential
/// mints a PAT — that would let a static token, a service account or a CI job
/// issue a longer-lived credential — and none may list or revoke either, so a
/// stolen PAT cannot enumerate the rest of its owner's tokens or revoke them out
/// of spite. Matched against the *configured* provider names rather than the
/// literal `"oidc"`: the name is operator-chosen, and comparing the literal
/// locked token management out of every deployment that renamed its provider.
///
/// **Ownership is `(provider, user_id)`, never `user_id` alone.** That id is a
/// bare string each provider picks for itself, so a static `[[auth.tokens]]`
/// entry with `user_id = "alice"`, or a service account whose username equals
/// Alice's OIDC `sub`, would otherwise address her tokens.
fn oidc_session_owner(
    identity: &Identity,
    oidc_providers: &OidcProviderNames,
) -> Option<TokenOwner> {
    let provider = identity.auth_provider.as_deref()?;
    if !oidc_providers.contains(provider) {
        return None;
    }
    let user_id = identity.user_id.as_deref()?;
    Some(TokenOwner::new(provider, user_id))
}

/// A token event's `detail`: its id, and its name when the caller has it.
/// Never the value or the hash (RFC 0036 §6.1).
fn token_detail(id: Uuid, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("token_id={id} name={name:?}"),
        None => format!("token_id={id}"),
    }
}

fn parse_role(s: &str) -> Option<Role> {
    match s {
        "user" => Some(Role::User),
        "admin" => Some(Role::Admin),
        _ => None,
    }
}
