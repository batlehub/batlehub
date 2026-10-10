use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use actix_web::{get, post, web, HttpRequest, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use batlehub_adapters::auth::{random_urlsafe, OidcSsoFlow, PkceChallenge};
use batlehub_core::entities::{AccessAction, AccessEvent, AccessResult, CallerNet, Role};
use batlehub_core::ports::{LoginState, LoginStateStore};
use batlehub_core::services::AdminService;

use super::{spa_error_redirect, url_encode, CallbackQuery, LoginQuery};
use crate::error::AppError;
use crate::extractors::caller_net;
use crate::handlers::schemas::OkResponse;

/// How long a started login stays redeemable.
///
/// Long enough for a human to authenticate, re-enter a password manager and
/// clear an MFA prompt; short enough that an abandoned login is not sitting in
/// the table an hour later. The identity provider's own code lifetime is
/// typically shorter still (60 s for many), so this is the outer bound, not the
/// operative one.
const LOGIN_STATE_TTL_SECS: u32 = 600;

/// Longest caller-supplied `state` a started login will carry back.
///
/// `GET /api/v1/auth/oidc/login` is unauthenticated by necessity — it is the
/// first request of a sign-in — and it writes a row that lives for
/// [`LOGIN_STATE_TTL_SECS`] with the caller's `state` stored verbatim. Nothing
/// throttles the route: `RateLimitMiddleware` only buckets `/proxy/{registry}/…`
/// and [`RefreshRateLimiter`] guards the refresh endpoint alone. Without a cap,
/// one unauthenticated client can write megabyte rows as fast as it can issue
/// requests and fill the table (and the in-memory store) with entries the prune
/// task cannot reclaim before their TTL.
///
/// The value is a CSRF nonce: the console sends a UUID and the CLI a UUID, so
/// this is two orders of magnitude more than any real caller needs, and a
/// request over it is refused rather than truncated — silently storing a
/// different `state` than the caller kept would fail its own round-trip check
/// with no explanation.
const MAX_SPA_STATE_LEN: usize = 512;

/// Refresh attempts allowed per client IP per window, and the window.
///
/// `POST /auth/oidc/refresh` is unauthenticated by necessity — a client whose
/// access token has expired has nothing else to present — and it relays to the
/// identity provider with this deployment's `client_secret` attached. That makes
/// it both an oracle for refresh-token validity and a way to aim traffic at the
/// IdP from behind this server's address. Legitimate use is roughly one call per
/// token lifetime, so a generous ceiling still cuts abuse by orders of
/// magnitude.
const REFRESH_MAX_PER_WINDOW: u32 = 30;
const REFRESH_WINDOW: Duration = Duration::from_secs(60);

/// Per-IP fixed-window counter for the refresh endpoint.
///
/// Process-local on purpose. A shared store would bound the total across
/// replicas more tightly, but it would also put a network dependency in front of
/// the one endpoint a client hits when its session is already failing — an
/// outage there would lock everyone out rather than merely fail to throttle.
/// Each replica capping its own share bounds the amplification, which is what
/// this is for.
#[derive(Default)]
pub struct RefreshRateLimiter {
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RefreshRateLimiter {
    /// Count one attempt from `client`; `false` when it is over the ceiling.
    fn allow(&self, client: &str) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("refresh limiter mutex");

        // Drop windows that have rolled over, so a burst of distinct source
        // addresses cannot grow the map without bound.
        windows.retain(|_, (started, _)| now.duration_since(*started) < REFRESH_WINDOW);

        let entry = windows.entry(client.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= REFRESH_WINDOW {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= REFRESH_MAX_PER_WINDOW
    }
}

// ── Provider list ──────────────────────────────────────────────────────────────

#[derive(Serialize, ToSchema)]
pub struct OidcProviderInfo {
    /// Configured provider name (e.g. `"oidc"`, `"oidc2"`).
    pub name: String,
}

/// List OIDC providers that have SSO (browser login) enabled.
///
/// Returns an empty array when no OIDC provider has `redirect_uri` configured.
/// Use this endpoint instead of probing `/api/v1/auth/oidc/login` to decide
/// whether and how many OIDC login buttons to show.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oidc/providers",
    tag = "front-office",
    responses(
        (status = 200, description = "OIDC providers with browser SSO configured", body = Vec<OidcProviderInfo>),
    ),
)]
#[get("/api/v1/auth/oidc/providers")]
pub async fn list_oidc_providers(flows: web::Data<Vec<OidcSsoFlow>>) -> impl Responder {
    let providers: Vec<OidcProviderInfo> = flows
        .iter()
        .map(|sso| OidcProviderInfo {
            name: sso.name.clone(),
        })
        .collect();
    HttpResponse::Ok().json(providers)
}

// ── Login ──────────────────────────────────────────────────────────────────────

/// Redirect the browser to the OIDC provider's authorization endpoint.
///
/// The caller supplies a `state` query parameter holding a random value it has
/// kept locally (`sessionStorage` in the SPA, memory in the CLI). That value is
/// **not** what goes to the identity provider: the server generates its own
/// unguessable handle, records the caller's value against it along with the PKCE
/// verifier and nonce, and sends only the handle. The caller's value comes back
/// at the end of the flow so it can confirm this callback belongs to the login
/// it started.
///
/// The two halves protect different things and neither replaces the other. The
/// server-side entry gives PKCE verifier custody, one-time redemption, expiry
/// and provider binding; the caller's value is what ties the flow to a
/// particular browser or CLI process, which the server cannot observe.
///
/// Omitting `state` (e.g. a HEAD probe) returns 200 when OIDC is configured,
/// 503 when it is not — useful for the frontend to decide whether to show the
/// "Sign in with OIDC" button.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oidc/login",
    tag = "front-office",
    params(
        ("state" = Option<String>, Query, description = "CSRF state (frontend-generated); omit to probe availability"),
        ("provider" = Option<String>, Query, description = "Provider name to log in with; defaults to the first configured provider"),
    ),
    responses(
        (status = 200, description = "OIDC is configured (probe response when state is absent)", body = OkResponse),
        (status = 302, description = "Redirect to OIDC authorization endpoint"),
        (status = 404, description = "Named provider not found"),
        (status = 503, description = "OIDC not configured"),
    ),
)]
#[get("/api/v1/auth/oidc/login")]
pub async fn oidc_login(
    flows: web::Data<Vec<OidcSsoFlow>>,
    login_states: web::Data<Arc<dyn LoginStateStore>>,
    query: web::Query<LoginQuery>,
) -> Result<impl Responder, AppError> {
    if flows.is_empty() {
        return Err(AppError::service_unavailable(
            "OIDC SSO is not configured on this server",
        ));
    }

    let Some(ref spa_state) = query.state else {
        // Probe request — confirm the endpoint exists and OIDC is configured.
        return Ok(HttpResponse::Ok().json(OkResponse::new()));
    };

    // Refused before anything is written: this route is unauthenticated and
    // unthrottled, and the row it creates outlives the request by
    // `LOGIN_STATE_TTL_SECS`. See `MAX_SPA_STATE_LEN`.
    if spa_state.len() > MAX_SPA_STATE_LEN {
        return Err(AppError::bad_request(format!(
            "`state` must be at most {MAX_SPA_STATE_LEN} bytes"
        )));
    }

    let sso = if let Some(ref name) = query.provider {
        flows.iter().find(|f| &f.name == name)
    } else {
        flows.first()
    };

    let Some(sso) = sso else {
        return Err(AppError::not_found("OIDC provider not found"));
    };

    // The provider is recorded server-side rather than encoded into the state
    // string. The previous design sent `"<provider>:<caller state>"` and parsed
    // it back on return, which meant the value deciding *which token endpoint a
    // code is redeemed at* arrived from the network.
    let pkce = PkceChallenge::generate();
    let state = random_urlsafe();
    login_states
        .put(
            &state,
            LoginState {
                provider: sso.name.clone(),
                code_verifier: pkce.verifier.clone(),
                nonce: pkce.nonce.clone(),
                spa_state: spa_state.clone(),
            },
            LOGIN_STATE_TTL_SECS,
        )
        .await?;

    let location = sso.authorization_url(&state, &pkce);
    Ok(HttpResponse::Found()
        .insert_header(("Location", location))
        // A started login is single-use; a cached 302 would send a second visit
        // to an authorization URL whose state has already been redeemed.
        .insert_header(("Cache-Control", "no-store"))
        .finish())
}

