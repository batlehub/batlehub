use std::collections::HashMap;

use async_trait::async_trait;

use crate::entities::Identity;
use crate::error::CoreError;

/// Raw auth data extracted from an HTTP request before any provider processes it.
#[derive(Debug, Clone)]
pub struct RawAuthRequest {
    pub headers: HashMap<String, String>,
    pub query_params: HashMap<String, String>,
    /// The caller's address, as the proxy-trust rules resolved it — what a
    /// provider compares against a credential's last-seen source
    /// (RFC 0036 §6.1). `None` where there is no request behind the call.
    pub source_ip: Option<String>,
}

impl RawAuthRequest {
    /// Extracts the token from a `Bearer`/`bearer`-prefixed `Authorization`
    /// header, or `None` if the header is absent or uses a different scheme.
    ///
    /// `extractors::raw_auth_from_request` always normalizes header names to
    /// lowercase before this map is built, but `RawAuthRequest` is also
    /// constructed directly (tests, and any other future caller), so both
    /// header-name casings are checked here rather than only the lowercase one.
    pub fn bearer_token(&self) -> Option<&str> {
        let value = self
            .headers
            .get("authorization")
            .or_else(|| self.headers.get("Authorization"))?;
        value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
    }
}

/// Authenticates an HTTP request and returns the caller's identity.
///
/// Returns `Ok(None)` when this provider does not recognise the credentials
/// (the auth middleware will then try the next provider).
/// Returns `Ok(Some(identity))` on successful authentication.
/// Returns `Err` only on internal provider failures (network, crypto, etc.).
#[async_trait]
pub trait AuthProvider: Send + Sync {
    fn name(&self) -> &str;

    async fn authenticate(&self, req: &RawAuthRequest) -> Result<Option<Identity>, CoreError>;
}
