use std::collections::HashMap;

use actix_web::{dev::Payload, FromRequest, HttpMessage, HttpRequest};
use futures::future::{ready, Ready};

use batlehub_core::entities::Identity;

use crate::error::AppError;

/// Where a request came from, for the audit trail.
///
/// Two ambient facts about the caller that no handler argument carries: the
/// address the proxy-trust verdict says it came from, and the `User-Agent` it
/// sent. Both are read once, by [`caller_net`], so every audit row that records
/// them agrees about what they mean.
///
/// The type itself lives in `core`, because the local read path takes it as an
/// argument all the way down to the event it writes (RFC 0018 §13.10) and a web
/// type it converted from would be a second spelling of the same two fields.
pub use batlehub_core::entities::CallerNet;

/// Read the caller's address and agent off the request.
///
/// The address goes through [`crate::middleware::proxy_trust::client_ip`]
/// rather than `connection_info().realip_remote_addr()`, which believes
/// `X-Forwarded-For` from any peer: that would let a caller write whatever
/// source address it liked into its own audit row. Same verdict as every other
/// IP-consuming path, so they cannot disagree.
pub fn caller_net(req: &HttpRequest) -> CallerNet {
    CallerNet {
        ip: Some(crate::middleware::proxy_trust::client_ip(
            req,
            crate::middleware::proxy_trust::peer_trust(req),
        )),
        user_agent: req
            .headers()
            .get(actix_web::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    }
}

/// Extracts the `Identity` attached by `AuthMiddleware` from request extensions,
/// plus the caller's address and agent for the audit trail.
///
/// Falls back to `Identity::anonymous()` if no middleware has run (should not
/// happen in production, but avoids panics in tests).
#[derive(Debug, Clone)]
pub struct AuthIdentity(pub Identity, pub CallerNet);

impl FromRequest for AuthIdentity {
    type Error = AppError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let identity = req
            .extensions()
            .get::<Identity>()
            .cloned()
            .unwrap_or_else(Identity::anonymous);
        ready(Ok(AuthIdentity(identity, caller_net(req))))
    }
}