// ── Callback ───────────────────────────────────────────────────────────────────

/// Handle the OIDC provider's redirect back: validate the state, exchange the
/// code for tokens, and redirect the browser to the SPA with the tokens in the
/// URL fragment.
///
/// The returned `state` must match an entry this server created and has not yet
/// consumed, or nothing is exchanged. The caller then compares the echoed
/// `oidc_state` against the value it kept locally — see `oidc_login` for why
/// both checks exist.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oidc/callback",
    tag = "front-office",
    responses(
        (status = 302, description = "Redirect to SPA with tokens or error"),
        (status = 503, description = "OIDC not configured"),
    ),
)]
#[get("/api/v1/auth/oidc/callback")]
pub async fn oidc_callback(
    req: HttpRequest,
    flows: web::Data<Vec<OidcSsoFlow>>,
    login_states: web::Data<Arc<dyn LoginStateStore>>,
    admin_svc: web::Data<Arc<AdminService>>,
    query: web::Query<CallbackQuery>,
) -> impl Responder {
    let (response, outcome) = callback_response(&flows, &login_states, &query).await;
    if let Some(event) = outcome.into_event(caller_net(&req)) {
        admin_svc.record_event(event).await;
    }
    response
}

/// What a callback did, for its audit row (RFC 0036 §6.1).
///
/// A failure carries its *class* — never the code, the state or a token — and
/// the provider when the callback got far enough to know it.
enum SignInOutcome {
    /// Nothing was attempted: SSO is not configured.
    NotAttempted,
    Failed {
        class: &'static str,
        provider: Option<String>,
    },
    Succeeded {
        provider: String,
        user_id: Option<String>,
    },
}

