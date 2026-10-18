//! Baseline security response headers, applied to every response.
//!
//! BatleHub serves three very different things from one origin: the admin SPA
//! (which holds bearer tokens in `localStorage`), the JSON API, and artifact
//! bytes that outsiders control — raw repository files, npm tarballs, `.vsix`
//! bundles. Same-origin is what makes that combination worth hardening: anything
//! the browser can be talked into *rendering* from an artifact URL runs with the
//! SPA's origin, and therefore its storage.
//!
//! Three headers, chosen because they are unconditionally safe for all three
//! surfaces:
//!
//! - `X-Content-Type-Options: nosniff` — the important one. Stops the browser
//!   second-guessing the declared `Content-Type`, which is what turns an artifact
//!   containing HTML into a document. Pairs with `proxy_stream`'s
//!   `application/octet-stream` default; either alone leaves a gap.
//! - `X-Frame-Options: DENY` — no part of this app is meant to be framed, so
//!   clickjacking against the admin UI has no legitimate use case to preserve.
//! - `Referrer-Policy: no-referrer` — package coordinates are in the path, and
//!   registry URLs are often internal. Nothing here should leak to a third party
//!   through a `Referer`.
//!
//! `nosniff` is load-bearing but narrow: it stops a browser second-guessing a
//! declared type, and does **nothing** when a handler legitimately declares
//! `text/html`. The PyPI Simple index does exactly that, from this origin, with
//! publisher-controlled strings in it — so `/proxy/**` gets a CSP of its own,
//! [`protocol_document_csp`]. See its docs for why that scope is the right one
//! and why the objections to a *global* policy do not reach it.
//!
//! **Deliberately not set here:**
//!
//! - `Strict-Transport-Security` — TLS terminates at the ingress; the server
//!   itself usually speaks plaintext HTTP, and emitting HSTS from behind a
//!   proxy is how you pin a browser to `https://` for a host that cannot serve
//!   it. Set it on the ingress instead (the chart's `ingress.annotations`
//!   carries a documented example).
//! - `Content-Security-Policy` — it cannot be global, because the three things
//!   this origin serves need three different policies: `/proxy/**` gets
//!   [`PROTOCOL_DOCUMENT_CSP`], `/scalar` gets [`API_DOCS_CSP`] (same
//!   `script-src 'self'`, but it needs `style-src 'unsafe-inline'` for a
//!   stylesheet the API reference builds at runtime, which the sandboxed proxy
//!   policy must not grant), and the SPA gets its own. It also cannot be
//!   attached to the
//!   static-file service, because `actix_files::Files` is not a
//!   `ServiceFactory` and takes no middleware. The SPA therefore declares its
//!   policy in a
//!   `<meta http-equiv="Content-Security-Policy">` tag, built at build time by
//!   `ui/build/csp.ts` so `connect-src` can track the configured API origin —
//!   and *narrowed to the running config* when the document is served, by
//!   `crate::spa`, which can subtract sources but never add one.
//!   `frame-ancestors` is ignored in meta form, which is precisely why
//!   `X-Frame-Options` is sent here for every response.
//!
//! [`DefaultHeaders`] only inserts a header the response does not already carry,
//! so a handler that sets its own value (the OIDC callback's stricter
//! `Referrer-Policy`, for instance) keeps it.

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::http::header::{HeaderName, HeaderValue, CONTENT_SECURITY_POLICY};
use actix_web::middleware::{DefaultHeaders, Next};
use actix_web::Error;

