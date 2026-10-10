use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use rand::Rng as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use batlehub_core::ports::OidcAuthConfig;
use batlehub_core::{
    entities::{Identity, Role},
    error::CoreError,
    ports::{AuthProvider, RawAuthRequest},
};

use crate::registry::http_client::percent_encode;

const JWKS_MIN_REFRESH: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
struct OidcDiscovery {
    issuer: String,
    jwks_uri: String,
    authorization_endpoint: String,
    token_endpoint: String,
}

// ── SSO flow (Authorization Code) ────────────────────────────────────────────

/// Tokens returned by the OIDC provider after a successful code exchange or refresh.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OidcTokens {
    /// The credential the SPA and CLI send back as a bearer, and the one this
    /// server authenticates.
    ///
    /// This is the **ID token** when the provider issued one, and the access
    /// token otherwise. The ID token is the assertion about *who the user is*:
    /// it is a JWT by specification, its `aud` is this client, and it carries the
    /// `nonce` that ties it to the authorization request. An access token is a
    /// credential for calling an API and is not required to be a JWT at all —
    /// several major providers issue opaque ones, against which this server's JWT
    /// validation cannot work.
    pub session_token: String,
    /// The raw access token, kept for calls to the identity provider itself.
    /// Never used to establish identity.
    pub access_token: String,
    /// Whether the provider actually issued an `id_token`.
    ///
    /// Stated rather than inferred from `session_token != access_token`: the two
    /// are equal whenever the fallback fired *and* whenever a provider happens
    /// to return the same string twice, and the callback needs to tell those
    /// apart. It decides whether OIDC Core §3.1.3.7 step 11 applies at all —
    /// the `nonce` check is a rule about ID tokens, and an access token has no
    /// claim to check.
    pub has_id_token: bool,
    pub refresh_token: Option<String>,
    /// Lifetime of the access token in seconds as reported by the provider.
    pub expires_in: Option<u64>,
}

/// Holds everything the web layer needs to initiate and complete the browser-based
/// OIDC Authorization Code flow.  Cloneable so it can be stored in `web::Data`.
#[derive(Clone)]
pub struct OidcSsoFlow {
    /// Provider name — matches the `name` field in `[[auth]]` config (default: `"oidc"`).
    pub name: String,
    pub client_id: String,
    client_secret: Option<String>,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// Base URL of the SPA — used to build the post-callback redirect.
    pub frontend_url: String,
    /// The claim `OidcAuthProvider` reads the user id from, so the sign-in's
    /// audit row names the same principal every later request is attributed
    /// to (RFC 0036 §6.1).
    pub user_id_claim: String,
    http: reqwest::Client,
}

/// A PKCE verifier/challenge pair plus the nonce for one authorization request.
///
/// Generated per login and kept server-side (`LoginStateStore`) until the code
/// comes back. The verifier is the secret half: if it ever reaches the browser
/// or the identity provider before redemption, PKCE stops protecting anything.
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
    pub nonce: String,
}

impl PkceChallenge {
    /// 32 bytes of CSPRNG per value, base64url-encoded without padding — the
    /// high end of the 43–128 character verifier range RFC 7636 §4.1 allows.
    pub fn generate() -> Self {
        let verifier = random_urlsafe();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
            nonce: random_urlsafe(),
        }
    }
}

/// 32 CSPRNG bytes, base64url without padding. Used for the PKCE verifier, the
/// nonce, and the `state` handle — all values that must be unguessable and safe
/// to put in a URL untouched.
pub fn random_urlsafe() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The endpoints and client credentials for one provider's SSO flow.
///
/// A separate struct because `OidcSsoFlow` also carries a shared
/// `reqwest::Client`, which callers should not have to supply.
pub struct OidcSsoFlowParams {
    pub name: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub frontend_url: String,
    pub user_id_claim: String,
}

impl OidcSsoFlow {
    /// Build a flow from already-resolved endpoints.
    ///
    /// The normal path is `OidcAuthProvider::new`, which discovers the endpoints
    /// from the provider's `.well-known` document and produces the flow as a
    /// by-product. This constructor exists for callers that already know them —
    /// notably integration tests standing the flow up against a mock identity
    /// provider, which cannot reach the private fields otherwise.
    pub fn new(params: OidcSsoFlowParams) -> Self {
        Self {
            name: params.name,
            client_id: params.client_id,
            client_secret: params.client_secret,
            redirect_uri: params.redirect_uri,
            scopes: params.scopes,
            authorization_endpoint: params.authorization_endpoint,
            token_endpoint: params.token_endpoint,
            frontend_url: params.frontend_url,
            user_id_claim: params.user_id_claim,
            http: reqwest::Client::new(),
        }
    }

    /// Build the provider's authorization URL.
    ///
    /// `state` is the server-generated handle into `LoginStateStore`, not
    /// anything the caller supplied — see `LoginState`.
    pub fn authorization_url(&self, state: &str, pkce: &PkceChallenge) -> String {
        let scope = self.scopes.join(" ");
        let params = [
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("scope", &scope),
            ("state", state),
            ("nonce", &pkce.nonce),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", "S256"),
        ];
        let qs = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{}?{}", self.authorization_endpoint, qs)
    }

