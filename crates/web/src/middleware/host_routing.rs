//! Host-based registry routing — the inbound half of RFC 0001.
//!
//! There are ~249 route definitions carrying `/proxy/{registry}/…`. None of them
//! change. This middleware sits outermost and rewrites the request URI to the
//! canonical subpath before actix routes it, so everything downstream — route
//! matching, auth, the rate limiter's `extract_registry_from_path`, tracing
//! spans, metrics labels — sees the path it already understands.
//!
//! ```text
//! GET /lodash            Host: npm.acme.io   ⇒  /proxy/npm1/lodash
//! GET /api/v1/crates/new Host: cargo1.hub…   ⇒  /proxy/cargo1/api/v1/crates/new
//! ```
//!
//! **On a registry host, every path is the registry's.** There is no passthrough
//! allowlist: cargo (`/api/v1/…`), GitLab (`/api/v4/…`) and Forgejo
//! (`/api/packages/…`) all legitimately serve paths under `/api`, and a `generic`
//! or `deb` registry can legitimately mirror `/healthz` or `/metrics`. Any
//! reserved prefix would shadow a real registry route, so the admin API, the SPA
//! and the probes live on the main host only.
//!
//! Being outermost also makes this the one place that can compute the
//! proxy-trust verdict once per request (see [`super::proxy_trust`]) — routing
//! reads a header, so it and every downstream consumer of that header must agree
//! about which peers may set it.

use std::future::{ready, Ready};
use std::rc::Rc;

use actix_web::{
    body::EitherBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    http::uri::{PathAndQuery, Uri},
    Error, HttpMessage, HttpRequest, HttpResponse,
};
use futures::future::LocalBoxFuture;

use batlehub_core::services::mise_wire::WireOutcome;

use super::extract_registry_from_path;
use super::proxy_trust::{routing_host, ProxyTrust};
use crate::mise_wire::{MiseWire, WireAnswer, WiredFrom};
use crate::RegistryHostMap;

/// Request extension marking a request that arrived on a registry host, holding
/// the registry it was rewritten to.
///
/// Read by `registry_public_base` so generated URLs are rooted at the host the
/// client actually used, rather than at `…/proxy/{registry}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRoutedRegistry(pub String);

/// The registry this request was host-routed to, if any.
pub fn host_routed_registry(req: &HttpRequest) -> Option<String> {
    req.extensions()
        .get::<HostRoutedRegistry>()
        .map(|r| r.0.clone())
}

// ── Factory ───────────────────────────────────────────────────────────────────

/// Register this **last**, so it is the outermost layer: the URI rewrite has to
/// happen before route matching, and the trust verdict before any middleware
/// that reads a forwarded header.
///
/// ```ignore
/// app.wrap(TracingLogger::default())
///    .wrap(RateLimitMiddlewareFactory::new(…))
///    // …
///    .wrap(HostRoutingMiddlewareFactory::new(registry_host_map, proxy_trust))
/// ```
pub struct HostRoutingMiddlewareFactory {
    map: RegistryHostMap,
    trust: ProxyTrust,
}

impl HostRoutingMiddlewareFactory {
    pub fn new(map: RegistryHostMap, trust: ProxyTrust) -> Self {
        Self { map, trust }
    }
}

impl<S, B> Transform<S, ServiceRequest> for HostRoutingMiddlewareFactory
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = HostRoutingMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(HostRoutingMiddleware {
            service: Rc::new(service),
            map: self.map.clone(),
            trust: self.trust.clone(),
        }))
    }
}

// ── Middleware ────────────────────────────────────────────────────────────────

pub struct HostRoutingMiddleware<S> {
    service: Rc<S>,
    map: RegistryHostMap,
    trust: ProxyTrust,
}

