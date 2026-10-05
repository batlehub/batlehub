pub mod auth;
pub mod host_routing;
pub mod ip_block;
pub mod proxy_trust;
pub mod rate_limit;
pub mod security_headers;
pub mod user_block;

/// Extract the registry name from a canonical proxy path like
/// `/proxy/{registry}/...`, or `None` when the path is not one.
///
/// Shared by the rate limiter (which keys its buckets on the registry) and the
/// host-routing middleware (which uses it to enforce the `path_routing = false`
/// opt-out), so the two cannot disagree about what counts as a proxy path.
///
/// **Both callers must pass `req.match_info().unprocessed()`, never
/// `req.path()`.** The latter is the raw percent-encoded URI while actix routes
/// on the requoted one, so `/proxy/npm%32/…` reaches the `npm2` handler while
/// looking like an unknown registry here — which would silently skip both the
/// host-only 404 and the registry's rate limit.
///
/// Host-routed requests have already been rewritten to this shape by the time
/// the rate limiter runs — see [`host_routing`], whose `rewrite_uri` updates
/// `match_info` alongside the URI for exactly this reason.
pub fn extract_registry_from_path(path: &str) -> Option<&str> {
    let mut segments = path.splitn(4, '/');
    segments.next(); // leading ""
    let prefix = segments.next()?; // "proxy"
    if prefix != "proxy" {
        return None;
    }
    segments.next() // registry name
}

pub use auth::{AuthMiddlewareFactory, CredentialRejectionAudit};
pub use host_routing::{HostRoutedRegistry, HostRoutingMiddlewareFactory};
pub use ip_block::IpBlockMiddlewareFactory;
pub use proxy_trust::{PeerTrust, ProxyTrust};
pub use rate_limit::RateLimitMiddlewareFactory;
pub use rate_limit::RateLimitService;
pub use security_headers::{
    protocol_document_csp, security_headers, API_DOCS_CSP, PROTOCOL_DOCUMENT_CSP,
};
pub use user_block::UserBlockMiddlewareFactory;