    /// Exchange an authorization `code` for tokens, proving possession of the
    /// PKCE verifier that produced the challenge sent with the request.
    ///
    /// `code_verifier` is always sent, including when a `client_secret` is
    /// configured: RFC 9700 §2.1.1 asks for PKCE on confidential clients too,
    /// and an authorization server that does not recognise the parameter
    /// ignores it.
    pub async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> anyhow::Result<OidcTokens> {
        let mut params = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("code_verifier", code_verifier),
        ];
        if let Some(ref secret) = self.client_secret {
            params.push(("client_secret", secret.as_str()));
        }
        self.token_request(&params).await
    }

    /// Whether `token` is shaped like a JWT at all.
    ///
    /// The one diagnostic that matters after a code exchange: if the provider
    /// returned no `id_token` *and* its access token is opaque, nothing this
    /// server does downstream can work, and the failure would otherwise surface
    /// as every request silently degrading to anonymous — which reads as "my
    /// permissions are wrong", not "this identity provider is unsupported".
    pub fn session_token_is_a_jwt(token: &str) -> bool {
        let mut parts = token.split('.');
        let three = matches!(
            (parts.next(), parts.next(), parts.next()),
            (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty()
        );
        three && parts.next().is_none()
    }

    /// Check the `nonce` claim of an ID token against the one sent with the
    /// authorization request.
    ///
    /// Required by OpenID Connect Core §3.1.3.7 step 11, and the reason the
    /// nonce is generated per login and held server-side: it is what stops an ID
    /// token captured from one authorization request being replayed into
    /// another.
    ///
    /// Returns `Ok(())` when the token carries no `nonce` *and* no `nonce` was
    /// expected. A token that omits the claim when one was sent is rejected.
    pub fn verify_nonce(session_token: &str, expected_nonce: &str) -> anyhow::Result<()> {
        if expected_nonce.is_empty() {
            return Ok(());
        }
        // Reading claims without verifying the signature is safe here and only
        // here: the token was just received over TLS directly from the token
        // endpoint, in response to a request only this server could make. It is
        // the *authentication* path that must verify signatures, and it does.
        let claims: serde_json::Map<String, serde_json::Value> =
            jsonwebtoken::dangerous::insecure_decode(session_token)
                .map_err(|e| anyhow::anyhow!("reading nonce from the ID token: {e}"))?
                .claims;

        match claims.get("nonce").and_then(|v| v.as_str()) {
            Some(actual) if actual == expected_nonce => Ok(()),
            Some(_) => Err(anyhow::anyhow!(
                "ID token nonce does not match the authorization request"
            )),
            None => Err(anyhow::anyhow!(
                "ID token carries no nonce, but one was sent with the authorization request"
            )),
        }
    }

    /// The user id the session token names, for the sign-in's audit row.
    ///
    /// Read without verifying the signature, for the reason
    /// [`Self::verify_nonce`] gives: the token came over TLS straight from the
    /// token endpoint in answer to this server's own request. `None` when the
    /// claim is absent or the token is not a JWT — the row is still written,
    /// without a user.
    pub fn session_subject(&self, session_token: &str) -> Option<String> {
        let claims: serde_json::Map<String, serde_json::Value> =
            jsonwebtoken::dangerous::insecure_decode(session_token)
                .ok()?
                .claims;
        match claims.get(&self.user_id_claim)? {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    /// Use a refresh token to obtain a fresh access token (and possibly a new refresh token).
    pub async fn refresh(&self, refresh_token: &str) -> anyhow::Result<OidcTokens> {
        let mut params = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &self.client_id),
        ];
        if let Some(ref secret) = self.client_secret {
            params.push(("client_secret", secret.as_str()));
        }
        self.token_request(&params).await
    }

    async fn token_request(&self, params: &[(&str, &str)]) -> anyhow::Result<OidcTokens> {
        let resp: serde_json::Value = self
            .http
            .post(&self.token_endpoint)
            .form(params)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let access_token = resp["access_token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("token response missing access_token"))?;

        let id_token = resp["id_token"].as_str().map(str::to_owned);

        // Prefer the ID token; fall back to the access token so a provider that
        // returns none (or a refresh that omits it) still works, as it did
        // before. `session_token_is_a_jwt` is what tells an operator when that
        // fallback has landed them on an opaque credential.
        let session_token = id_token.clone().unwrap_or_else(|| access_token.clone());

        // `expires_in` describes the **access** token, and the credential the
        // clients hold is the **ID** token. Both schedule their refresh off this
        // number (`useAuth.ts`, `cli/src/api/auth.rs`), so on any provider whose
        // ID token is the shorter of the two — Entra ID, Auth0 — the session
        // expired while the client still believed it live. `authenticate` then
        // answers `Ok(None)` and the middleware falls through to *anonymous*
        // rather than `401`: the user silently loses their role and sees 403s,
        // with nothing to trigger a refresh. Take the smaller of the two.
        let expires_in = min_with_id_token_exp(resp["expires_in"].as_u64(), &session_token);

        Ok(OidcTokens {
            session_token,
            has_id_token: id_token.is_some(),
            access_token,
            refresh_token: resp["refresh_token"].as_str().map(str::to_owned),
            expires_in,
        })
    }
}

/// `reported`, capped by the session token's own `exp` when it has one.
///
/// Reading a claim without verifying the signature is safe here for the reason
/// `verify_nonce` gives: the token arrived over TLS straight from the token
/// endpoint, in response to a request only this server could make, and nothing
/// is *authenticated* on the strength of this read — it only shortens a client's
/// refresh timer, which is the safe direction to be wrong in. A token that is
/// not a JWT, or carries no `exp`, leaves the provider's own number alone.
fn min_with_id_token_exp(reported: Option<u64>, session_token: &str) -> Option<u64> {
    let decoded: Option<jsonwebtoken::TokenData<serde_json::Map<String, serde_json::Value>>> =
        jsonwebtoken::dangerous::insecure_decode(session_token).ok();
    let Some(exp) = decoded
        .as_ref()
        .and_then(|d| d.claims.get("exp"))
        .and_then(serde_json::Value::as_u64)
    else {
        // Not a JWT, or a JWT with no `exp` — the opaque-access-token fallback.
        // The provider's own number is all there is.
        return reported;
    };
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    // An already-expired ID token yields `0`, which is a client that refreshes
    // at once rather than one that waits out an access token it is not using.
    let remaining = exp.saturating_sub(now);
    Some(reported.map_or(remaining, |reported| reported.min(remaining)))
}

struct JwksCache {
    keys: JwkSet,
    fetched_at: Instant,
}

pub struct OidcAuthProvider {
    name: String,
    /// Canonical issuer identifier from the OIDC discovery document (`issuer` field).
    /// Used to validate the `iss` claim so that two providers with different issuers
    /// cannot validate each other's tokens.
    issuer: String,
    user_id_claim: String,
    role_claim: String,
    role_mappings: HashMap<String, String>,
    /// Accepted `aud` values. Never empty: it falls back to `[client_id]`, so
    /// there is no configuration in which audience goes unchecked.
    audiences: Vec<String>,
    http: reqwest::Client,
    jwks_uri: String,
    cache: Arc<RwLock<JwksCache>>,
    sso: Option<OidcSsoFlow>,
}

impl OidcAuthProvider {
    pub async fn new(cfg: &OidcAuthConfig) -> anyhow::Result<Self> {
        for (claim_value, role) in &cfg.role_mappings {
            role.parse::<Role>()
                .map_err(|e| anyhow::anyhow!("role_mappings.{claim_value}: {e}"))?;
        }

        let http = reqwest::Client::new();

        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            cfg.issuer_url.trim_end_matches('/')
        );
        let discovery: OidcDiscovery = http
            .get(&discovery_url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("fetching OIDC discovery document: {e}"))?
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("parsing OIDC discovery document: {e}"))?;

        // OpenID Connect Discovery §4.3 requires this comparison. Without it,
        // the `iss` this provider goes on to enforce is whatever the document
        // said it was — so a document served from the wrong place defines its
        // own notion of who it is, and `jwks_uri` names the keys that will be
        // trusted.
        if !issuer_matches(&cfg.issuer_url, &discovery.issuer) {
            anyhow::bail!(
                "OIDC discovery document at '{discovery_url}' declares issuer '{}', \
                 which is not the configured issuer_url '{}'. \
                 Set issuer_url to the value the provider publishes.",
                discovery.issuer,
                cfg.issuer_url,
            );
        }

        let keys = fetch_jwks(&http, &discovery.jwks_uri).await.map_err(|e| {
            anyhow::anyhow!("fetching initial JWKS from {}: {e}", discovery.jwks_uri)
        })?;

        let sso = cfg.redirect_uri.as_ref().map(|redirect_uri| OidcSsoFlow {
            name: cfg.name.clone(),
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
            redirect_uri: redirect_uri.clone(),
            scopes: cfg.scopes.clone(),
            authorization_endpoint: discovery.authorization_endpoint.clone(),
            token_endpoint: discovery.token_endpoint.clone(),
            frontend_url: cfg.frontend_url.clone(),
            user_id_claim: cfg.user_id_claim.clone(),
            http: http.clone(),
        });

        Ok(Self {
            name: cfg.name.clone(),
            issuer: discovery.issuer,
            user_id_claim: cfg.user_id_claim.clone(),
            role_claim: cfg.role_claim.clone(),
            role_mappings: cfg.role_mappings.clone(),
            // An ID token issued to this client carries `aud = client_id`, so
            // that is the right default and it is never empty — `authenticate`
            // has no "audience unchecked" branch to fall into.
            audiences: if cfg.audiences.is_empty() {
                vec![cfg.client_id.clone()]
            } else {
                cfg.audiences.clone()
            },
            http,
            jwks_uri: discovery.jwks_uri,
            cache: Arc::new(RwLock::new(JwksCache {
                keys,
                fetched_at: Instant::now(),
            })),
            sso,
        })
    }

    /// Returns the SSO flow helper if `redirect_uri` was configured, `None` otherwise.
    pub fn sso_flow(&self) -> Option<&OidcSsoFlow> {
        self.sso.as_ref()
    }

    async fn get_decoding_key(&self, kid: Option<&str>) -> Result<DecodingKey, CoreError> {
        // Try the current cache first.
        {
            let cache = self.cache.read().await;
            if let Some(key) = find_key(&cache.keys, kid) {
                return Ok(key);
            }
            // If the cache was refreshed very recently, don't hammer the JWKS endpoint.
            if cache.fetched_at.elapsed() < JWKS_MIN_REFRESH {
                return Err(CoreError::Auth("unknown JWT signing key".to_owned()));
            }
        }

        // Refresh JWKS and update cache.
        let new_keys = fetch_jwks(&self.http, &self.jwks_uri)
            .await
            .map_err(|e| CoreError::Auth(format!("JWKS refresh failed: {e}")))?;

        let key = find_key(&new_keys, kid)
            .ok_or_else(|| CoreError::Auth("unknown JWT signing key after refresh".to_owned()))?;

        *self.cache.write().await = JwksCache {
            keys: new_keys,
            fetched_at: Instant::now(),
        };

        Ok(key)
    }

    fn map_role(&self, claim_value: &serde_json::Value) -> Role {
        let candidates: Vec<&str> = match claim_value {
            serde_json::Value::String(s) => vec![s.as_str()],
            serde_json::Value::Array(arr) => arr.iter().filter_map(|v| v.as_str()).collect(),
            _ => vec![],
        };

        candidates
            .into_iter()
            .filter_map(|s| self.role_mappings.get(s))
            .filter_map(|mapped| mapped.parse::<Role>().ok())
            .max()
            .unwrap_or(Role::Anonymous)
    }
}