impl SignInOutcome {
    fn failed(class: &'static str, provider: Option<&str>) -> Self {
        Self::Failed {
            class,
            provider: provider.map(str::to_owned),
        }
    }

    fn into_event(self, net: CallerNet) -> Option<AccessEvent> {
        let (action, user_id, result, provider) = match self {
            Self::NotAttempted => return None,
            Self::Failed { class, provider } => (
                AccessAction::SignInFailed,
                None,
                AccessResult::Denied {
                    reason: class.to_owned(),
                },
                provider,
            ),
            Self::Succeeded { provider, user_id } => (
                AccessAction::SignIn,
                user_id,
                AccessResult::Allowed,
                Some(provider),
            ),
        };
        Some(AccessEvent::about_identity(
            action,
            user_id,
            Role::Anonymous,
            result,
            net,
            provider.map(|p| format!("provider={p}")),
        ))
    }
}

async fn callback_response(
    flows: &[OidcSsoFlow],
    login_states: &Arc<dyn LoginStateStore>,
    query: &CallbackQuery,
) -> (HttpResponse, SignInOutcome) {
    if flows.is_empty() {
        return (
            HttpResponse::ServiceUnavailable()
                .json(serde_json::json!({ "error": "OIDC SSO is not configured on this server" })),
            SignInOutcome::NotAttempted,
        );
    }

    // Use the first provider's frontend_url as fallback for error redirects.
    let fallback_base = flows[0].frontend_url.trim_end_matches('/').to_owned();

    // Provider-side error (e.g. user denied access).
    if let Some(ref err) = query.error {
        let desc = query.error_description.as_deref().unwrap_or(err.as_str());
        return (
            spa_error_redirect(&fallback_base, desc),
            SignInOutcome::failed("provider_error", None),
        );
    }

    let code = match query.code.as_deref() {
        Some(c) => c.to_owned(),
        None => {
            return (
                spa_error_redirect(&fallback_base, "Authorization code missing from callback."),
                SignInOutcome::failed("code_missing", None),
            )
        }
    };

    // Consume the state before anything else. `take` deletes as it reads, so a
    // replayed callback — the same code and state delivered twice — finds
    // nothing the second time. A state this server never issued, or one that
    // expired, lands here too.
    let Some(raw_state) = query.state.as_deref().filter(|s| !s.is_empty()) else {
        return (
            spa_error_redirect(&fallback_base, "Sign-in state missing from callback."),
            SignInOutcome::failed("state_missing", None),
        );
    };
    let login = match login_states.take(raw_state).await {
        Ok(Some(login)) => login,
        Ok(None) => {
            tracing::warn!("OIDC callback with an unknown, expired or already-redeemed state");
            return (
                spa_error_redirect(
                    &fallback_base,
                    "This sign-in link has expired or was already used. Please sign in again.",
                ),
                SignInOutcome::failed("state_mismatch", None),
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "reading OIDC login state failed");
            return (
                spa_error_redirect(
                    &fallback_base,
                    "Failed to complete sign-in. Please try again.",
                ),
                SignInOutcome::failed("state_unreadable", None),
            );
        }
    };

    // The provider comes from the stored entry, so a code can only ever be
    // redeemed at the token endpoint that issued the authorization request.
    let Some(sso) = flows.iter().find(|f| f.name == login.provider) else {
        tracing::warn!(
            provider = %login.provider,
            "OIDC login state names a provider that is no longer configured"
        );
        return (
            spa_error_redirect(&fallback_base, "OIDC provider not found."),
            SignInOutcome::failed("provider_unknown", Some(&login.provider)),
        );
    };

    let base = sso.frontend_url.trim_end_matches('/').to_owned();

    let tokens = match sso.exchange_code(&code, &login.code_verifier).await {
        Ok(tokens) => tokens,
        Err(e) => {
            // Log the full anyhow chain (which can include the upstream token
            // endpoint URL) server-side only; the browser-visible redirect gets a
            // generic message so it never leaks upstream connectivity details.
            tracing::warn!(error = %e, "OIDC token exchange failed");
            return (
                spa_error_redirect(&base, "Failed to complete sign-in. Please try again."),
                SignInOutcome::failed("token_exchange", Some(&sso.name)),
            );
        }
    };

    // Order matters, and it is this way round rather than the reverse.
    //
    // A provider that returns no `id_token` and an opaque access token cannot
    // work here: `OidcAuthProvider::authenticate` validates a JWT. Said once,
    // loudly, at the point of failure — otherwise every later request quietly
    // resolves to anonymous and the operator goes looking at their RBAC rules.
    //
    // Running the nonce check first made this diagnostic unreachable:
    // `login.nonce` is never empty, so `verify_nonce` would `insecure_decode`
    // the opaque token, fail to parse it, and answer with the generic "try
    // again" — leaving the operator with the exact silent failure the message
    // below exists to prevent.
    if !OidcSsoFlow::session_token_is_a_jwt(&tokens.session_token) {
        tracing::error!(
            provider = %login.provider,
            "the identity provider returned no id_token and an opaque access token; \
             batlehub authenticates JWTs, so this provider needs an `openid` scope \
             (or, on Auth0/Okta, an API audience that yields a JWT access token)"
        );
        return (
            spa_error_redirect(
                &base,
                "This identity provider is not returning a usable token. Ask an administrator \
                 to check the server log.",
            ),
            SignInOutcome::failed("claims", Some(&sso.name)),
        );
    }

    // OpenID Connect Core §3.1.3.7 step 11. The nonce was minted for this one
    // authorization request and kept server-side, so a matching one proves the
    // ID token was issued in response to *this* login and not replayed from
    // another.
    //
    // Only when the session token *is* the ID token. §3.1.3.7 is a rule about
    // ID tokens, and a provider that issued none has nothing to carry the claim:
    // `token_request` fell back to the access token, and requiring a `nonce` of
    // that rejects every JWT-access-token deployment — the configuration the
    // message above tells operators to reach for. What ties *that* case to this
    // login is PKCE plus the one-time `state`, both already checked, and the
    // fallback never becomes a way to skip the check because the token it
    // reaches for is one the provider minted for this exchange either way.
    if tokens.has_id_token {
        if let Err(e) = OidcSsoFlow::verify_nonce(&tokens.session_token, &login.nonce) {
            tracing::warn!(error = %e, provider = %login.provider, "OIDC nonce check failed");
            return (
                spa_error_redirect(&base, "Failed to complete sign-in. Please try again."),
                SignInOutcome::failed("nonce", Some(&sso.name)),
            );
        }
    }

    // Tokens ride in the URL **fragment**, not the query string. A fragment is
    // never sent to a server, so it stays out of the SPA host's access logs and
    // out of any proxy in front of it; a query string would be written to both.
    // It is still visible in browser history, which is why `router/index.ts`
    // clears it before the first navigation.
    //
    // `Referrer-Policy: no-referrer` keeps the token-bearing URL out of the
    // `Referer` of sub-resource loads on the landing page, and
    // `Cache-Control: no-store` keeps the redirect out of intermediary caches.
    //
    // The parameter is still called `oidc_access_token` — it is what the SPA and
    // CLI read, and renaming it would break every client mid-upgrade for no
    // security gain. What changed is its *value*: the ID token, when the
    // provider issued one.
    let mut location = format!(
        "{base}/#oidc_access_token={}&oidc_state={}&oidc_provider={}",
        url_encode(&tokens.session_token),
        url_encode(&login.spa_state),
        url_encode(&sso.name),
    );
    if let Some(ref rt) = tokens.refresh_token {
        location.push_str(&format!("&oidc_refresh_token={}", url_encode(rt)));
    }
    if let Some(exp) = tokens.expires_in {
        location.push_str(&format!("&oidc_expires_in={exp}"));
    }
    let outcome = SignInOutcome::Succeeded {
        provider: sso.name.clone(),
        user_id: sso.session_subject(&tokens.session_token),
    };
    (
        HttpResponse::Found()
            .insert_header(("Location", location))
            .insert_header(("Referrer-Policy", "no-referrer"))
            .insert_header(("Cache-Control", "no-store"))
            .finish(),
        outcome,
    )
}