/// Whether `registry` is safe to splice into `/proxy/{registry}/…` as one path
/// segment.
///
/// The name comes from config, not from the request, but it is interpolated into
/// a URI without escaping: a `/` would silently shift the rest of the path down a
/// segment (so `{registry}` would match only the first half), a `?` or `#` would
/// truncate the path into a query or fragment, and a `.`/`..` segment would be
/// collapsed by any normalising hop. Restrict it to what a registry name is
/// allowed to look like everywhere else — see `is_dns_label`, which is stricter
/// still — and let the caller fail the request rather than route a mangled URI.
fn is_routable_registry_name(registry: &str) -> bool {
    if registry.is_empty() || registry == "." || registry == ".." {
        return false;
    }
    registry
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Rewrite `req`'s URI to `/proxy/{registry}{original path and query}`.
///
/// The **raw** `path_and_query` is concatenated and never decoded: npm scoped
/// packages arrive as `/@scope%2fpkg`, and decoding would turn one path segment
/// into two and change what is fetched.
///
/// Returns `false` if `registry` is not a single safe path segment, or if the
/// result is not a valid URI. The caller must turn that into a `400` — never a
/// silent passthrough. On a registry host a passthrough would expose the admin
/// API at a place the operator believes is registry-only.
fn rewrite_uri(req: &mut ServiceRequest, registry: &str) -> bool {
    if !is_routable_registry_name(registry) {
        return false;
    }
    let original = req.uri().path_and_query().map_or("/", PathAndQuery::as_str);
    // `path_and_query` always starts with '/', so "/" becomes "/proxy/{reg}/"
    // (with its trailing slash) and "/x?y=1" becomes "/proxy/{reg}/x?y=1".
    let rewritten = format!("/proxy/{registry}{original}");
    set_path_and_query(req, &rewritten)
}

/// Point `req` at `rewritten` (a path and query), for routing and for
/// handlers alike.
fn set_path_and_query(req: &mut ServiceRequest, rewritten: &str) -> bool {
    let Ok(path_and_query) = rewritten.parse::<PathAndQuery>() else {
        return false;
    };
    let mut parts = req.uri().clone().into_parts();
    parts.path_and_query = Some(path_and_query);
    match Uri::from_parts(parts) {
        Ok(uri) => {
            // Both halves, in this order — exactly what actix's own
            // `NormalizePath` does. `match_info` is parsed from the URI when the
            // request is created and is what the router actually matches on;
            // updating only `head.uri` would rewrite the URI handlers see while
            // still routing on the original path.
            req.match_info_mut().get_mut().update(&uri);
            req.head_mut().uri = uri;
            true
        }
        Err(_) => false,
    }
}

impl<S, B> Service<ServiceRequest> for HostRoutingMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, mut req: ServiceRequest) -> Self::Future {
        // Resolve proxy trust first and store it, so `routing_host` below — and
        // every downstream reader of a forwarded header — share one verdict.
        let verdict = self.trust.verdict_for(req.peer_addr().map(|a| a.ip()));
        req.extensions_mut().insert(verdict);

        let service = self.service.clone();

        // Nothing bound and no mise wire: the feature is off, so skip the host
        // lookup entirely and leave the request byte-identical to what it was
        // before this RFC.
        let wire = self.map.wire();
        if self.map.is_empty() && wire.is_none() {
            return Box::pin(async move { Ok(service.call(req).await?.map_into_left_body()) });
        }