/// The audience every test provider accepts and every test token carries.
/// Audience is always checked now — there is no "unchecked" configuration — so a
/// token without a matching `aud` is simply not for us.
#[cfg(test)]
const TEST_AUDIENCE: &str = "batlehub-test-client";

/// Test-only constructor that skips the network bootstrap.
#[cfg(test)]
impl OidcAuthProvider {
    fn for_testing(
        name: impl Into<String>,
        user_id_claim: impl Into<String>,
        role_claim: impl Into<String>,
        role_mappings: HashMap<String, String>,
        jwks: JwkSet,
    ) -> Self {
        Self::for_testing_with_audiences(
            name,
            user_id_claim,
            role_claim,
            role_mappings,
            jwks,
            vec![TEST_AUDIENCE.to_owned()],
        )
    }

    fn for_testing_with_audiences(
        name: impl Into<String>,
        user_id_claim: impl Into<String>,
        role_claim: impl Into<String>,
        role_mappings: HashMap<String, String>,
        jwks: JwkSet,
        audiences: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            issuer: String::new(), // no issuer validation in tests
            user_id_claim: user_id_claim.into(),
            role_claim: role_claim.into(),
            role_mappings,
            audiences,
            http: reqwest::Client::new(),
            jwks_uri: String::new(),
            cache: Arc::new(RwLock::new(JwksCache {
                keys: jwks,
                fetched_at: Instant::now(),
            })),
            sso: None,
        }
    }
}

/// The claims a token must actually carry, not merely satisfy if present.
///
/// `jsonwebtoken`'s `set_audience`/`set_issuer` validate their claim **only when
/// it appears in the token** — a token omitting `aud` passes an audience check
/// that a token with the wrong `aud` fails. Listing them here is what turns
/// "must match if present" into "must match".
///
/// Shared by both JWT providers so the two cannot drift.
pub(crate) fn required_spec_claims(with_issuer: bool) -> Vec<&'static str> {
    let mut claims = vec!["exp", "aud"];
    if with_issuer {
        claims.push("iss");
    }
    claims
}

/// Whether the discovery document's `issuer` is the one that was configured.
///
/// Compared with a trailing slash normalised away on both sides, since that is
/// the one difference providers and operators routinely disagree about and it
/// carries no meaning. Everything else must match exactly.
fn issuer_matches(configured: &str, discovered: &str) -> bool {
    configured.trim_end_matches('/') == discovered.trim_end_matches('/')
}

fn find_key(jwks: &JwkSet, kid: Option<&str>) -> Option<DecodingKey> {
    let jwk = if let Some(kid) = kid {
        jwks.find(kid)
    } else {
        jwks.keys.first()
    }?;
    DecodingKey::from_jwk(jwk).ok()
}

async fn fetch_jwks(http: &reqwest::Client, uri: &str) -> Result<JwkSet, reqwest::Error> {
    http.get(uri).send().await?.json().await
}

