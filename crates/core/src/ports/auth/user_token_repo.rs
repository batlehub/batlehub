use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::entities::Role;
use crate::error::CoreError;

/// Who a personal access token belongs to.
///
/// A `user_id` on its own is not an identity: it is whatever string the
/// authenticating provider chose — an OIDC `sub`, a Kubernetes service account
/// username, a value an operator typed into a static `[[auth.tokens]]` entry.
/// Two providers can hand back the same string for entirely different people, so
/// the provider name has to travel with it. Listing and revocation used to match
/// on `user_id` alone, which meant any identity carrying the same string could
/// enumerate and destroy another principal's tokens.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenOwner {
    /// The `name` of the `[[auth]]` provider that authenticated the caller.
    pub provider: String,
    pub user_id: String,
}

impl TokenOwner {
    pub fn new(provider: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            user_id: user_id.into(),
        }
    }
}

pub struct UserToken {
    pub id: Uuid,
    pub user_id: String,
    /// Provider that authenticated the session which created this token.
    pub provider: String,
    pub name: String,
    pub role: Role,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// When the token was last presented, or `None` if not since this was
    /// recorded. Not the same as "never used" for a token that predates the
    /// column.
    pub last_used_at: Option<DateTime<Utc>>,
    /// The source IP the token was last presented from, written with
    /// `last_used_at`. `None` until its first use since this was recorded —
    /// "unknown", which no `token_new_source` event is ever emitted against.
    pub last_used_ip: Option<String>,
    /// The creator's groups, snapshotted at creation and capped to a subset of
    /// what they held (RFC 0011-bis §4.4).
    ///
    /// A snapshot rather than a live lookup because there is nothing to look up
    /// *from*: a PAT has no session and no refresh token, so re-resolving from
    /// the IDP is not an option that exists. The cost is staleness — a developer
    /// who leaves a team keeps reading that team's packages until the token
    /// expires or is revoked — which is why the TTL is capped and mandatory and
    /// why offboarding means revoking tokens.
    ///
    /// Empty for every token minted before this column, which is exactly what
    /// those tokens already resolved to.
    pub groups: Vec<String>,
}

#[async_trait]
pub trait UserTokenRepository: Send + Sync {
    /// `groups` must already have been capped to the creator's own — see
    /// [`snapshot_pat_groups`]. A repository stores what it is handed; the
    /// subset invariant is decided where the creator's `Identity` is in scope,
    /// which is not here.
    ///
    /// [`snapshot_pat_groups`]: crate::entities::snapshot_pat_groups
    #[allow(clippy::too_many_arguments)]
    async fn create_token(
        &self,
        id: Uuid,
        owner: &TokenOwner,
        name: &str,
        token_hash: &str,
        role: Role,
        expires_at: DateTime<Utc>,
        groups: &[String],
    ) -> Result<UserToken, CoreError>;

    /// Look up an active (non-expired, non-revoked) token by its SHA-256 hash.
    async fn find_by_hash(&self, token_hash: &str) -> Result<Option<UserToken>, CoreError>;

    async fn list_for_user(&self, owner: &TokenOwner) -> Result<Vec<UserToken>, CoreError>;

    /// Record that `id` was just presented, and from where.
    ///
    /// Best-effort and off the critical path: a failure here must never turn a
    /// valid credential into a rejected one, so callers log and carry on.
    async fn touch_last_used(&self, id: Uuid, source_ip: Option<&str>) -> Result<(), CoreError>;

    /// Soft-delete a token. Returns true if a row was revoked.
    ///
    /// Scoped to `owner`, not just to the token id: a caller may only revoke
    /// what it owns, and ownership includes which provider it authenticated
    /// through.
    async fn revoke(&self, id: Uuid, owner: &TokenOwner) -> Result<bool, CoreError>;
}