        let host = routing_host(req.request());
        match self.map.registry_for(&host) {
            Some(registry) => {
                if !rewrite_uri(&mut req, &registry) {
                    tracing::warn!(
                        %host,
                        %registry,
                        uri = %req.uri(),
                        "host-routed request could not be rewritten to a valid URI"
                    );
                    let response = HttpResponse::BadRequest()
                        .content_type("application/json")
                        .body(r#"{"error":"Bad Request","message":"malformed request URI"}"#);
                    return Box::pin(async move {
                        Ok(req.into_response(response).map_into_right_body())
                    });
                }
                req.extensions_mut().insert(HostRoutedRegistry(registry));
            }
            None => {
                // RFC 0045: the wire is a main-host ingress, rewritten before
                // the opt-out check below so that check runs on its output.
                let wired = wire.as_deref().and_then(|w| {
                    wire_request(&mut req, w, &host).map(|from| (w.registry.clone(), from))
                });
                if let Some((wire_registry, from_host)) = wired {
                    let host_only = extract_registry_from_path(req.match_info().unprocessed())
                        .is_some_and(|registry| self.map.is_host_only(registry));
                    if host_only {
                        let response = HttpResponse::NotFound()
                            .content_type("application/json")
                            .body(r#"{"error":"Not Found","message":"unknown registry"}"#);
                        return Box::pin(async move {
                            Ok(req.into_response(response).map_into_right_body())
                        });
                    }
                    // §15 Q15: a rewrite whose target the caller cannot read
                    // answers exactly as an unwired host would.
                    return Box::pin(async move {
                        let resp = service.call(req).await?;
                        let status = resp.status().as_u16();
                        if status != 401 && status != 403 {
                            return Ok(resp.map_into_left_body());
                        }
                        let request = resp.request().clone();
                        let answer = crate::handlers::proxy::mise_versions::wire_refusal(
                            &request,
                            &wire_registry,
                            &from_host,
                        )
                        .await;
                        Ok(resp.into_response(answer).map_into_right_body())
                    });
                }
                // Not a registry host — the main host, a bare IP, a probe. The
                // only thing to enforce here is the §4.6 opt-out, because this is
                // the one place that knows the request was *not* host-routed.
                //
                // Read the path from `match_info`, not `req.path()`: the latter is
                // the raw percent-encoded URI, while actix routes on the requoted
                // one, so `/proxy/npm%32/…` would slip past a `npm2` opt-out here
                // and still reach the npm2 handler. Using the router's own view
                // also avoids over-decoding — `%2f` stays encoded in both, so a
                // scoped npm package is still one segment.
                let host_only = extract_registry_from_path(req.match_info().unprocessed())
                    .is_some_and(|registry| self.map.is_host_only(registry));
                if host_only {
                    // 404, not 403: a disabled ingress should look absent,
                    // indistinguishable from an unknown registry.
                    let response = HttpResponse::NotFound()
                        .content_type("application/json")
                        .body(r#"{"error":"Not Found","message":"unknown registry"}"#);
                    return Box::pin(async move {
                        Ok(req.into_response(response).map_into_right_body())
                    });
                }
            }
        }

        Box::pin(async move { Ok(service.call(req).await?.map_into_left_body()) })
    }
}

/// Apply the mise wire to a main-host request under its `_wire/` path.
///
/// Returns the host mise asked for when the request was rewritten to a target
/// route; `None` when it was not under the wire, or was turned into a
/// `_wire-answer` (which needs no post-processing). Never answers itself:
/// every outcome is a rewrite, so it passes the IP block, auth, rate limit and
/// span like any request (§5.1).
fn wire_request(req: &mut ServiceRequest, wire: &MiseWire, request_host: &str) -> Option<String> {
    let prefix = wire.wire_prefix();
    let tail = req.uri().path().strip_prefix(prefix.as_str())?.to_owned();
    let (host, rest) = tail.split_once('/').unwrap_or((tail.as_str(), ""));
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let answer = match wire.table.target_on(request_host, host, rest, wire.air_gap) {
        WireOutcome::Rewrite(path) => {
            if set_path_and_query(req, &format!("{path}{query}")) {
                metrics::counter!("batlehub_mise_wire_requests_total", "outcome" => "rewritten")
                    .increment(1);
                req.extensions_mut().insert(WiredFrom(host.to_owned()));
                return Some(host.to_owned());
            }
            WireAnswer::Malformed
        }
        WireOutcome::Bounce(url) => WireAnswer::Bounce(format!("{url}{query}")),
        WireOutcome::Sink(path) => WireAnswer::Sink(
            path.strip_prefix("/_air-gap/unmirrored/")
                .unwrap_or(&path)
                .to_owned(),
        ),
        WireOutcome::NotWired => WireAnswer::NotWired(host.to_owned()),
        WireOutcome::Malformed => WireAnswer::Malformed,
    };
    metrics::counter!("batlehub_mise_wire_requests_total", "outcome" => answer.outcome())
        .increment(1);
    tracing::debug!(outcome = answer.outcome(), %host, "mise wire");
    // Built from a validated registry name: it always parses.
    if set_path_and_query(req, &wire.answer_path(&answer)) {
        req.extensions_mut().insert(WiredFrom(host.to_owned()));
        req.extensions_mut().insert(answer);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use actix_web::{
        http::StatusCode,
        test::{self, TestRequest},
        web, App, HttpResponse,
    };

    use super::*;
    use crate::middleware::proxy_trust::PeerTrust;

    fn map() -> RegistryHostMap {
        RegistryHostMap::new(
            HashMap::from([
                ("npm.acme.io".to_owned(), "npm1".to_owned()),
                ("npm1.hub.example.com".to_owned(), "npm1".to_owned()),
                ("private.acme.io".to_owned(), "npm2".to_owned()),
            ]),
            HashMap::from([("npm1".to_owned(), "https://npm.acme.io".to_owned())]),
            HashMap::from([("npm2".to_owned(), true)]),
        )
    }

    /// An app that echoes back the path actix ended up routing on, so a test can
    /// assert on the rewrite without needing a real registry handler.
    async fn echo_app(
        map: RegistryHostMap,
        trust: ProxyTrust,
    ) -> impl Service<
        actix_http::Request,
        Response = ServiceResponse<EitherBody<actix_web::body::BoxBody>>,
        Error = Error,
    > {
        test::init_service(
            App::new()
                .wrap(HostRoutingMiddlewareFactory::new(map, trust))
                .default_service(web::to(|req: HttpRequest| async move {
                    let routed = host_routed_registry(&req).unwrap_or_default();
                    HttpResponse::Ok()
                        .insert_header(("x-routed-registry", routed))
                        .body(
                            req.uri()
                                .path_and_query()
                                .map(|pq| pq.as_str().to_owned())
                                .unwrap_or_default(),
                        )
                })),
        )
        .await
    }

    async fn routed_path(uri: &str, host: &str) -> String {
        let app = echo_app(map(), ProxyTrust::legacy_permissive()).await;
        let req = TestRequest::get()
            .uri(uri)
            .insert_header(("host", host))
            .to_request();
        let body = test::call_and_read_body(&app, req).await;
        String::from_utf8(body.to_vec()).expect("utf-8 path")
    }

    // ── Rewrite ───────────────────────────────────────────────────────────────

    #[actix_web::test]
    async fn root_becomes_the_registry_root_with_its_trailing_slash() {
        assert_eq!(routed_path("/", "npm.acme.io").await, "/proxy/npm1/");
    }

    #[actix_web::test]
    async fn nested_paths_are_prefixed() {
        assert_eq!(
            routed_path("/lodash/-/lodash-4.17.21.tgz", "npm.acme.io").await,
            "/proxy/npm1/lodash/-/lodash-4.17.21.tgz"
        );
    }

    #[actix_web::test]
    async fn the_query_string_is_preserved() {
        assert_eq!(
            routed_path("/v3/query?q=serde&take=20", "npm.acme.io").await,
            "/proxy/npm1/v3/query?q=serde&take=20"
        );
        assert_eq!(
            routed_path("/?page=2", "npm.acme.io").await,
            "/proxy/npm1/?page=2"
        );
    }

    #[actix_web::test]
    async fn a_scoped_package_stays_percent_encoded() {
        // Decoding %2f would split one path segment into two and fetch something
        // else entirely.
        assert_eq!(
            routed_path("/@scope%2fpkg", "npm.acme.io").await,
            "/proxy/npm1/@scope%2fpkg"
        );
    }

    #[actix_web::test]
    async fn a_registry_name_that_is_not_one_safe_segment_is_rejected() {
        for bad in [
            "",
            ".",
            "..",
            "npm1/../admin",
            "npm1/x",
            "npm1?q=1",
            "npm1#frag",
            "npm1%2f..",
            "npm 1",
        ] {
            assert!(
                !is_routable_registry_name(bad),
                "{bad:?} must not be spliced into the route"
            );
        }
        for good in ["npm1", "internal-crates", "my_npm", "npm.eu", "a"] {
            assert!(is_routable_registry_name(good), "{good:?} must still route");
        }
    }

    #[actix_web::test]
    async fn a_host_bound_to_an_unroutable_registry_name_returns_400() {
        let map = RegistryHostMap::new(
            HashMap::from([("evil.acme.io".to_owned(), "npm1/../api/v1".to_owned())]),
            HashMap::new(),
            HashMap::new(),
        );
        let app = echo_app(map, ProxyTrust::legacy_permissive()).await;
        let req = TestRequest::get()
            .uri("/admin/config")
            .insert_header(("host", "evil.acme.io"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn the_host_is_matched_case_port_and_dot_insensitively() {
        assert_eq!(
            routed_path("/lodash", "NPM.Acme.io:8443.").await,
            "/proxy/npm1/lodash"
        );
    }

    #[actix_web::test]
    async fn a_wildcard_host_and_a_vanity_host_reach_the_same_registry() {
        assert_eq!(
            routed_path("/lodash", "npm.acme.io").await,
            "/proxy/npm1/lodash"
        );
        assert_eq!(
            routed_path("/lodash", "npm1.hub.example.com").await,
            "/proxy/npm1/lodash"
        );
    }

    #[actix_web::test]
    async fn an_unknown_host_is_left_untouched() {
        assert_eq!(
            routed_path("/api/v1/registries", "hub.example.com").await,
            "/api/v1/registries"
        );
        assert_eq!(
            routed_path("/proxy/npm1/lodash", "hub.example.com").await,
            "/proxy/npm1/lodash"
        );
    }

    #[actix_web::test]
    async fn an_empty_map_never_rewrites() {
        let app = echo_app(RegistryHostMap::default(), ProxyTrust::legacy_permissive()).await;
        let req = TestRequest::get()
            .uri("/lodash")
            .insert_header(("host", "npm.acme.io"))
            .to_request();
        let body = test::call_and_read_body(&app, req).await;
        assert_eq!(body, "/lodash");
    }

    #[actix_web::test]
    async fn the_routed_registry_is_recorded_on_the_request() {
        let app = echo_app(map(), ProxyTrust::legacy_permissive()).await;
        let req = TestRequest::get()
            .uri("/lodash")
            .insert_header(("host", "npm.acme.io"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.headers().get("x-routed-registry").unwrap(), "npm1");

        let req = TestRequest::get()
            .uri("/lodash")
            .insert_header(("host", "hub.example.com"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.headers().get("x-routed-registry").unwrap(), "");
    }

    // ── Proxy trust ───────────────────────────────────────────────────────────

    #[actix_web::test]
    async fn a_spoofed_forwarded_host_from_an_untrusted_peer_does_not_route() {
        let trust = ProxyTrust::from_config(Some(&["10.42.0.0/16".to_owned()]));
        let app = echo_app(map(), trust).await;
        let req = TestRequest::get()
            .uri("/lodash")
            .peer_addr("203.0.113.9:1234".parse().unwrap())
            .insert_header(("host", "hub.example.com"))
            .insert_header(("x-forwarded-host", "npm.acme.io"))
            .to_request();
        let body = test::call_and_read_body(&app, req).await;
        assert_eq!(
            body, "/lodash",
            "an untrusted peer must not pick a registry"
        );
    }

    #[actix_web::test]
    async fn the_same_header_from_a_trusted_peer_routes() {
        let trust = ProxyTrust::from_config(Some(&["10.42.0.0/16".to_owned()]));
        let app = echo_app(map(), trust).await;
        let req = TestRequest::get()
            .uri("/lodash")
            .peer_addr("10.42.7.1:1234".parse().unwrap())
            .insert_header(("host", "hub.example.com"))
            .insert_header(("x-forwarded-host", "npm.acme.io"))
            .to_request();
        let body = test::call_and_read_body(&app, req).await;
        assert_eq!(body, "/proxy/npm1/lodash");
    }

    #[actix_web::test]
    async fn the_verdict_is_stored_for_downstream_middleware() {
        let trust = ProxyTrust::from_config(Some(&["10.42.0.0/16".to_owned()]));
        let app = test::init_service(
            App::new()
                .wrap(HostRoutingMiddlewareFactory::new(map(), trust))
                .default_service(web::to(|req: HttpRequest| async move {
                    let verdict = *req
                        .extensions()
                        .get::<PeerTrust>()
                        .expect("verdict inserted by the middleware");
                    HttpResponse::Ok().body(format!("{verdict:?}"))
                })),
        )
        .await;

        let req = TestRequest::get()
            .uri("/x")
            .peer_addr("10.42.0.1:1234".parse().unwrap())
            .to_request();
        assert_eq!(test::call_and_read_body(&app, req).await, "Trusted");

        let req = TestRequest::get()
            .uri("/x")
            .peer_addr("198.51.100.1:1234".parse().unwrap())
            .to_request();
        assert_eq!(test::call_and_read_body(&app, req).await, "Untrusted");
    }

    // ── path_routing = false (§4.6) ───────────────────────────────────────────

    #[actix_web::test]
    async fn the_subpath_of_a_host_only_registry_returns_404() {
        let app = echo_app(map(), ProxyTrust::legacy_permissive()).await;
        let req = TestRequest::get()
            .uri("/proxy/npm2/lodash")
            .insert_header(("host", "hub.example.com"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_percent_encoded_registry_name_does_not_bypass_the_opt_out() {
        // actix requotes the path before routing, so `npm%32` reaches the npm2
        // handler. Matching on the raw URI here would 200 what must be a 404.
        let app = echo_app(map(), ProxyTrust::legacy_permissive()).await;
        let req = TestRequest::get()
            .uri("/proxy/npm%32/lodash")
            .insert_header(("host", "hub.example.com"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_host_only_registry_still_serves_its_own_host() {
        assert_eq!(
            routed_path("/lodash", "private.acme.io").await,
            "/proxy/npm2/lodash"
        );
    }

    #[actix_web::test]
    async fn a_sibling_registry_with_path_routing_is_unaffected() {
        assert_eq!(
            routed_path("/proxy/npm1/lodash", "hub.example.com").await,
            "/proxy/npm1/lodash"
        );
    }

    #[actix_web::test]
    async fn the_opt_out_does_not_swallow_non_proxy_paths() {
        assert_eq!(
            routed_path("/api/v1/registries", "hub.example.com").await,
            "/api/v1/registries"
        );
    }
}
