//! The browser sign-in flow: `GET /api/v1/auth/oidc/login` and
//! `GET /api/v1/auth/oidc/callback`.
//!
//! These are regression tests for a callback that used to redeem any code it was
//! handed. The `state` it received was split on `:` to pick a provider, echoed
//! back to the SPA, and otherwise never checked — the handler even carried a
//! comment claiming CSRF was "prevented by the `state` parameter validated
//! above", which nothing did. There was no PKCE, and the resulting tokens went
//! back in the query string.
//!
//! What the server can and cannot enforce is worth stating, because it decides
//! what belongs here: it can prove a state is one it issued, has not expired and
//! has not been redeemed before, and it can pin the provider. It cannot tell
//! that the login started in the caller's browser — that is the `oidc_state`
//! round trip, tested in `ui/src/router/index.test.ts`.

mod common;

use std::sync::{Arc, Mutex};

use actix_web::test::{call_service, TestRequest};
use batlehub_adapters::auth::{OidcSsoFlow, OidcSsoFlowParams};
use batlehub_adapters::in_memory::InMemoryLoginStateStore;
use batlehub_core::ports::{LoginState, LoginStateStore};
use common::*;

const FRONTEND: &str = "https://app.example.test";

/// A [`LoginStateStore`] that remembers what the login leg wrote.
///
/// The nonce and PKCE verifier are generated inside the handler and never leave
/// the server, which is the point — but a test that wants to mint an ID token
/// bound to the right nonce has to learn it somehow, and `take` consumes the
/// entry. Recording on the way in leaves the one-time-use behaviour under test
/// exactly as it ships.
struct RecordingLoginStateStore {
    inner: InMemoryLoginStateStore,
    written: Mutex<Vec<(String, LoginState)>>,
}

impl RecordingLoginStateStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: InMemoryLoginStateStore::new(),
            written: Mutex::new(Vec::new()),
        })
    }

    /// The `LoginState` recorded for `state`, still redeemable.
    fn recorded(&self, state: &str) -> LoginState {
        self.written
            .lock()
            .unwrap()
            .iter()
            .find(|(s, _)| s == state)
            .map(|(_, v)| v.clone())
            .expect("the login leg recorded this state")
    }
}

#[async_trait::async_trait]
impl LoginStateStore for RecordingLoginStateStore {
    async fn put(
        &self,
        state: &str,
        value: LoginState,
        ttl_secs: u32,
    ) -> Result<(), batlehub_core::error::CoreError> {
        self.written
            .lock()
            .unwrap()
            .push((state.to_owned(), value.clone()));
        self.inner.put(state, value, ttl_secs).await
    }

    async fn take(
        &self,
        state: &str,
    ) -> Result<Option<LoginState>, batlehub_core::error::CoreError> {
        self.inner.take(state).await
    }

    async fn prune_expired(&self) -> Result<u64, batlehub_core::error::CoreError> {
        self.inner.prune_expired().await
    }
}

/// An app whose single OIDC provider points its token endpoint at `idp_url`.
///
/// Returns the login-state store as well, so a test can assert what the login
/// leg wrote and what the callback consumed.
async fn make_sso_app(
    idp_url: &str,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    Arc<RecordingLoginStateStore>,
) {
    let login_states = RecordingLoginStateStore::new();
    let flow = OidcSsoFlow::new(OidcSsoFlowParams {
        name: "authentik".to_owned(),
        client_id: "batlehub".to_owned(),
        client_secret: Some("shh".to_owned()),
        redirect_uri: "https://api.example.test/api/v1/auth/oidc/callback".to_owned(),
        scopes: vec!["openid".to_owned()],
        authorization_endpoint: format!("{idp_url}/authorize"),
        token_endpoint: format!("{idp_url}/token"),
        frontend_url: FRONTEND.to_owned(),
    });

    let parts = local_registry_app_parts(
        "npm",
        "npm",
        batlehub_config::schema::RegistryMode::Proxy,
        None,
    );
    let app = build_local_registry_app_with_defaults(
        parts,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults {
            sso_flows: vec![flow],
            login_states: Arc::clone(&login_states) as Arc<dyn LoginStateStore>,
            ..Default::default()
        },
    )
    .await;
    (app, login_states)
}

fn location(resp: &actix_web::dev::ServiceResponse<actix_web::body::BoxBody>) -> String {
    resp.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Pull `key=value` out of an authorization URL's query string.
fn param(url: &str, key: &str) -> Option<String> {
    let (_, qs) = url.split_once('?')?;
    qs.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_owned())
    })
}

