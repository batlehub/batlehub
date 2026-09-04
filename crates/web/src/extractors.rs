use std::collections::HashMap;

use actix_web::{dev::Payload, FromRequest, HttpMessage, HttpRequest};
use futures::future::{ready, Ready};

use batlehub_core::entities::Identity;

use crate::error::AppError;

/// Extracts the `Identity` attached by `AuthMiddleware` from request extensions.
///
/// Falls back to `Identity::anonymous()` if no middleware has run (should not
/// happen in production, but avoids panics in tests).
pub struct AuthIdentity(pub Identity);

impl FromRequest for AuthIdentity {
    type Error = AppError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let identity = req
            .extensions()
            .get::<Identity>()
            .cloned()
            .unwrap_or_else(Identity::anonymous);
        ready(Ok(AuthIdentity(identity)))
    }
}

impl std::ops::Deref for AuthIdentity {
    type Target = Identity;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
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
    // scheme; the registry web API says so and cargo 1.98 does so (measured,
    // tests/heavy/cargo.sh). Every `AuthProvider` reads a `Bearer`, so a
    // `cargo publish` arrived anonymous and was refused `releases:publish` by
    // a registry whose admin token it carried. Normalised here, scoped to
    // cargo's API namespace: no other client speaks this way, and a bare
    // value elsewhere stays what it is — a malformed header.
    if req.path().contains("/api/v1/crates") {
        if let Some(bare) = headers
            .get("authorization")
            .filter(|v| !v.trim().is_empty() && !v.contains(' '))
            .cloned()
        {
            headers.insert("authorization".to_owned(), format!("Bearer {bare}"));
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