/// Baseline headers for every response. Register near-outermost so it also
/// covers responses produced by inner middleware (the IP-block `403`, the rate
/// limiter's `429`) and by the static-file service, not just handler output.
pub fn security_headers() -> DefaultHeaders {
    DefaultHeaders::new()
        .add((
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .add((
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .add((
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        ))
}

/// The policy sent on everything under `/proxy/**`.
///
/// `default-src 'none'` allows no subresource of any kind; `sandbox` with no
/// tokens removes script execution, form submission, plugins, and the origin
/// itself — a document served under this cannot read `localStorage`, because it
/// does not have this origin's storage to read.
pub const PROTOCOL_DOCUMENT_CSP: &str = "default-src 'none'; sandbox";

/// The policy sent on `/scalar`, the API reference.
///
/// This route was the standing reason the module gives for having no CSP at all
/// — it loaded its bundle from jsdelivr, so `script-src 'self'` would break it.
/// That argued against a *global* policy and was read as arguing against any,
/// which left the one page executing third-party code as the one page with no
/// policy. The bundle is now served from this origin
/// ([`crate::SCALAR_BUNDLE_PATH`]), so the objection is gone entirely and
/// `script-src` is plain `'self'`.
///
/// No `'unsafe-inline'` for scripts: the spec travels in a
/// `type="application/json"` block, which is data rather than script.
/// `style-src 'unsafe-inline'` is not optional — Scalar builds its stylesheet at
/// runtime — and is the weakest of the directives here, since a style cannot
/// read `localStorage`.
///
/// `connect-src 'self'` started as `*`, on the reasoning that firing requests at
/// arbitrary servers is what an API explorer *is*. Rendering the page in a real
/// browser showed what `*` was actually buying: the bundle calls
/// `api.scalar.com/vector/registry/{curated,search}` on every load, so opening
/// the docs of a private registry announced it to a third party. Nothing is lost
/// by closing it — the generated spec declares no `servers` block, so Scalar
/// targets the current origin, and this page documents the server that serves
/// it. "Test Request" is same-origin here.
///
/// `frame-ancestors 'none'` duplicates the `X-Frame-Options: DENY` that
/// [`security_headers`] sends, and is kept because the header form is the
/// obsolete one.
///
/// Verified in Chrome against a running server: the reference renders (2 204
/// elements, four stylesheets) with an empty console.
pub const API_DOCS_CSP: &str = "default-src 'none'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; \
     font-src 'self' data:; \
     connect-src 'self'; \
     worker-src 'self' blob:; \
     base-uri 'none'; \
     form-action 'none'; \
     frame-ancestors 'none'";

/// [`API_DOCS_CSP`] with `connect-src` opened to `api.scalar.com`, sent only
/// when the operator turned the hosted agent on ([`crate::ScalarAgent`]). The
/// agent's backend lives there and nothing else would reach it — and the
/// bundle's registry lookups, which the strict policy stops, go through too.
/// That is the price of the opt-in, and why it is off by default.
pub const API_DOCS_AI_CSP: &str = "default-src 'none'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob:; \
     font-src 'self' data:; \
     connect-src 'self' https://api.scalar.com; \
     worker-src 'self' blob:; \
     base-uri 'none'; \
     form-action 'none'; \
     frame-ancestors 'none'";

/// A restrictive `Content-Security-Policy` for protocol documents and artifacts.
///
/// # Why a policy here when the module says it cannot be global
///
/// Both reasons the module gives are about *other* routes. `/scalar` loads a CDN
/// bundle and now carries [`API_DOCS_CSP`] for it; `actix_files::Files` takes no
/// middleware. Neither is `/proxy/**`,
/// which is neither the SPA nor a static file — and which is the one category of
/// response that is **both attacker-influenced and rendered as HTML**. The PyPI
/// Simple index is served as `text/html` from the same origin as the console,
/// and `nosniff` does not help: the type is genuinely HTML.
///
/// This does not fix an injection — it caps what one is worth. With it, markup
/// that reaches a protocol document defaces a page nobody styles instead of
/// reading the bearer *and refresh* tokens the SPA keeps in `localStorage`
/// (survey findings 3 and 14).
///
/// # Why the whole prefix, not just `text/html`
///
/// Testing the content type would miss precisely the case that motivates this:
/// a response whose declared type is wrong, or is one of the *other* types a
/// browser renders as a scriptable document — `image/svg+xml`,
/// `application/xhtml+xml`. A package manager ignores CSP entirely, and the
/// header is inert on a download, so applying it to every response under the
/// prefix costs a few bytes and removes the question.
///
/// # Why the path is read on the way in
///
/// `match_info().unprocessed()` is the part of the path the router has not
/// consumed yet — the whole of it here, because this runs before routing.
/// Reading it again on the way out finds an empty remainder and matches nothing.
///
/// Two orderings make that safe. [`super::host_routing`] is wrapped *outside*
/// this one, so a request that arrived as `GET /simple/foo/` on `npm.acme.io`
/// has already been rewritten to `/proxy/{registry}/simple/foo/` — the
/// deployment shape most likely to have a browser pointed at it is covered, not
/// skipped. And routing has not run, so the path is still whole.
///
/// An existing `Content-Security-Policy` is never overwritten, matching
/// [`DefaultHeaders`]' rule: a handler that has thought about its own policy
/// keeps it.
pub async fn protocol_document_csp<B: MessageBody>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<B>, Error> {
    // `match_info().unprocessed()` rather than `path()`: the latter is the raw
    // percent-encoded URI while actix routes on the requoted one, so
    // `/proxy/npm%32/simple/x/` reaches the `npm2` handler while looking like
    // something else here. The same trap `extract_registry_from_path` documents.
    let path = req.match_info().unprocessed();
    let policy = if super::extract_registry_from_path(path).is_some() {
        Some(PROTOCOL_DOCUMENT_CSP)
    } else if is_api_docs_path(path) {
        Some(if req.app_data::<crate::ScalarAgent>().is_some() {
            API_DOCS_AI_CSP
        } else {
            API_DOCS_CSP
        })
    } else {
        None
    };

    let mut res = next.call(req).await?;
    if let Some(policy) = policy {
        if !res.headers().contains_key(CONTENT_SECURITY_POLICY) {
            if let Ok(value) = HeaderValue::from_str(policy) {
                res.headers_mut().insert(CONTENT_SECURITY_POLICY, value);
            }
        }
    }
    Ok(res)
}

/// `/scalar` and anything it serves beneath itself.
///
/// A prefix test, not equality: `Scalar::with_url` mounts a scope, so the exact
/// path may carry a trailing slash. It must not match `/scalarion` or any other
/// route that merely starts with those letters, which is what the boundary
/// check is for.
fn is_api_docs_path(path: &str) -> bool {
    path.strip_prefix("/scalar")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{http::header, test, web, App, HttpResponse};

    #[actix_web::test]
    async fn baseline_headers_are_present_on_a_normal_response() {
        let app = test::init_service(App::new().wrap(security_headers()).route(
            "/",
            web::get().to(|| async { HttpResponse::Ok().body("x") }),
        ))
        .await;

        let resp = test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
        let h = resp.headers();
        assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
        assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
    }

    /// The headers have to reach error responses too — a `403` body is still a
    /// body the browser may be talked into rendering.
    #[actix_web::test]
    async fn baseline_headers_are_present_on_an_error_response() {
        let app = test::init_service(App::new().wrap(security_headers()).route(
            "/",
            web::get().to(|| async { HttpResponse::Forbidden().body("nope") }),
        ))
        .await;

        let resp = test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::FORBIDDEN);
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
    }

    // ── protocol-document CSP ────────────────────────────────────────────────

    /// An app shaped like the real one: a `/proxy/**` route beside a non-proxy
    /// one, both under the CSP middleware.
    macro_rules! csp_app {
        () => {
            test::init_service(
                App::new()
                    .wrap(actix_web::middleware::from_fn(protocol_document_csp))
                    .route(
                        "/proxy/{registry}/simple/{package}/",
                        web::get().to(|| async {
                            HttpResponse::Ok()
                                .content_type("text/html; charset=utf-8")
                                .body("<html><body>Links for x</body></html>")
                        }),
                    )
                    .route(
                        "/proxy/{registry}/{name}/{version}/tarball",
                        web::get().to(|| async {
                            HttpResponse::Ok()
                                .content_type("application/octet-stream")
                                .body("bytes")
                        }),
                    )
                    .route(
                        "/api/v1/packages",
                        web::get().to(|| async { HttpResponse::Ok().json(serde_json::json!([])) }),
                    )
                    .route(
                        "/",
                        web::get().to(|| async {
                            HttpResponse::Ok()
                                .content_type("text/html; charset=utf-8")
                                .body("<html><!-- the console --></html>")
                        }),
                    ),
            )
            .await
        };
    }

    /// The document survey finding 3 injects into: `text/html`, on the console's
    /// own origin, built from publisher-controlled strings.
    #[actix_web::test]
    async fn a_proxy_html_document_carries_the_restrictive_policy() {
        let app = csp_app!();
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/proxy/pypi1/simple/requests/")
                .to_request(),
        )
        .await;
        assert_eq!(
            resp.headers().get("content-security-policy").unwrap(),
            PROTOCOL_DOCUMENT_CSP
        );
    }

    /// The whole prefix, not just the HTML: a mislabelled type is exactly what a
    /// content-type test would miss, and the header is inert on a download.
    #[actix_web::test]
    async fn a_proxy_artifact_carries_it_too() {
        let app = csp_app!();
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/proxy/npm1/chalk/5.3.0/tarball")
                .to_request(),
        )
        .await;
        assert!(resp.headers().contains_key("content-security-policy"));
    }

    /// **The console must not get it.** `default-src 'none'` on the SPA is a
    /// blank page: it declares its own policy in a `<meta>` tag, and the API it
    /// calls is not a document at all. This is the assertion that makes the
    /// scope a scope rather than a global policy by another name.
    #[actix_web::test]
    async fn nothing_outside_the_proxy_prefix_gets_it() {
        let app = csp_app!();
        for uri in ["/", "/api/v1/packages"] {
            let resp =
                test::call_service(&app, test::TestRequest::get().uri(uri).to_request()).await;
            assert!(
                !resp.headers().contains_key("content-security-policy"),
                "{uri} must keep its own policy"
            );
        }
    }

    /// A percent-encoded registry name routes on the requoted path, so reading
    /// `req.path()` here would see something that does not look like a proxy
    /// path and skip the header — the trap `extract_registry_from_path`
    /// documents for the rate limiter and host routing.
    #[actix_web::test]
    async fn a_percent_encoded_registry_name_still_gets_it() {
        let app = csp_app!();
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/proxy/pypi%31/simple/requests/")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::OK);
        assert!(resp.headers().contains_key("content-security-policy"));
    }

    /// A handler that has thought about its own policy keeps it, matching
    /// `DefaultHeaders`' rule for the baseline headers.
    #[actix_web::test]
    async fn a_handler_supplied_policy_is_not_overwritten() {
        let app = test::init_service(
            App::new()
                .wrap(actix_web::middleware::from_fn(protocol_document_csp))
                .route(
                    "/proxy/{registry}/thing",
                    web::get().to(|| async {
                        HttpResponse::Ok()
                            .insert_header((header::CONTENT_SECURITY_POLICY, "default-src 'self'"))
                            .body("x")
                    }),
                ),
        )
        .await;
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/proxy/npm1/thing")
                .to_request(),
        )
        .await;
        assert_eq!(
            resp.headers().get("content-security-policy").unwrap(),
            "default-src 'self'"
        );
    }

    // ── API-docs CSP ─────────────────────────────────────────────────────────

    /// `/scalar` beside a lookalike route and a plain one.
    macro_rules! docs_app {
        () => {
            test::init_service(
                App::new()
                    .wrap(actix_web::middleware::from_fn(protocol_document_csp))
                    .route(
                        "/scalar",
                        web::get().to(|| async { HttpResponse::Ok().body("<html></html>") }),
                    )
                    .route(
                        "/scalarion",
                        web::get().to(|| async { HttpResponse::Ok().body("not the docs") }),
                    )
                    .route(
                        "/api/v1/packages",
                        web::get().to(|| async { HttpResponse::Ok().body("[]") }),
                    ),
            )
            .await
        };
    }

    /// The route that executes a third-party bundle was the one route with no
    /// policy at all.
    #[actix_web::test]
    async fn the_api_reference_carries_its_own_policy() {
        let app = docs_app!();
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/scalar").to_request()).await;
        assert_eq!(
            resp.headers().get("content-security-policy").unwrap(),
            API_DOCS_CSP
        );
    }

    /// The opt-in widens `connect-src` to the agent's backend and nothing else.
    #[actix_web::test]
    async fn the_ai_opt_in_opens_only_connect_src() {
        let app = test::init_service(
            App::new()
                .wrap(actix_web::middleware::from_fn(protocol_document_csp))
                .app_data(crate::ScalarAgent::default())
                .route(
                    "/scalar",
                    web::get().to(|| async { HttpResponse::Ok().finish() }),
                ),
        )
        .await;
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/scalar").to_request()).await;
        assert_eq!(
            resp.headers().get("content-security-policy").unwrap(),
            API_DOCS_AI_CSP
        );
        assert_eq!(
            API_DOCS_AI_CSP.replace(" https://api.scalar.com", ""),
            API_DOCS_CSP
        );
    }

    /// It names the CDN the bundle is pinned to, and does **not** open
    /// `script-src` to inline code — the spec block is JSON, not script.
    #[actix_web::test]
    async fn the_api_docs_policy_confines_script_to_the_pinned_cdn() {
        assert!(API_DOCS_CSP.contains("script-src 'self';"));
        assert!(
            !API_DOCS_CSP.contains("jsdelivr"),
            "the bundle is served from this origin; no CDN may remain"
        );
        assert!(
            !API_DOCS_CSP.contains("script-src 'self' 'unsafe-inline'"),
            "inline script must not be allowed on this origin"
        );
        assert!(API_DOCS_CSP.starts_with("default-src 'none';"));
        // The bundle calls `api.scalar.com` on load and would font-load from
        // `fonts.scalar.com`; both are closed here, and the page was confirmed
        // to render anyway.
        assert!(
            API_DOCS_CSP.contains("connect-src 'self';"),
            "an open connect-src lets the API reference phone home"
        );
        assert!(!API_DOCS_CSP.contains("scalar.com"));
    }

    /// A prefix test that matched `/scalarion` would hand an unrelated route the
    /// permissive docs policy instead of nothing.
    #[actix_web::test]
    async fn a_route_merely_starting_with_scalar_is_not_the_api_reference() {
        let app = docs_app!();
        let resp = test::call_service(
            &app,
            test::TestRequest::get().uri("/scalarion").to_request(),
        )
        .await;
        assert!(resp.headers().get("content-security-policy").is_none());
    }

    /// The API keeps getting no CSP from this middleware; only the two prefixes
    /// are in scope.
    #[actix_web::test]
    async fn an_ordinary_api_route_gets_no_policy_from_this_middleware() {
        let app = docs_app!();
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/packages")
                .to_request(),
        )
        .await;
        assert!(resp.headers().get("content-security-policy").is_none());
    }

    /// The two scopes must not bleed into each other: a proxy path keeps the
    /// sandbox, which the docs policy would undo.
    #[actix_web::test]
    async fn the_two_scopes_do_not_overlap() {
        assert_ne!(PROTOCOL_DOCUMENT_CSP, API_DOCS_CSP);
        assert!(!is_api_docs_path("/proxy/npm1/simple/x/"));
        assert!(is_api_docs_path("/scalar"));
        assert!(is_api_docs_path("/scalar/"));
        assert!(!is_api_docs_path("/scalarion"));
    }

    /// `DefaultHeaders` must not clobber a handler that set a stricter value of
    /// its own — the OIDC callback relies on this.
    #[actix_web::test]
    async fn handler_supplied_value_wins() {
        let app = test::init_service(App::new().wrap(security_headers()).route(
            "/",
            web::get().to(|| async {
                HttpResponse::Ok()
                    .insert_header((header::REFERRER_POLICY, "same-origin"))
                    .body("x")
            }),
        ))
        .await;

        let resp = test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
        assert_eq!(
            resp.headers().get("referrer-policy").unwrap(),
            "same-origin"
        );
    }
}