#[async_trait]
impl AuthProvider for OidcAuthProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn authenticate(&self, req: &RawAuthRequest) -> Result<Option<Identity>, CoreError> {
        let Some(token) = req.bearer_token() else {
            return Ok(None);
        };

        // Not a JWT at all is "not ours", not a fault — the same `Ok(None)` the
        // wrong-`aud`/wrong-`iss` arm below returns, and for the same reason.
        //
        // Returned as `Err`, this counted every personal access token as an
        // authentication *failure*: a PAT is `bh_pat_…` and carries no dots, the
        // OIDC provider is consulted before `UserTokenAuthProvider`, so on any
        // OIDC deployment every PAT-authenticated request logged a WARN and
        // incremented `batlehub_auth_failures_total{provider="oidc"}`. A CI job
        // pulling at 100 rps produced 100 warn lines a second and pinned any
        // alert built on that metric — while the credential it was complaining
        // about was perfectly valid for the provider one place down the list.
        let Ok(header) = decode_header(token) else {
            tracing::debug!(provider = %self.name, "credential is not a JWT; not ours");
            return Ok(None);
        };

        let decoding_key = self.get_decoding_key(header.kid.as_deref()).await?;

        // Validate the issuer so each provider only accepts tokens from its own issuer.
        // This prevents two providers that share JWKS keys (e.g. same identity server,
        // different client apps) from processing each other's tokens.
        //
        // And validate the audience, which `iss` alone does not cover: one issuer
        // signs for all of its clients, so without `aud` a token minted for a
        // different application at the same identity server authenticates here.
        let mut validation = Validation::new(header.alg);
        validation.set_audience(&self.audiences);
        if !self.issuer.is_empty() {
            validation.set_issuer(&[&self.issuer]);
        }
        // `set_audience` alone only checks `aud` **when the claim is present** —
        // a token carrying no `aud` at all would sail through it. Requiring the
        // claim is what closes that, and the same applies to `iss`.
        validation.set_required_spec_claims(&required_spec_claims(!self.issuer.is_empty()));