/// Drive the login leg and return `(state sent to the IdP, the whole URL)`.
async fn start_login<S>(app: &S, spa_state: &str) -> (String, String)
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
{
    let req = TestRequest::get()
        .uri(&format!("/api/v1/auth/oidc/login?state={spa_state}"))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 302, "login must redirect to the IdP");
    let url = location(&resp);
    let state = param(&url, "state").expect("authorization URL carries a state");
    (state, url)
}

// ── The login leg ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn login_sends_pkce_and_a_server_generated_state() {
    let server = mockito::Server::new_async().await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let (state, url) = start_login(&app, "spa-csrf-value").await;

    assert!(url.starts_with(&format!("{}/authorize?", server.url())));
    assert_eq!(
        param(&url, "code_challenge_method").as_deref(),
        Some("S256"),
        "PKCE must be S256, never plain"
    );
    assert!(param(&url, "code_challenge").is_some());
    assert!(param(&url, "nonce").is_some());

    // The state going to the identity provider is the server's own handle. The
    // caller's value must not be in the URL at all: the previous design sent
    // "<provider>:<caller value>", which put both the provider decision and the
    // caller's CSRF value on the wire.
    assert_ne!(state, "spa-csrf-value");
    assert!(!url.contains("spa-csrf-value"));
    assert!(!url.contains("authentik:"));
}

#[actix_web::test]
async fn login_without_state_is_a_probe_not_a_redirect() {
    let server = mockito::Server::new_async().await;
    let (app, _states) = make_sso_app(&server.url()).await;
    let req = TestRequest::get()
        .uri("/api/v1/auth/oidc/login")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        200,
        "the frontend probes this to show a button"
    );
}

#[actix_web::test]
async fn two_logins_get_different_states() {
    let server = mockito::Server::new_async().await;
    let (app, _states) = make_sso_app(&server.url()).await;
    let (first, _) = start_login(&app, "a").await;
    let (second, _) = start_login(&app, "b").await;
    assert_ne!(first, second);
}

// ── The callback leg ──────────────────────────────────────────────────────────

/// A JWT-shaped ID token carrying `nonce`, which the callback checks against the
/// value it sent with the authorization request (OIDC Core §3.1.3.7 step 11).
///
/// Unsigned: nothing on this path verifies the signature, and the comment on
/// `verify_nonce` explains why that is correct there and nowhere else.
fn id_token_with_nonce(nonce: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let claims = serde_json::json!({ "sub": "alice", "nonce": nonce });
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
        URL_SAFE_NO_PAD.encode("sig"),
    )
}

/// Mock the IdP's token endpoint, requiring the PKCE verifier to be present and
/// returning an ID token bound to `nonce`.
async fn mock_token_endpoint(server: &mut mockito::ServerGuard, nonce: &str) -> mockito::Mock {
    server
        .mock("POST", "/token")
        .match_body(mockito::Matcher::Regex("code_verifier=".to_owned()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"access_token":"opaque-at","id_token":"{}","refresh_token":"rt-1","expires_in":3600}}"#,
            id_token_with_nonce(nonce)
        ))
        .create_async()
        .await
}

#[actix_web::test]
async fn a_full_round_trip_returns_the_tokens_in_the_fragment() {
    let mut server = mockito::Server::new_async().await;
    let (app, states) = make_sso_app(&server.url()).await;

    let (state, _) = start_login(&app, "spa-csrf-value").await;
    let token = mock_token_endpoint(&mut server, &states.recorded(&state).nonce).await;
    let req = TestRequest::get()
        .uri(&format!(
            "/api/v1/auth/oidc/callback?code=abc&state={state}"
        ))
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 302);
    let loc = location(&resp);
    token.assert_async().await;

    // The fragment is the point: a query string reaches the SPA's own web server
    // and lands in its access log, a fragment never leaves the browser.
    let (base, fragment) = loc.split_once('#').expect("tokens ride in the fragment");
    assert_eq!(base, format!("{FRONTEND}/"));
    // The ID token, not the opaque access token the IdP also returned: the ID
    // token is the assertion about who the user is, and it is a JWT by
    // specification. `oidc_access_token` keeps its name because that is what
    // every client reads.
    assert!(!fragment.contains("opaque-at"));
    let session = fragment
        .split('&')
        .find_map(|p| p.strip_prefix("oidc_access_token="))
        .expect("a session token");
    assert_eq!(
        session.matches("%2E").count() + session.matches('.').count(),
        2
    );
    assert!(fragment.contains("oidc_refresh_token=rt-1"));
    assert!(fragment.contains("oidc_expires_in=3600"));
    assert!(
        !base.contains('?'),
        "nothing may be left in the query string"
    );

    // The caller's own value comes back so it can match this callback to the
    // login it started — the browser binding the server cannot do itself.
    assert!(fragment.contains("oidc_state=spa-csrf-value"));
    assert!(fragment.contains("oidc_provider=authentik"));

    assert_eq!(
        resp.headers().get("referrer-policy").unwrap(),
        "no-referrer"
    );
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-store");
}

