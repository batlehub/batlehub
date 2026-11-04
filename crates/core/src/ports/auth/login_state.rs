use async_trait::async_trait;

use crate::error::CoreError;

/// Everything the OIDC callback needs that must not travel through the browser.
///
/// Created by `GET /api/v1/auth/oidc/login`, keyed by the `state` value sent to
/// the identity provider, and consumed exactly once by
/// `GET /api/v1/auth/oidc/callback`.
///
/// **What this does and does not protect.** Holding the state server-side gives
/// four things the previous design had none of: the `code_verifier` never
/// reaches the browser (PKCE is meaningless otherwise), the callback can only be
/// redeemed once, an abandoned login expires, and the provider is decided by the
/// server rather than by a string the caller controls.
///
/// It does **not** bind the flow to a particular browser — the store cannot tell
/// two browsers apart. That binding is the `spa_state` round trip: the caller
/// generates a value, keeps it in `sessionStorage` (browser) or memory (CLI),
/// and compares it against the one echoed back at the end. Both layers are
/// needed, and neither substitutes for the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginState {
    /// Which configured OIDC provider issued the authorization request. Read
    /// from here rather than parsed out of the returned `state`, so a caller
    /// cannot steer a code to a different provider's token endpoint.
    pub provider: String,
    /// PKCE verifier (RFC 7636). Sent to the token endpoint at redemption; must
    /// never be exposed to the browser or to the identity provider before then.
    pub code_verifier: String,
    /// The `nonce` sent in the authorization request, checked against the ID
    /// token's `nonce` claim on redemption.
    pub nonce: String,
    /// The caller's own CSRF value, echoed back to it after redemption so it can
    /// confirm this callback belongs to the login *it* started.
    pub spa_state: String,
    /// Where the callback sends the tokens instead of the console: the CLI's
    /// loopback listener, `http://127.0.0.1:<port>/callback` exactly, checked
    /// before this entry is written (RFC 0040 decision 40).
    pub return_to: Option<String>,
}

/// One-time store for in-flight OIDC authorization requests.
///
/// Implementations must make `take` atomic: two concurrent callbacks carrying
/// the same `state` must not both receive a `LoginState`, or the one-time-use
/// property is lost.
#[async_trait]
pub trait LoginStateStore: Send + Sync {
    /// Record an in-flight login under `state`, expiring after `ttl_secs`.
    async fn put(&self, state: &str, value: LoginState, ttl_secs: u32) -> Result<(), CoreError>;

    /// Consume the entry for `state`, returning it only if it exists and has not
    /// expired. A second call for the same `state` returns `None`.
    async fn take(&self, state: &str) -> Result<Option<LoginState>, CoreError>;

    /// Drop expired entries. Called periodically; never on the request path.
    async fn prune_expired(&self) -> Result<u64, CoreError>;
}