        let token_data = match decode::<serde_json::Map<String, serde_json::Value>>(
            token,
            &decoding_key,
            &validation,
        ) {
            Ok(data) => data,
            Err(e) if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::ExpiredSignature) => {
                tracing::debug!(provider = %self.name, "JWT expired");
                return Ok(None);
            }
            // Not ours: wrong audience or issuer, or missing one of them
            // entirely. Per the `AuthProvider` contract that is `Ok(None)` — the
            // next provider still gets its turn, and the auth-failure counter
            // stays for genuine provider faults. A missing `exp` is *not*
            // included: that is a malformed token, not someone else's.
            Err(e)
                if matches!(
                    e.kind(),
                    jsonwebtoken::errors::ErrorKind::InvalidAudience
                        | jsonwebtoken::errors::ErrorKind::InvalidIssuer
                ) || matches!(
                    e.kind(),
                    jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(c) if c == "aud" || c == "iss"
                ) =>
            {
                tracing::debug!(provider = %self.name, error = %e, "JWT is not for this provider");
                return Ok(None);
            }
            Err(e) => return Err(CoreError::Auth(format!("JWT validation failed: {e}"))),
        };

        let claims = token_data.claims;

        let user_id = claims
            .get(&self.user_id_claim)
            .and_then(|v| v.as_str())
            .map(str::to_owned);

        let role_claim_value = claims.get(&self.role_claim);

        let role = role_claim_value
            .map(|v| self.map_role(v))
            .unwrap_or(Role::Anonymous);

        // Extract raw strings from the claim, then namespace-prefix any value that is
        // not explicitly in role_mappings so groups from different providers stay distinct.
        let raw_groups: Vec<String> = role_claim_value
            .map(|v| match v {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                _ => vec![],
            })
            .unwrap_or_default();

        let groups: Vec<String> = raw_groups
            .into_iter()
            .map(|s| {
                if self.role_mappings.contains_key(&s) {
                    s
                } else {
                    format!("{}:{s}", self.name)
                }
            })
            .collect();

        Ok(Some(Identity {
            user_id,
            role,
            auth_provider: Some(self.name.clone()),
            groups,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
    use serde_json::json;
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    // ECDSA P-256 test key pair taken from jsonwebtoken's own test fixtures.
    // Private: PKCS#8 PEM; public key encoded as JWK below.
    const TEST_EC_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWTFfCGljY6aw3Hrt\n\
kHmPRiazukxPLb6ilpRAewjW8nihRANCAATDskChT+Altkm9X7MI69T3IUmrQU0L\n\
950IxEzvw/x5BMEINRMrXLBJhqzO9Bm+d6JbqA21YQmd1Kt4RzLJR1W+\n\
-----END PRIVATE KEY-----";

    // JWK Set whose public key matches TEST_EC_PRIVATE_KEY above.
    // x/y coordinates derived from the SubjectPublicKeyInfo DER.
    const TEST_JWKS_JSON: &str = r#"{
      "keys": [{
        "kty": "EC",
        "crv": "P-256",
        "use": "sig",
        "kid": "test-kid",
        "x": "w7JAoU_gJbZJvV-zCOvU9yFJq0FNC_edCMRM78P8eQQ",
        "y": "wQg1EytcsEmGrM70Gb53oluoDbVhCZ3Uq3hHMslHVb4"
      }]
    }"#;

    fn future_exp() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600
    }

    fn past_exp() -> i64 {
        // Use an hour in the past to stay clear of jsonwebtoken's default 60-second leeway.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 3600
    }

    fn test_jwks() -> JwkSet {
        serde_json::from_str(TEST_JWKS_JSON).unwrap()
    }

    fn make_provider(
        name: &str,
        user_id_claim: &str,
        role_claim: &str,
        role_mappings: HashMap<String, String>,
    ) -> OidcAuthProvider {
        OidcAuthProvider::for_testing(name, user_id_claim, role_claim, role_mappings, test_jwks())
    }

    fn default_provider() -> OidcAuthProvider {
        make_provider(
            "oidc",
            "sub",
            "role",
            [
                ("admin".to_owned(), "admin".to_owned()),
                ("developer".to_owned(), "user".to_owned()),
                ("viewer".to_owned(), "anonymous".to_owned()),
            ]
            .into(),
        )
    }

    fn signed_token(extra_header_kid: Option<&str>, claims: serde_json::Value) -> String {
        let header = Header {
            alg: Algorithm::ES256,
            kid: extra_header_kid.map(str::to_owned),
            ..Default::default()
        };
        let key = EncodingKey::from_ec_pem(TEST_EC_PRIVATE_KEY.as_bytes()).unwrap();
        encode(&header, &claims, &key).unwrap()
    }

    /// A JWT-shaped string with the given claims and a signature that is not one.
    ///
    /// Enough for `verify_nonce`, which reads claims without verifying — see the
    /// comment there for why that is safe on the token-endpoint path. The header
    /// still names a real algorithm, because `insecure_decode` parses it even
    /// though it verifies nothing.
    fn unsigned_jwt(claims: &serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap()),
            URL_SAFE_NO_PAD.encode("not-a-real-signature"),
        )
    }

    fn bearer(token: &str) -> RawAuthRequest {
        RawAuthRequest {
            headers: [("authorization".to_owned(), format!("Bearer {token}"))].into(),
            query_params: Default::default(),
            source_ip: None,
        }
    }

    fn no_auth() -> RawAuthRequest {
        RawAuthRequest {
            headers: Default::default(),
            query_params: Default::default(),
            source_ip: None,
        }
    }

    // ── Header parsing ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn no_auth_header_returns_none() {
        let p = default_provider();
        assert!(p.authenticate(&no_auth()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn basic_auth_header_returns_none() {
        let p = default_provider();
        let req = RawAuthRequest {
            headers: [("authorization".to_owned(), "Basic dXNlcjpwYXNz".to_owned())].into(),
            query_params: Default::default(),
            source_ip: None,
        };
        assert!(p.authenticate(&req).await.unwrap().is_none());
    }

    /// A credential this provider cannot even read as a JWT is somebody else's,
    /// not a fault of this one.
    ///
    /// It used to be `Err(CoreError::Auth(…))`, which the middleware turns into
    /// a WARN line and a `batlehub_auth_failures_total{provider="oidc"}`
    /// increment. A personal access token is `bh_pat_…` — no dots, no header to
    /// decode — and the OIDC provider is consulted before
    /// `UserTokenAuthProvider`, so on any OIDC deployment *every* PAT request
    /// logged a failure for a credential that was about to authenticate
    /// perfectly well one provider down the list.
    #[tokio::test]
    async fn a_credential_that_is_not_a_jwt_is_declined_not_failed() {
        let p = default_provider();
        for token in ["not.a.valid.jwt", "bh_pat_abcdef0123456789", ""] {
            assert!(
                p.authenticate(&bearer(token)).await.unwrap().is_none(),
                "{token} must be declined, leaving the next provider its turn"
            );
        }
    }

    // ── Role mapping ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn string_role_claim_maps_to_correct_role() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "role": "developer", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.role, Role::User);
        assert_eq!(id.user_id.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn array_role_claim_picks_highest_role() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "bob", "role": ["viewer", "developer", "admin"], "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Admin);
    }

    #[tokio::test]
    async fn array_with_one_known_entry_returns_that_role() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "carol", "role": ["unknown-group", "developer"], "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.role, Role::User);
    }

    #[tokio::test]
    async fn unmapped_string_role_defaults_to_anonymous() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "dave", "role": "superuser", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Anonymous);
    }

    #[tokio::test]
    async fn all_unmapped_array_values_default_to_anonymous() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "dave", "role": ["unknown1", "unknown2"], "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Anonymous);
    }

    #[tokio::test]
    async fn missing_role_claim_defaults_to_anonymous() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "eve", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Anonymous);
    }

    #[tokio::test]
    async fn custom_user_id_claim_is_extracted() {
        let p = make_provider(
            "oidc",
            "email",
            "role",
            [("admin".to_owned(), "admin".to_owned())].into(),
        );
        let token = signed_token(
            Some("test-kid"),
            json!({ "email": "alice@example.com", "role": "admin", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.user_id.as_deref(), Some("alice@example.com"));
        assert_eq!(id.role, Role::Admin);
    }

    #[tokio::test]
    async fn missing_user_id_claim_leaves_user_id_none() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "role": "admin", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.user_id, None);
    }

    // ── ID token, nonce, opaque-token diagnosis ───────────────────────────────
    // The flow used to read only `access_token` from the token response and hand
    // that back as the session credential. An access token is a credential for
    // calling an API, not an assertion about who the user is: it carries no
    // `nonce`, and several major providers issue it opaque, against which this
    // server's JWT validation cannot work at all.

    #[tokio::test]
    async fn the_id_token_becomes_the_session_token() {
        let mut server = mockito::Server::new_async().await;
        let _token = server
            .mock("POST", "/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"opaque-at","id_token":"h.p.s","expires_in":300}"#)
            .create_async()
            .await;

        let mut flow = test_flow();
        flow.token_endpoint = format!("{}/token", server.url());
        let tokens = flow.exchange_code("code", "verifier").await.unwrap();

        assert_eq!(
            tokens.session_token, "h.p.s",
            "identity comes from id_token"
        );
        assert_eq!(
            tokens.access_token, "opaque-at",
            "the access token is kept for calls to the IdP, not thrown away"
        );
    }

    #[tokio::test]
    async fn the_access_token_is_the_fallback_when_no_id_token_is_returned() {
        let mut server = mockito::Server::new_async().await;
        let _token = server
            .mock("POST", "/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"jwt.access.token"}"#)
            .create_async()
            .await;

        let mut flow = test_flow();
        flow.token_endpoint = format!("{}/token", server.url());
        let tokens = flow.exchange_code("code", "verifier").await.unwrap();
        assert_eq!(tokens.session_token, "jwt.access.token");
    }

    #[test]
    fn verify_nonce_accepts_a_matching_claim() {
        let token = unsigned_jwt(&json!({ "sub": "alice", "nonce": "n-123" }));
        assert!(OidcSsoFlow::verify_nonce(&token, "n-123").is_ok());
    }

    #[test]
    fn verify_nonce_rejects_a_different_claim() {
        // The replay this exists to stop: an ID token captured from one
        // authorization request, presented against another.
        let token = unsigned_jwt(&json!({ "sub": "alice", "nonce": "from-another-login" }));
        assert!(OidcSsoFlow::verify_nonce(&token, "n-123").is_err());
    }

    #[test]
    fn verify_nonce_rejects_a_missing_claim_when_one_was_sent() {
        let token = unsigned_jwt(&json!({ "sub": "alice" }));
        let err = OidcSsoFlow::verify_nonce(&token, "n-123").unwrap_err();
        assert!(err.to_string().contains("no nonce"), "got: {err}");
    }

    #[test]
    fn verify_nonce_is_a_no_op_when_none_was_sent() {
        // The refresh path: no authorization request to be tied to, and OIDC
        // Core §12.2 lets an ID token from a refresh omit the claim.
        let token = unsigned_jwt(&json!({ "sub": "alice" }));
        assert!(OidcSsoFlow::verify_nonce(&token, "").is_ok());
    }

    #[test]
    fn verify_nonce_rejects_a_token_it_cannot_read() {
        assert!(OidcSsoFlow::verify_nonce("not-a-jwt", "n-123").is_err());
    }

    #[test]
    fn session_token_is_a_jwt_recognises_opaque_credentials() {
        assert!(OidcSsoFlow::session_token_is_a_jwt("header.payload.sig"));

        // What Okta and Auth0 hand out by default, and the case that used to
        // degrade every request to anonymous with nothing said about it.
        assert!(!OidcSsoFlow::session_token_is_a_jwt(
            "00aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789abcdef"
        ));
        assert!(!OidcSsoFlow::session_token_is_a_jwt(""));
        assert!(!OidcSsoFlow::session_token_is_a_jwt("two.parts"));
        assert!(!OidcSsoFlow::session_token_is_a_jwt("a.b.c.d"));
        assert!(!OidcSsoFlow::session_token_is_a_jwt("a..c"));
    }

    // ── Audience ──────────────────────────────────────────────────────────────
    // One issuer signs for all of its clients. Validating only `iss` therefore
    // lets a token minted for any *other* application at the same identity
    // server authenticate here, with this deployment's role mapping applied to
    // its claims. `validate_aud` used to be off with a comment calling audience
    // "deployment-specific and not standardised".

    #[tokio::test]
    async fn a_token_for_another_audience_is_not_ours() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({
                "sub": "alice",
                "role": "admin",
                "exp": future_exp(),
                "aud": "some-other-application",
            }),
        );
        assert!(
            p.authenticate(&bearer(&token)).await.unwrap().is_none(),
            "a valid signature from the right issuer is not enough"
        );
    }

    #[tokio::test]
    async fn a_token_with_no_audience_at_all_is_not_ours() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "role": "admin", "exp": future_exp() }),
        );
        assert!(p.authenticate(&bearer(&token)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn any_configured_audience_is_accepted() {
        let p = OidcAuthProvider::for_testing_with_audiences(
            "oidc",
            "sub",
            "role",
            [("admin".to_owned(), "admin".to_owned())].into(),
            test_jwks(),
            vec!["api://one".to_owned(), "api://two".to_owned()],
        );
        for aud in ["api://one", "api://two"] {
            let token = signed_token(
                Some("test-kid"),
                json!({ "sub": "alice", "role": "admin", "exp": future_exp(), "aud": aud }),
            );
            assert_eq!(
                p.authenticate(&bearer(&token)).await.unwrap().unwrap().role,
                Role::Admin,
                "aud={aud} is configured and must be accepted"
            );
        }
    }

    #[tokio::test]
    async fn an_audience_array_containing_ours_is_accepted() {
        // `aud` is allowed to be an array; a token listing us among several
        // audiences is for us.
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({
                "sub": "alice",
                "role": "admin",
                "exp": future_exp(),
                "aud": ["another-app", TEST_AUDIENCE],
            }),
        );
        assert_eq!(
            p.authenticate(&bearer(&token)).await.unwrap().unwrap().role,
            Role::Admin
        );
    }

    #[tokio::test]
    async fn a_foreign_audience_yields_none_not_an_error() {
        // `Ok(None)`, not `Err`: with several providers configured the next one
        // must still get its turn, and the auth-failure counter is for genuine
        // provider faults.
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "a", "exp": future_exp(), "aud": "elsewhere" }),
        );
        assert!(matches!(p.authenticate(&bearer(&token)).await, Ok(None)));
    }

    #[test]
    fn audiences_default_to_the_client_id() {
        // The `aud` of an ID token issued to this client *is* the client_id, so
        // an operator who configures nothing still gets a real check.
        let cfg = OidcAuthConfig {
            name: "oidc".to_owned(),
            required: false,
            issuer_url: "https://idp.test".to_owned(),
            client_id: "my-client".to_owned(),
            client_secret: None,
            redirect_uri: None,
            frontend_url: String::new(),
            scopes: vec![],
            audiences: vec![],
            user_id_claim: "sub".to_owned(),
            role_claim: "role".to_owned(),
            role_mappings: HashMap::new(),
        };
        let resolved = if cfg.audiences.is_empty() {
            vec![cfg.client_id.clone()]
        } else {
            cfg.audiences.clone()
        };
        assert_eq!(resolved, vec!["my-client".to_owned()]);
    }

    // ── Discovery issuer ──────────────────────────────────────────────────────

    #[test]
    fn issuer_matches_ignores_only_a_trailing_slash() {
        assert!(issuer_matches("https://idp.test", "https://idp.test"));
        assert!(issuer_matches("https://idp.test/", "https://idp.test"));
        assert!(issuer_matches("https://idp.test", "https://idp.test/"));

        assert!(!issuer_matches("https://idp.test", "https://evil.test"));
        assert!(
            !issuer_matches("https://idp.test", "https://idp.test/realms/a"),
            "a path is not a trailing slash"
        );
        assert!(
            !issuer_matches("https://idp.test", "http://idp.test"),
            "scheme must match"
        );
    }

    #[tokio::test]
    async fn construction_fails_when_discovery_declares_a_different_issuer() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();

        // The document claims to be someone else — which, unchecked, would
        // define both the `iss` we go on to enforce and the JWKS we trust.
        let _discovery = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "issuer": "https://somewhere-else.example",
                    "jwks_uri": format!("{base}/jwks"),
                    "authorization_endpoint": format!("{base}/auth"),
                    "token_endpoint": format!("{base}/token"),
                })
                .to_string(),
            )
            .create_async()
            .await;

        let cfg = OidcAuthConfig {
            name: "oidc".to_owned(),
            required: false,
            issuer_url: base.clone(),
            client_id: "my-client".to_owned(),
            client_secret: None,
            redirect_uri: None,
            frontend_url: String::new(),
            scopes: vec!["openid".to_owned()],
            audiences: vec![],
            user_id_claim: "sub".to_owned(),
            role_claim: "role".to_owned(),
            role_mappings: HashMap::new(),
        };

        let Err(err) = OidcAuthProvider::new(&cfg).await else {
            panic!("a mismatched issuer must not produce a working provider");
        };
        let msg = err.to_string();
        assert!(msg.contains("somewhere-else.example"), "got: {msg}");
        assert!(
            msg.contains("issuer_url"),
            "the message names the fix: {msg}"
        );
    }

    // ── JWT validation errors ─────────────────────────────────────────────────

    #[tokio::test]
    async fn expired_token_returns_none() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "frank", "role": "admin", "exp": past_exp(), "aud": TEST_AUDIENCE }),
        );
        assert!(p.authenticate(&bearer(&token)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unknown_kid_returns_auth_error() {
        let p = default_provider();
        let token = signed_token(
            Some("unknown-key-id"),
            json!({ "sub": "grace", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let err = p.authenticate(&bearer(&token)).await.unwrap_err();
        assert!(matches!(err, CoreError::Auth(_)));
    }

    #[tokio::test]
    async fn token_without_kid_uses_first_jwk() {
        let p = default_provider();
        // No kid in header — falls back to jwks.keys[0]
        let token = signed_token(
            None,
            json!({ "sub": "henry", "role": "developer", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.role, Role::User);
    }

    // ── Identity metadata ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn auth_provider_name_defaults_to_oidc() {
        assert_eq!(default_provider().name(), "oidc");
    }

    #[tokio::test]
    async fn auth_provider_name_is_configurable() {
        let p = make_provider("authentik", "sub", "role", HashMap::new());
        assert_eq!(p.name(), "authentik");
    }

    #[tokio::test]
    async fn identity_auth_provider_reflects_configured_name() {
        let p = make_provider("oidc1", "sub", "role", HashMap::new());
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "iris", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.auth_provider.as_deref(), Some("oidc1"));
    }

    #[tokio::test]
    async fn array_role_claim_populates_groups_with_provider_name_prefix() {
        let p = default_provider(); // name="oidc", role_mappings: admin/developer/viewer
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "role": ["team-a", "team-b"], "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        // Neither "team-a" nor "team-b" is in role_mappings → prefixed with provider name
        assert!(id.groups.contains(&"oidc:team-a".to_owned()));
        assert!(id.groups.contains(&"oidc:team-b".to_owned()));
    }

    #[tokio::test]
    async fn named_provider_uses_its_name_as_prefix() {
        let p = make_provider("oidc2", "sub", "role", HashMap::new());
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "role": "team-a", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.groups, vec!["oidc2:team-a".to_owned()]);
    }

    #[tokio::test]
    async fn mapped_role_claim_values_have_no_prefix() {
        let p = default_provider(); // "admin" is in role_mappings
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "role": ["admin", "team-a"], "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert!(
            id.groups.contains(&"admin".to_owned()),
            "mapped value stored without prefix"
        );
        assert!(
            id.groups.contains(&"oidc:team-a".to_owned()),
            "unmapped value stored with provider name prefix"
        );
    }

    #[tokio::test]
    async fn string_role_claim_populates_single_group_with_prefix() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "role": "team-a", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.groups, vec!["oidc:team-a".to_owned()]);
    }

    #[tokio::test]
    async fn missing_role_claim_yields_empty_groups() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert!(id.groups.is_empty());
    }

    // ── lowercase bearer prefix ───────────────────────────────────────────────

    #[tokio::test]
    async fn lowercase_bearer_prefix_is_accepted() {
        let p = default_provider();
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "role": "admin", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let req = RawAuthRequest {
            headers: [("authorization".to_owned(), format!("bearer {token}"))].into(),
            query_params: Default::default(),
            source_ip: None,
        };
        let id = p.authenticate(&req).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Admin);
    }

    // ── OidcSsoFlow::authorization_url ────────────────────────────────────────

    fn test_flow() -> OidcSsoFlow {
        OidcSsoFlow {
            name: "oidc".to_owned(),
            client_id: "my-client".to_owned(),
            client_secret: None,
            redirect_uri: "https://app.example.com/callback".to_owned(),
            scopes: vec!["openid".to_owned(), "profile".to_owned()],
            authorization_endpoint: "https://idp.example.com/auth".to_owned(),
            token_endpoint: "https://idp.example.com/token".to_owned(),
            frontend_url: "https://app.example.com".to_owned(),
            user_id_claim: "sub".to_owned(),
            http: reqwest::Client::new(),
        }
    }

    #[test]
    fn authorization_url_contains_required_params() {
        let flow = test_flow();
        let url = flow.authorization_url("csrf-state-123", &PkceChallenge::generate());
        assert!(url.starts_with("https://idp.example.com/auth?"));
        assert!(url.contains("client_id=my-client"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=csrf-state-123"));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("scope="));
    }

    // ── PKCE ──────────────────────────────────────────────────────────────────

    #[test]
    fn authorization_url_carries_the_s256_challenge_and_nonce() {
        let flow = test_flow();
        let pkce = PkceChallenge::generate();
        let url = flow.authorization_url("state", &pkce);

        assert!(
            url.contains("code_challenge_method=S256"),
            "plain PKCE is not acceptable; the method must be pinned to S256"
        );
        assert!(url.contains(&format!(
            "code_challenge={}",
            percent_encode(&pkce.challenge)
        )));
        assert!(url.contains(&format!("nonce={}", percent_encode(&pkce.nonce))));
        assert!(
            !url.contains(&pkce.verifier),
            "the verifier is the secret half and must never leave the server"
        );
    }

    #[test]
    fn pkce_challenge_is_the_base64url_sha256_of_the_verifier() {
        let pkce = PkceChallenge::generate();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
        // RFC 7636 §4.1 allows 43–128 characters; 32 bytes base64url is 43.
        assert!((43..=128).contains(&pkce.verifier.len()));
        // base64url alphabet only — no padding, nothing needing escaping in a URL.
        assert!(pkce
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn each_challenge_is_unique() {
        let a = PkceChallenge::generate();
        let b = PkceChallenge::generate();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.challenge, b.challenge);
        assert_ne!(a.nonce, b.nonce);
    }

    #[tokio::test]
    async fn exchange_code_sends_the_verifier() {
        let mut server = mockito::Server::new_async().await;
        let token = server
            .mock("POST", "/token")
            .match_body(mockito::Matcher::UrlEncoded(
                "code_verifier".to_owned(),
                "the-verifier".to_owned(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"at"}"#)
            .create_async()
            .await;

        let mut flow = test_flow();
        flow.token_endpoint = format!("{}/token", server.url());
        flow.exchange_code("code", "the-verifier").await.unwrap();
        token.assert_async().await;
    }

    // ── sso_flow() accessor ───────────────────────────────────────────────────

    #[test]
    fn sso_flow_returns_none_when_not_configured() {
        let p = default_provider();
        assert!(p.sso_flow().is_none());
    }

    // ── OidcAuthProvider::new() + exchange_code + refresh (mockito) ───────────

    fn discovery_json(base_url: &str) -> String {
        serde_json::json!({
            "issuer": base_url,
            "jwks_uri": format!("{base_url}/jwks"),
            "authorization_endpoint": format!("{base_url}/auth"),
            "token_endpoint": format!("{base_url}/token"),
        })
        .to_string()
    }

    #[tokio::test]
    async fn new_bootstraps_provider_from_discovery_document() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();

        let _discovery = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(discovery_json(&base))
            .create_async()
            .await;

        let _jwks = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(TEST_JWKS_JSON)
            .create_async()
            .await;

        use batlehub_core::ports::OidcAuthConfig;
        let cfg = OidcAuthConfig {
            name: "test".to_owned(),
            required: false,
            issuer_url: base.clone(),
            client_id: "my-client".to_owned(),
            client_secret: None,
            redirect_uri: None,
            frontend_url: String::new(),
            scopes: vec!["openid".to_owned()],
            audiences: vec![TEST_AUDIENCE.to_owned()],
            user_id_claim: "sub".to_owned(),
            role_claim: "role".to_owned(),
            role_mappings: HashMap::new(),
        };

        let provider = OidcAuthProvider::new(&cfg)
            .await
            .expect("provider construction failed");
        assert!(provider.sso_flow().is_none());
    }

    #[tokio::test]
    async fn new_with_redirect_uri_creates_sso_flow() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();

        let _discovery = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(discovery_json(&base))
            .create_async()
            .await;

        let _jwks = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(TEST_JWKS_JSON)
            .create_async()
            .await;

        use batlehub_core::ports::OidcAuthConfig;
        let cfg = OidcAuthConfig {
            name: "oidc".to_owned(),
            required: false,
            issuer_url: base.clone(),
            client_id: "my-client".to_owned(),
            client_secret: Some("secret".to_owned()),
            redirect_uri: Some("https://app.example.com/callback".to_owned()),
            frontend_url: "https://app.example.com".to_owned(),
            scopes: vec!["openid".to_owned()],
            audiences: vec![TEST_AUDIENCE.to_owned()],
            user_id_claim: "sub".to_owned(),
            role_claim: "role".to_owned(),
            role_mappings: HashMap::new(),
        };

        let provider = OidcAuthProvider::new(&cfg)
            .await
            .expect("provider construction failed");
        let sso = provider
            .sso_flow()
            .expect("sso_flow should be Some with redirect_uri");
        let auth_url = sso.authorization_url("test-state", &PkceChallenge::generate());
        assert!(auth_url.contains("state=test-state"));
    }

    #[tokio::test]
    async fn exchange_code_sends_code_grant_request() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();

        let _discovery = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(discovery_json(&base))
            .create_async()
            .await;

        let _jwks = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(TEST_JWKS_JSON)
            .create_async()
            .await;

        let _token = server
            .mock("POST", "/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"at-123","refresh_token":"rt-xyz","expires_in":3600}"#)
            .create_async()
            .await;

        use batlehub_core::ports::OidcAuthConfig;
        let cfg = OidcAuthConfig {
            name: "oidc".to_owned(),
            required: false,
            issuer_url: base.clone(),
            client_id: "my-client".to_owned(),
            client_secret: Some("secret".to_owned()),
            redirect_uri: Some("https://app.example.com/callback".to_owned()),
            frontend_url: String::new(),
            scopes: vec!["openid".to_owned()],
            audiences: vec![TEST_AUDIENCE.to_owned()],
            user_id_claim: "sub".to_owned(),
            role_claim: "role".to_owned(),
            role_mappings: HashMap::new(),
        };

        let provider = OidcAuthProvider::new(&cfg).await.unwrap();
        let sso = provider.sso_flow().unwrap();
        let tokens = sso
            .exchange_code("auth-code-abc", "verifier")
            .await
            .unwrap();
        assert_eq!(tokens.access_token, "at-123");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-xyz"));
        assert_eq!(tokens.expires_in, Some(3600));
    }

    #[tokio::test]
    async fn refresh_sends_refresh_token_grant_request() {
        let mut server = mockito::Server::new_async().await;
        let base = server.url();

        let _discovery = server
            .mock("GET", "/.well-known/openid-configuration")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(discovery_json(&base))
            .create_async()
            .await;

        let _jwks = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(TEST_JWKS_JSON)
            .create_async()
            .await;

        let _token = server
            .mock("POST", "/token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"new-at","expires_in":1800}"#)
            .create_async()
            .await;

        use batlehub_core::ports::OidcAuthConfig;
        let cfg = OidcAuthConfig {
            name: "oidc".to_owned(),
            required: false,
            issuer_url: base,
            client_id: "my-client".to_owned(),
            client_secret: None,
            redirect_uri: Some("https://app.example.com/callback".to_owned()),
            frontend_url: String::new(),
            scopes: vec!["openid".to_owned()],
            audiences: vec![TEST_AUDIENCE.to_owned()],
            user_id_claim: "sub".to_owned(),
            role_claim: "role".to_owned(),
            role_mappings: HashMap::new(),
        };

        let provider = OidcAuthProvider::new(&cfg).await.unwrap();
        let sso = provider.sso_flow().unwrap();
        let tokens = sso.refresh("old-refresh-token").await.unwrap();
        assert_eq!(tokens.access_token, "new-at");
        assert_eq!(tokens.expires_in, Some(1800));
    }

    // ── JWKS cache refresh path ───────────────────────────────────────────────

    #[tokio::test]
    async fn get_decoding_key_refreshes_stale_jwks_cache() {
        let mut server = mockito::Server::new_async().await;
        let jwks_url = format!("{}/jwks", server.url());

        let _m = server
            .mock("GET", "/jwks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(TEST_JWKS_JSON)
            .create_async()
            .await;

        // Create a provider with a stale JWKS cache (older than JWKS_MIN_REFRESH).
        let p = OidcAuthProvider {
            name: "oidc".to_owned(),
            issuer: String::new(),
            user_id_claim: "sub".to_owned(),
            role_claim: "role".to_owned(),
            role_mappings: HashMap::new(),
            audiences: vec![TEST_AUDIENCE.to_owned()],
            http: reqwest::Client::new(),
            jwks_uri: jwks_url,
            cache: Arc::new(RwLock::new(JwksCache {
                keys: serde_json::from_str::<JwkSet>(r#"{"keys":[]}"#).unwrap(),
                fetched_at: Instant::now() - Duration::from_secs(301),
            })),
            sso: None,
        };

        // Token signed with test-kid — not in the stale empty cache but in the fresh JWKS.
        let token = signed_token(
            Some("test-kid"),
            json!({ "sub": "alice", "exp": future_exp(), "aud": TEST_AUDIENCE }),
        );
        let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.user_id.as_deref(), Some("alice"));
    }
}