#[actix_web::test]
async fn a_state_the_server_never_issued_redeems_nothing() {
    let mut server = mockito::Server::new_async().await;
    // `expect(0)`: reaching the token endpoint at all is the failure.
    let token = server
        .mock("POST", "/token")
        .expect(0)
        .with_status(200)
        .with_body(r#"{"access_token":"at-1"}"#)
        .create_async()
        .await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let req = TestRequest::get()
        .uri("/api/v1/auth/oidc/callback?code=attacker-code&state=forged")
        .to_request();
    let resp = call_service(&app, req).await;

    assert_eq!(resp.status(), 302);
    let loc = location(&resp);
    assert!(
        loc.contains("oidc_error="),
        "must redirect to the error page"
    );
    assert!(!loc.contains("oidc_access_token"));
    token.assert_async().await;
}

#[actix_web::test]
async fn a_callback_with_no_state_redeems_nothing() {
    let mut server = mockito::Server::new_async().await;
    let token = server
        .mock("POST", "/token")
        .expect(0)
        .with_status(200)
        .with_body(r#"{"access_token":"at-1"}"#)
        .create_async()
        .await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let req = TestRequest::get()
        .uri("/api/v1/auth/oidc/callback?code=attacker-code")
        .to_request();
    let resp = call_service(&app, req).await;
    assert!(!location(&resp).contains("oidc_access_token"));
    token.assert_async().await;
}

#[actix_web::test]
async fn a_state_cannot_be_redeemed_twice() {
    let mut server = mockito::Server::new_async().await;
    let (app, states) = make_sso_app(&server.url()).await;

    let (state, _) = start_login(&app, "spa").await;
    // `expect(1)`: the second callback must not reach the token endpoint at all.
    let token = mock_token_endpoint(&mut server, &states.recorded(&state).nonce)
        .await
        .expect(1);
    let uri = format!("/api/v1/auth/oidc/callback?code=abc&state={state}");

    let first = call_service(&app, TestRequest::get().uri(&uri).to_request()).await;
    assert!(location(&first).contains("oidc_access_token="));

    let second = call_service(&app, TestRequest::get().uri(&uri).to_request()).await;
    assert!(
        !location(&second).contains("oidc_access_token"),
        "a replayed callback must not mint a second session"
    );
    assert!(location(&second).contains("oidc_error="));
    token.assert_async().await;
}

#[actix_web::test]
async fn the_login_leg_stores_state_that_the_callback_consumes() {
    let server = mockito::Server::new_async().await;
    let (app, states) = make_sso_app(&server.url()).await;

    let (state, url) = start_login(&app, "spa").await;

    // The verifier never appears in the URL handed to the browser; it is held
    // server-side until redemption, which is the entire point of PKCE.
    let stored = states.take(&state).await.unwrap().expect("login recorded");
    assert_eq!(stored.provider, "authentik");
    assert_eq!(stored.spa_state, "spa");
    assert!(!url.contains(&stored.code_verifier));
    assert_eq!(param(&url, "nonce").as_deref(), Some(stored.nonce.as_str()));
}

#[actix_web::test]
async fn an_expired_state_redeems_nothing() {
    let mut server = mockito::Server::new_async().await;
    let token = server
        .mock("POST", "/token")
        .expect(0)
        .with_status(200)
        .with_body(r#"{"access_token":"at-1"}"#)
        .create_async()
        .await;
    let (app, states) = make_sso_app(&server.url()).await;

    // A login recorded with a zero TTL is already past its expiry.
    states
        .put(
            "stale-state",
            batlehub_core::ports::LoginState {
                provider: "authentik".to_owned(),
                code_verifier: "v".to_owned(),
                nonce: "n".to_owned(),
                spa_state: "spa".to_owned(),
            },
            0,
        )
        .await
        .unwrap();

    let req = TestRequest::get()
        .uri("/api/v1/auth/oidc/callback?code=abc&state=stale-state")
        .to_request();
    let resp = call_service(&app, req).await;
    assert!(!location(&resp).contains("oidc_access_token"));
    token.assert_async().await;
}

#[actix_web::test]
async fn a_provider_side_error_is_forwarded_without_touching_the_token_endpoint() {
    let mut server = mockito::Server::new_async().await;
    let token = server
        .mock("POST", "/token")
        .expect(0)
        .with_status(200)
        .with_body("{}")
        .create_async()
        .await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let req = TestRequest::get()
        .uri("/api/v1/auth/oidc/callback?error=access_denied&error_description=User%20said%20no")
        .to_request();
    let resp = call_service(&app, req).await;
    assert!(location(&resp).contains("oidc_error="));
    token.assert_async().await;
}

// ── Nonce and opaque tokens ───────────────────────────────────────────────────

#[actix_web::test]
async fn an_id_token_bound_to_another_nonce_is_refused() {
    // The replay the nonce exists to stop: an ID token captured from one
    // authorization request, returned against another. The code exchange
    // succeeds — the identity provider is not the attacker here — and the
    // callback still refuses to mint a session.
    let mut server = mockito::Server::new_async().await;
    let (app, states) = make_sso_app(&server.url()).await;

    let (state, _) = start_login(&app, "spa").await;
    // Derived from the nonce this login actually recorded, so the two are
    // guaranteed to differ. A literal would only differ by assumption — and it
    // reads to a scanner as a hard-coded cryptographic value, which it is not:
    // the point of the fixture is to be the *wrong* nonce.
    let other_nonce = format!("{}-from-some-other-login", states.recorded(&state).nonce);
    let _token = mock_token_endpoint(&mut server, &other_nonce).await;

    let req = TestRequest::get()
        .uri(&format!(
            "/api/v1/auth/oidc/callback?code=abc&state={state}"
        ))
        .to_request();
    let loc = location(&call_service(&app, req).await);

    assert!(!loc.contains("oidc_access_token"), "no session was minted");
    assert!(loc.contains("oidc_error="));
}

#[actix_web::test]
async fn an_id_token_with_no_nonce_is_refused() {
    let mut server = mockito::Server::new_async().await;
    let _token = server
        .mock("POST", "/token")
        .with_status(200)
        .with_header("content-type", "application/json")
        // A JWT, correctly shaped, simply carrying no `nonce`.
        .with_body(r#"{"access_token":"h.p.s","id_token":"h.p.s"}"#)
        .create_async()
        .await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let (state, _) = start_login(&app, "spa").await;
    let req = TestRequest::get()
        .uri(&format!(
            "/api/v1/auth/oidc/callback?code=abc&state={state}"
        ))
        .to_request();
    assert!(!location(&call_service(&app, req).await).contains("oidc_access_token"));
}

#[actix_web::test]
async fn an_opaque_access_token_fails_the_login_rather_than_the_next_request() {
    // Okta and Auth0 issue opaque access tokens by default. Handing one back as
    // the session credential used to "work": every later request failed JWT
    // validation, fell through to anonymous, and the operator went looking at
    // their RBAC rules. Fail here instead, where the cause is visible.
    let mut server = mockito::Server::new_async().await;
    let _token = server
        .mock("POST", "/token")
        .with_status(200)
        .with_header("content-type", "application/json")
        // Named, not randomised. What the assertion needs is a token with no
        // dots in it — `session_token_is_a_jwt` wants three non-empty
        // dot-separated parts — and a realistic-looking 40-character opaque
        // string bought nothing except a `generic-api-key` hit from the secret
        // scanner, which cannot tell a fixture from an Okta token and should not
        // have to.
        .with_body(r#"{"access_token":"opaque-access-token-not-a-jwt"}"#)
        .create_async()
        .await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let (state, _) = start_login(&app, "spa").await;
    let req = TestRequest::get()
        .uri(&format!(
            "/api/v1/auth/oidc/callback?code=abc&state={state}"
        ))
        .to_request();
    let loc = location(&call_service(&app, req).await);

    assert!(!loc.contains("oidc_access_token"));
    assert!(loc.contains("oidc_error="));
}

#[actix_web::test]
async fn a_jwt_access_token_with_no_id_token_still_signs_in() {
    // The configuration the "opaque access token" error message tells operators
    // to reach for: no `id_token`, but an API audience that yields a JWT access
    // token. `token_request` falls back to it and `OidcAuthProvider` validates
    // it like any other JWT, so this has always worked.
    //
    // Running the nonce check unconditionally broke it: §3.1.3.7 step 11 is a
    // rule about *ID tokens*, an access token carries no `nonce` claim, and
    // demanding one rejected every such deployment with a generic "try again".
    // What ties this exchange to this login is the one-time `state` and the PKCE
    // verifier, both already checked above.
    let mut server = mockito::Server::new_async().await;
    let _token = server
        .mock("POST", "/token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"access_token":"jwt.access.token"}"#)
        .create_async()
        .await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let (state, _) = start_login(&app, "spa").await;
    let req = TestRequest::get()
        .uri(&format!(
            "/api/v1/auth/oidc/callback?code=abc&state={state}"
        ))
        .to_request();
    let loc = location(&call_service(&app, req).await);

    assert!(
        loc.contains("oidc_access_token=jwt.access.token"),
        "a JWT access token is a usable session credential: {loc}"
    );
    assert!(!loc.contains("oidc_error="), "{loc}");
}

// ── Refresh throttling ────────────────────────────────────────────────────────

#[actix_web::test]
async fn the_refresh_endpoint_is_throttled_per_client() {
    // Unauthenticated by necessity — a client whose access token expired has
    // nothing else to present — and it relays to the identity provider with this
    // deployment's client_secret attached. Unthrottled it is both a validity
    // oracle for refresh tokens and a way to aim traffic at the IdP.
    let mut server = mockito::Server::new_async().await;
    let _token = server
        .mock("POST", "/token")
        .expect_at_most(30)
        .with_status(400)
        .with_body("{}")
        .create_async()
        .await;
    let (app, _states) = make_sso_app(&server.url()).await;

    let mut statuses = Vec::new();
    for _ in 0..40 {
        let req = TestRequest::post()
            .uri("/api/v1/auth/oidc/refresh")
            .set_json(serde_json::json!({ "refresh_token": "guess" }))
            .to_request();
        statuses.push(call_service(&app, req).await.status().as_u16());
    }

    assert!(
        statuses.contains(&429),
        "a burst must eventually be refused, got: {statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s != 429).count(),
        30,
        "exactly the ceiling gets through before the window closes"
    );
}

/// An oversized `state` is refused before anything is stored.
///
/// `GET /api/v1/auth/oidc/login` is unauthenticated by necessity and nothing
/// throttles it — `RateLimitMiddleware` buckets `/proxy/{registry}/…` only, and
/// the refresh limiter guards the refresh endpoint alone. It writes a row that
/// outlives the request by the login TTL, with the caller's `state` stored
/// verbatim, so without a cap one client can fill the store with rows as large
/// as the request line allows — thousands of bytes each rather than the ~40 a
/// real CSRF nonce costs — faster than the prune can reclaim them.
///
/// Refused rather than truncated: storing a different `state` than the caller
/// kept would fail its own round-trip check with nothing to explain it.
#[actix_web::test]
async fn an_oversized_state_is_refused_and_stores_nothing() {
    let server = mockito::Server::new_async().await;
    let (app, states) = make_sso_app(&server.url()).await;

    let huge = "x".repeat(4_096);
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!("/api/v1/auth/oidc/login?state={huge}"))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400, "an oversized state is a bad request");

    // And a state of a size a real caller sends still starts a login.
    let (state, _) = start_login(&app, "spa-csrf-value").await;
    assert!(
        states.take(&state).await.unwrap().is_some(),
        "an ordinary login must still be stored"
    );
}

// ── The provider list ─────────────────────────────────────────────────────────
//
// The endpoint the login page asks before it draws anything: it decides whether
// there is an SSO button at all, and how many. It is deliberately unauthenticated
// — the caller has no token yet, which is the whole point of asking.

#[actix_web::test]
async fn provider_list_names_every_configured_sso_flow() {
    let mut idp = mockito::Server::new_async().await;
    let (app, _states) = make_sso_app(&idp.url()).await;

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/auth/oidc/providers")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([{ "name": "authentik" }]));

    // No call to the identity provider: the list is read off the configuration,
    // so a provider that is down still gets its button.
    idp.reset();
}

#[actix_web::test]
async fn provider_list_is_empty_when_no_sso_is_configured() {
    // An empty array rather than a 404 or a 503: the SPA branches on the length,
    // and an error status would show it a failure where the answer is "none".
    let app = make_app(batlehub_adapters::in_memory::InMemoryPackageRepository::new()).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/auth/oidc/providers")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    assert_eq!(body, serde_json::json!([]));
}