impl std::ops::Deref for AuthIdentity {
    type Target = Identity;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The registry a `/proxy/{registry}/…` request names.
fn proxy_registry(path: &str) -> Option<&str> {
    path.strip_prefix("/proxy/")?
        .split('/')
        .next()
        .filter(|segment| !segment.is_empty())
}

/// Whether this request may carry cargo's bare, scheme-less token.
///
/// See the call site for why this is a question about the registry's type
/// rather than about the path.
fn cargo_bare_token_request(req: &HttpRequest) -> bool {
    if let Some(name) = proxy_registry(req.path()) {
        if let Some(map) = req.app_data::<actix_web::web::Data<crate::RegistryMap>>() {
            if map.is_type(name, "cargo") {
                return true;
            }
            // A registry this instance knows, and it is not cargo. No other
            // client sends a scheme-less `Authorization`, so whatever this is,
            // it is not a token to normalise.
            if map.contains(name) {
                return false;
            }
        }
    }
    // No map to ask — a unit-level request. Fall back to what this check was.
    req.path().contains("/api/v1/crates")
}

/// Whether this request is to a `galaxy` registry, whose client sends its
/// credential under the **`Token`** scheme rather than `Bearer`.
///
/// `ansible-galaxy`'s `GalaxyToken.token_type` is the literal string `Token`
/// (Django REST Framework's scheme, which is what galaxy_ng speaks). Only
/// `KeycloakToken` — Automation Hub's OAuth2 path, which this server does not
/// implement (RFC 0031 §3) — uses `Bearer`. So the scheme a *configured*
/// `ansible-galaxy` actually presents is one no `AuthProvider` here reads, and
/// without this every authenticated read and every publish arrives anonymous:
/// a `galaxy` registry closed to anonymous callers would refuse the client that
/// is holding its token.
///
/// Measured against ansible-core 2.19.3 in `tests/heavy/galaxy.sh`, after RFC
/// 0031 §4.2 and §5.1 both recorded it as `Bearer`.
///
/// Scoped by the registry's **type**, exactly as [`cargo_bare_token_request`]
/// is and for the same reason: `Token` is a scheme other clients do not send,
/// and a known registry that is not galaxy must not have its header rewritten.
fn galaxy_token_scheme_request(req: &HttpRequest) -> bool {
    let Some(name) = proxy_registry(req.path()) else {
        return false;
    };
    let Some(map) = req.app_data::<actix_web::web::Data<crate::RegistryMap>>() else {
        // No map to ask — a unit-level request. The path is the only signal,
        // and it is a good one: `/galaxy/api/` is this kind's whole surface.
        return req.path().contains("/galaxy/api/");
    };
    map.is_type(name, "galaxy")
}

/// Builds a `RawAuthRequest` from the actix-web `HttpRequest`.
pub fn raw_auth_from_request(req: &HttpRequest) -> batlehub_core::ports::RawAuthRequest {
    let mut headers = req
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.to_string(), v.to_owned()))
        })
        .collect::<HashMap<_, _>>();

    // NuGet clients send X-NuGet-ApiKey instead of Authorization: Bearer.
    // Normalize so all auth providers see a standard Bearer token.
    if !headers.contains_key("authorization") {
        if let Some(key) = headers.get("x-nuget-apikey").cloned() {
            headers.insert("authorization".to_owned(), format!("Bearer {key}"));
        }
    }

    // cargo sends its registry token *bare* — `Authorization: <token>`, no
    // scheme. The registry web API reference says so in as many words ("Cargo
    // includes the `Authorization` header for requests that require
    // authentication. The header value is the API token.") and cargo 1.98 does
    // so (measured, tests/heavy/cargo.sh). Every `AuthProvider` reads a
    // `Bearer`, so a `cargo publish` arrived anonymous and was refused
    // `releases:publish` by a registry whose admin token it carried.
    //
    // Scoped by the registry's **type**, not by the path. It was the path
    // (`/api/v1/crates`) and that was wrong in both directions:
    //
    //   - Too narrow. With `auth-required: true` in `config.json` cargo sends
    //     the token on the *sparse index* and on *downloads* too, and neither
    //     lives under that prefix — `/proxy/{reg}/registry/…` and
    //     `/proxy/{reg}/{crate}/{version}/download`. The token arrived and was
    //     dropped on the floor, which is the whole reason a cargo read could
    //     not be authenticated at all.
    //   - Too wide. openvsx's `api/{namespace}/{extension}` route is greedy
    //     enough to claim `api/v1/crates` (see the ordering note in `lib.rs`),
    //     so that path exists on an openvsx registry too, where a bare header
    //     is simply malformed and should stay that way.
    //
    // A known registry that is not cargo therefore never takes this path. The
    // old path check survives only as the fallback for a request carrying no
    // registry map, which is what a unit-level `TestRequest` is.
    if cargo_bare_token_request(req) {
        if let Some(bare) = headers
            .get("authorization")
            .filter(|v| !v.trim().is_empty() && !v.contains(' '))
            .cloned()
        {
            headers.insert("authorization".to_owned(), format!("Bearer {bare}"));
        }
    }

    // `ansible-galaxy` sends `Authorization: Token <token>` — the scheme
    // `GalaxyToken.token_type` names, which is Django REST Framework's and not
    // HTTP's. Normalised here beside the NuGet header and cargo's bare token,
    // so every `AuthProvider` still sees one shape (RFC 0031 §13).
    if galaxy_token_scheme_request(req) {
        if let Some(token) = headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Token "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
        {
            headers.insert("authorization".to_owned(), format!("Bearer {token}"));
        }
    }

    // Percent-decoded, with `+` read as a space, per
    // `application/x-www-form-urlencoded` — which is what a query string is.
    //
    // No `AuthProvider` reads query params today; every one of them is
    // header-only. But the field is part of the `RawAuthRequest` contract, and a
    // provider added later would otherwise compare a still-encoded string against
    // a decoded secret and silently never match — the kind of bug that looks like
    // "auth is broken for some tokens and not others".
    let query_params = form_urlencoded::parse(req.query_string().as_bytes())
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<HashMap<_, _>>();

    // `ovsx publish` sends its token as `?token=…` rather than a header
    // (measured against ovsx 1.1.1 — RFC 0009 §12.6). Normalised here, beside
    // the NuGet header above, so every `AuthProvider` still sees one shape.
    //
    // **Scoped to that one route on purpose.** A token in a query string is a
    // token in access logs, proxy logs and shell history, so accepting one
    // everywhere would widen that exposure across the whole API to satisfy a
    // single client. `ovsx` gives no way to send a header instead, so the
    // choice is this or no publish; confining it to the endpoint that requires
    // it is the narrowest form of yes.
    if !headers.contains_key("authorization") && req.path().ends_with("/api/-/publish") {
        if let Some(token) = query_params.get("token") {
            headers.insert("authorization".to_owned(), format!("Bearer {token}"));
        }
    }

    batlehub_core::ports::RawAuthRequest {
        headers,
        query_params,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn a_bare_cargo_token_becomes_a_bearer_header_on_the_crates_api_only() {
        let req = TestRequest::put()
            .uri("/proxy/crates/api/v1/crates/new")
            .insert_header(("Authorization", "tok-123"))
            .to_http_request();
        assert_eq!(
            raw_auth_from_request(&req)
                .headers
                .get("authorization")
                .map(String::as_str),
            Some("Bearer tok-123"),
            "cargo sends its token without a scheme"
        );
        let req = TestRequest::get()
            .uri("/proxy/npm/pkg")
            .insert_header(("Authorization", "tok-123"))
            .to_http_request();
        assert_eq!(
            raw_auth_from_request(&req)
                .headers
                .get("authorization")
                .map(String::as_str),
            Some("tok-123"),
            "elsewhere a bare value is left alone"
        );
    }

    #[test]
    fn ovsx_publish_token_in_the_query_becomes_a_bearer_header() {
        let req = TestRequest::post()
            .uri("/proxy/vsx/api/-/publish?token=abc123")
            .to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(
            raw.headers.get("authorization").map(String::as_str),
            Some("Bearer abc123"),
            "`ovsx publish` sends its token only this way"
        );
    }

    /// The narrow scope is the point: a token in a query string ends up in
    /// logs, so it is accepted for the one endpoint whose client gives no
    /// alternative and nowhere else.
    #[test]
    fn a_query_token_is_ignored_on_every_other_route() {
        for path in [
            "/proxy/vsx/api/-/search?token=abc123",
            "/api/v1/admin/packages?token=abc123",
            "/proxy/npm/express?token=abc123",
        ] {
            let req = TestRequest::get().uri(path).to_http_request();
            let raw = raw_auth_from_request(&req);
            assert!(
                !raw.headers.contains_key("authorization"),
                "{path} must not authenticate from the query string"
            );
        }
    }

    /// An explicit header always wins, so a stale `?token=` in a scripted URL
    /// cannot override the credential the caller actually sent.
    #[test]
    fn an_authorization_header_beats_a_query_token() {
        let req = TestRequest::post()
            .uri("/proxy/vsx/api/-/publish?token=from-query")
            .insert_header(("authorization", "Bearer from-header"))
            .to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(
            raw.headers.get("authorization").map(String::as_str),
            Some("Bearer from-header")
        );
    }

    #[test]
    fn extracts_authorization_header() {
        let req = TestRequest::get()
            .insert_header(("authorization", "Bearer mytoken123"))
            .to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(
            raw.headers.get("authorization").map(String::as_str),
            Some("Bearer mytoken123")
        );
    }

    #[test]
    fn extracts_query_params() {
        let req = TestRequest::get()
            .uri("/?token=abc&foo=bar")
            .to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(
            raw.query_params.get("token").map(String::as_str),
            Some("abc")
        );
        assert_eq!(raw.query_params.get("foo").map(String::as_str), Some("bar"));
    }

    #[test]
    fn no_query_string_yields_empty_params() {
        let req = TestRequest::get().uri("/").to_http_request();
        let raw = raw_auth_from_request(&req);
        assert!(
            raw.query_params.is_empty(),
            "empty query string must produce no params"
        );
    }

    #[test]
    fn trailing_ampersand_does_not_insert_empty_key() {
        let req = TestRequest::get().uri("/?token=abc&").to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(
            raw.query_params.get("token").map(String::as_str),
            Some("abc")
        );
        assert!(
            !raw.query_params.contains_key(""),
            "trailing & must not insert empty key"
        );
    }

    /// A token carrying `+`, `/` or `=` (base64 alphabet, and every padded
    /// base64 secret ends in `=`) has to survive the round trip, or a future
    /// query-param provider would compare an encoded string to a decoded secret.
    #[test]
    fn percent_encoded_values_are_decoded() {
        let req = TestRequest::get()
            .uri("/?token=a%2Bb%2Fc%3D%3D&scope=read%20write")
            .to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(
            raw.query_params.get("token").map(String::as_str),
            Some("a+b/c==")
        );
        assert_eq!(
            raw.query_params.get("scope").map(String::as_str),
            Some("read write")
        );
    }

    /// `+` is a space in a query string, not a literal plus — the distinction
    /// that makes hand-rolled splitting wrong.
    #[test]
    fn plus_is_decoded_as_space() {
        let req = TestRequest::get().uri("/?q=hello+world").to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(
            raw.query_params.get("q").map(String::as_str),
            Some("hello world")
        );
    }

    #[test]
    fn valueless_key_yields_empty_string() {
        let req = TestRequest::get().uri("/?flag").to_http_request();
        let raw = raw_auth_from_request(&req);
        assert_eq!(raw.query_params.get("flag").map(String::as_str), Some(""));
    }
}
