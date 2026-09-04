//! The forge-specific ports (RFC 0019 §6.1): what a forge client can answer
//! about a ref, where a resolution is remembered, and the rate-limit budget
//! the proxy and the scan worker share.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::entities::RefKind;
use crate::error::CoreError;

/// What a forge said a ref points at, in one lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub kind: RefKind,
    /// The commit. For an annotated tag, the commit the tag object points at.
    pub sha: String,
    /// The tagger date for an annotated tag, the committer date for a branch
    /// head or a commit, when the lookup that classified the ref also carried
    /// it. `None` means "ask [`ForgeRegistry::commit`]".
    pub object_date: Option<DateTime<Utc>>,
    /// The tagger or committer, as the forge names them (login when it has
    /// one, else `name <email>`).
    pub publisher: Option<String>,
}

/// One commit, as the forge describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeCommit {
    pub sha: String,
    pub committed_at: Option<DateTime<Utc>>,
    pub committer: Option<String>,
}

/// The read-only questions a forge client answers beyond `RegistryClient`
/// (RFC 0019 §6.1). Reached through [`super::RegistryClient::forge`]; a
/// client that is not a forge returns `None` there and none of this applies.
#[async_trait]
pub trait ForgeRegistry: Send + Sync {
    /// Classify `git_ref` and resolve it to a commit: tag first, then branch,
    /// then `NotFound`. A ref that is both resolves as the tag — the
    /// documented order (RFC 0019 §4.2), pinned by the resolver's tests.
    ///
    /// A full commit SHA never reaches here; the resolver answers it without
    /// a call.
    async fn resolve_ref(
        &self,
        owner_repo: &str,
        git_ref: &str,
    ) -> Result<ResolvedTarget, CoreError>;

    /// The commit's date and committer, for the coordinates that resolved
    /// through a lookup that did not carry them.
    async fn commit(&self, owner_repo: &str, sha: &str) -> Result<ForgeCommit, CoreError>;
}

/// A remembered resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRefResolution {
    pub kind: RefKind,
    pub sha: String,
    pub resolved_at: DateTime<Utc>,
    pub previous: Option<String>,
}

/// Where resolutions are remembered (`ref_resolutions`, RFC 0019 §6.3).
///
/// Keyed by `(registry, owner_repo, git_ref)`. The table is what makes a
/// moved tag *detectable*: detection compares the forge's answer with the
/// one recorded here, so the first observation is trusted and any later
/// change is a finding.
#[async_trait]
pub trait RefResolutionRepository: Send + Sync {
    async fn get(
        &self,
        registry: &str,
        owner_repo: &str,
        git_ref: &str,
    ) -> Result<Option<StoredRefResolution>, CoreError>;

    /// Record `resolution`, keeping `previous` when the SHA changed.
    async fn upsert(
        &self,
        registry: &str,
        owner_repo: &str,
        git_ref: &str,
        resolution: &StoredRefResolution,
    ) -> Result<(), CoreError>;
}

/// Who is asking for budget (RFC 0019 §5.2).
///
/// The proxy always has priority: it refuses below a 10 % reserve, the
/// worker below 25 %, so under pressure the worker degrades first and the
/// proxy keeps serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetRole {
    Proxy,
    Worker,
}

impl BudgetRole {
    /// The fraction of the limit this role leaves untouched.
    pub fn reserve(&self) -> f64 {
        match self {
            Self::Proxy => 0.10,
            Self::Worker => 0.25,
        }
    }
}

/// What the forge last said about a token's budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitObservation {
    pub remaining: i64,
    pub limit: i64,
    pub reset_at: DateTime<Utc>,
}

/// Shared rate-limit state per `(registry, token fingerprint)`
/// (`rate_limit_budget`, RFC 0019 §5.2).
///
/// In the database rather than in the process because the whole point is
/// that two roles — the proxy and, with RFC 0018, the scan worker — on the
/// same token see one number. A caller that finds no observation yet is
/// allowed through: the first call is what produces one.
#[async_trait]
pub trait RateLimitBudget: Send + Sync {
    /// Whether `role` may make a call on this budget now.
    async fn acquire(
        &self,
        registry: &str,
        token_fingerprint: &str,
        role: BudgetRole,
    ) -> Result<bool, CoreError>;

    /// Record what the forge just reported.
    async fn observe(
        &self,
        registry: &str,
        token_fingerprint: &str,
        observation: RateLimitObservation,
    ) -> Result<(), CoreError>;
}

/// The decision [`RateLimitBudget::acquire`] makes, shared by every
/// implementation so the two stores cannot disagree.
///
/// `None` observation: allowed. Past `reset_at`: allowed, the window has
/// rolled. Otherwise allowed while `remaining` is above the role's reserve
/// of `limit`.
pub fn budget_allows(
    observation: Option<RateLimitObservation>,
    role: BudgetRole,
    now: DateTime<Utc>,
) -> bool {
    let Some(obs) = observation else {
        return true;
    };
    if now >= obs.reset_at {
        return true;
    }
    let reserve = (obs.limit as f64 * role.reserve()).ceil() as i64;
    obs.remaining > reserve
}

/// A stable, non-reversible name for a token, so the budget row can be
/// shared across registries that use the same one without storing it.
pub fn token_fingerprint(secret: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    match secret {
        Some(s) if !s.is_empty() => hex::encode(&Sha256::digest(s.as_bytes())[..16]),
        _ => "anonymous".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(remaining: i64, limit: i64, reset_in_secs: i64) -> RateLimitObservation {
        RateLimitObservation {
            remaining,
            limit,
            reset_at: Utc::now() + chrono::Duration::seconds(reset_in_secs),
        }
    }

    #[test]
    fn nothing_observed_yet_is_allowed() {
        assert!(budget_allows(None, BudgetRole::Proxy, Utc::now()));
        assert!(budget_allows(None, BudgetRole::Worker, Utc::now()));
    }

    #[test]
    fn the_proxy_keeps_ten_percent_and_the_worker_twenty_five() {
        let now = Utc::now();
        // 60/hour anonymous GitHub: proxy reserve 6, worker reserve 15.
        assert!(budget_allows(Some(obs(7, 60, 600)), BudgetRole::Proxy, now));
        assert!(!budget_allows(
            Some(obs(6, 60, 600)),
            BudgetRole::Proxy,
            now
        ));
        assert!(budget_allows(
            Some(obs(16, 60, 600)),
            BudgetRole::Worker,
            now
        ));
        assert!(!budget_allows(
            Some(obs(15, 60, 600)),
            BudgetRole::Worker,
            now
        ));
        // The proxy still goes when the worker is already refused.
        assert!(budget_allows(
            Some(obs(10, 60, 600)),
            BudgetRole::Proxy,
            now
        ));
    }

    #[test]
    fn a_rolled_window_is_allowed_again() {
        let now = Utc::now();
        assert!(budget_allows(Some(obs(0, 60, -1)), BudgetRole::Worker, now));
    }

    #[test]
    fn fingerprints_are_stable_and_never_the_secret() {
        let a = token_fingerprint(Some("ghp_secret"));
        assert_eq!(a, token_fingerprint(Some("ghp_secret")));
        assert_ne!(a, token_fingerprint(Some("ghp_other")));
        assert!(!a.contains("secret"));
        assert_eq!(token_fingerprint(None), "anonymous");
        assert_eq!(token_fingerprint(Some("")), "anonymous");
    }
}