// ── Refresh ────────────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
    /// Provider name that issued the refresh token. Defaults to the first configured provider.
    pub provider: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RefreshResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

/// Exchange a refresh token for a new access token.
///
/// The backend performs the confidential token refresh grant so the
/// `client_secret` never needs to be exposed to the browser.
#[utoipa::path(
    post,
    path = "/api/v1/auth/oidc/refresh",
    tag = "front-office",
    request_body = RefreshRequest,
    responses(
        (status = 200, description = "New tokens", body = RefreshResponse),
        (status = 400, description = "Refresh failed"),
        (status = 404, description = "Named provider not found"),
        (status = 429, description = "Too many refresh attempts from this client"),
        (status = 503, description = "OIDC not configured"),
    ),
)]
#[post("/api/v1/auth/oidc/refresh")]
pub async fn oidc_refresh(
    req: HttpRequest,
    flows: web::Data<Vec<OidcSsoFlow>>,
    limiter: web::Data<Arc<RefreshRateLimiter>>,
    body: web::Json<RefreshRequest>,
) -> Result<impl Responder, AppError> {
    // Same proxy-trust verdict as every other IP-consuming path, so a caller
    // behind an untrusted peer cannot spoof `X-Forwarded-For` to get a fresh
    // bucket per request.
    let client = crate::middleware::proxy_trust::client_ip(
        &req,
        crate::middleware::proxy_trust::peer_trust(&req),
    );
    if !limiter.allow(&client) {
        tracing::warn!(client = %client, "OIDC refresh rate limit exceeded");
        return Err(AppError::too_many_requests(
            "too many refresh attempts; try again shortly",
        ));
    }

    if flows.is_empty() {
        return Err(AppError::service_unavailable(
            "OIDC SSO is not configured on this server",
        ));
    }

    let sso = if let Some(ref name) = body.provider {
        flows.iter().find(|f| &f.name == name)
    } else {
        flows.first()
    };

    let Some(sso) = sso else {
        return Err(AppError::not_found("OIDC provider not found"));
    };

    match sso.refresh(&body.refresh_token).await {
        // `session_token`, matching the callback: the client stores this under
        // `access_token` and sends it back as a bearer, so it has to be the same
        // kind of credential the callback handed out or the session dies at the
        // first refresh. No nonce check here — a refresh has no authorization
        // request to be tied to, and OIDC Core §12.2 says an ID token from a
        // refresh may omit the claim.
        Ok(tokens) => {
            // The same shape check the callback makes, for the same reason and
            // with more at stake. An ID token in a refresh response is optional
            // (OIDC Core §12.2) and `refresh` sends no `scope`, so a provider
            // that omits one lands `session_token` on the access token — opaque
            // on Okta and Auth0. Handing that back would replace a working
            // credential with one `OidcAuthProvider::authenticate` answers
            // `Ok(None)` to: the client stores it, keeps sending it, and every
            // request from then on resolves to *anonymous* with no error
            // anywhere. Failing the refresh instead is recoverable — the client
            // still holds the credential that works and the user is asked to
            // sign in again.
            if !OidcSsoFlow::session_token_is_a_jwt(&tokens.session_token) {
                tracing::error!(
                    provider = %sso.name,
                    "OIDC refresh returned no id_token and an opaque access token; \
                     refusing to downgrade the session to an unusable credential"
                );
                return Err(AppError::bad_request("failed to refresh OIDC token"));
            }
            Ok(HttpResponse::Ok().json(RefreshResponse {
                access_token: tokens.session_token,
                refresh_token: tokens.refresh_token,
                expires_in: tokens.expires_in,
            }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "OIDC token refresh failed");
            Err(AppError::bad_request("failed to refresh OIDC token"))
        }
    }
}
