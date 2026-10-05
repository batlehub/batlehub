use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use batlehub_core::ports::KubernetesAuthConfig;
use batlehub_core::{
    entities::{Identity, Role},
    error::CoreError,
    ports::{AuthProvider, RawAuthRequest},
};

const IN_CLUSTER_CA: &str = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt";
const IN_CLUSTER_TOKEN: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

/// Whether `token` has the shape of a JWT — three dot-separated, non-empty parts.
///
/// Every Kubernetes service account token is a JWT, so anything else cannot be
/// one and must not be sent to the API server. This matters beyond saving a
/// round trip: `authenticate` posts the caller's bearer token verbatim in a
/// TokenReview body, so without this guard a BatleHub personal access token —
/// which reaches this provider first, since `UserTokenAuthProvider` is appended
/// last — would be handed to the cluster control plane, a system that has no
/// business seeing it and may well audit-log the request body.
///
/// The mirror image of the `raw.contains('.')` short-circuit in
/// `user_token.rs`, which keeps JWTs out of the PAT table lookup.
fn looks_like_a_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    let ok = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty()
    );
    ok && parts.next().is_none()
}

/// The two claims that decide whether a JWT is even *addressed to us*.
///
/// [`looks_like_a_jwt`] keeps a personal access token out of a TokenReview body,
/// and stops there: an OIDC ID token and a GitHub Actions token are both
/// perfectly JWT-shaped. With `type = "kubernetes"` ordered before
/// `type = "oidc"` — the natural order for a cluster deployment — every browser
/// request's ID token was therefore POSTed verbatim to the cluster API server,
/// and because only *successes* are cached it was re-shipped on every single
/// request, which is precisely the amplification [`TOKENREVIEW_CACHE_TTL`]
/// exists to prevent.
struct PeekedClaims {
    iss: Option<String>,
    aud: Vec<String>,
}

/// Read `iss`/`aud` out of a JWT payload **without verifying anything**.
///
/// Safe because nothing is granted on the result: it can only make this provider
/// decline to ask the API server about a token. A forged payload buys an
/// attacker a TokenReview that rejects them, which is what they get by sending
/// no claims at all.
fn peek_claims(token: &str) -> Option<PeekedClaims> {
    use base64::Engine as _;

    let payload = token.split('.').nth(1)?;
    // JWT mandates unpadded base64url; tolerate padding rather than treat a
    // padded token as unreadable.
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&bytes).ok()?;

    // `aud` is a string or an array of strings (RFC 7519 §4.1.3).
    let aud = match claims.get("aud") {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => vec![],
    };
    Some(PeekedClaims {
        iss: claims
            .get("iss")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        aud,
    })
}

// ── TokenReview wire types ────────────────────────────────────────────────────

#[derive(Serialize)]
struct TokenReviewRequest {
    #[serde(rename = "apiVersion")]
    api_version: &'static str,
    kind: &'static str,
    spec: TokenReviewSpec,
}

#[derive(Serialize)]
struct TokenReviewSpec {
    token: String,
    audiences: Vec<String>,
}

#[derive(Deserialize)]
struct TokenReviewResponse {
    status: TokenReviewStatus,
}

#[derive(Deserialize, Default)]
struct TokenReviewStatus {
    #[serde(default)]
    authenticated: bool,
    #[serde(default)]
    user: Option<UserInfo>,
    /// The audiences the authenticator actually validated the token against —
    /// the intersection of `spec.audiences` and the token's own `aud`.
    ///
    /// Checked, not trusted-by-omission: see `audiences_are_confirmed`.
    #[serde(default)]
    audiences: Vec<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    username: String,
    #[serde(default)]
    groups: Vec<String>,
}

// ── Provider ──────────────────────────────────────────────────────────────────

/// How long a successful TokenReview verdict is reused.
///
/// Without this, every proxied request costs one API server round trip plus a
/// disk read — which turns BatleHub into an amplifier aimed at the cluster
/// control plane, and makes the API server a hard dependency of every artifact
/// download. Well under the lifetime of a projected token (an hour is typical),
/// so a revoked service account still loses access promptly.
const TOKENREVIEW_CACHE_TTL: Duration = Duration::from_secs(60);

/// How long a *rejection* is reused.
///
/// Rejections used not to be cached at all, on the grounds that repeating one is
/// cheap and caching it would delay a newly-granted service account by a minute.
/// The first half is only true per *token*: a client that keeps presenting the
/// same rejected credential — which is what a misconfigured CI job and a browser
/// session both do — put one TokenReview on the API server per request, with no
/// ceiling. The answer is not "do not cache" but "cache briefly": ten seconds
/// takes the amplification factor to at most one review per token per ten
/// seconds, and is short enough that a RoleBinding landing mid-CI-run is not
/// something anyone notices.
const TOKENREVIEW_REJECT_TTL: Duration = Duration::from_secs(10);

/// What a TokenReview concluded about a token, as cached.
enum Verdict {
    Granted(Identity),
    Refused,
}

struct CachedReview {
    verdict: Verdict,
    at: Instant,
}

impl CachedReview {
    fn ttl(&self) -> Duration {
        match self.verdict {
            Verdict::Granted(_) => TOKENREVIEW_CACHE_TTL,
            Verdict::Refused => TOKENREVIEW_REJECT_TTL,
        }
    }
}

pub struct KubernetesAuthProvider {
    name: String,
    http: reqwest::Client,
    tokenreview_url: String,
    self_token_path: String,
    audiences: Vec<String>,
    /// Accepted token issuers; empty means "any" — see `KubernetesAuthConfig`.
    issuers: Vec<String>,
    role_mappings: HashMap<String, Role>,
    /// Keyed by SHA-256 of the presented token, so the credential itself is not
    /// held in memory beyond the request that carried it.
    review_cache: Mutex<HashMap<String, CachedReview>>,
}

impl KubernetesAuthProvider {
    pub async fn new(cfg: &KubernetesAuthConfig) -> anyhow::Result<Self> {
        let ca_cert_path = cfg.ca_cert_path.as_deref().unwrap_or(IN_CLUSTER_CA);
        let ca_bytes = tokio::fs::read(ca_cert_path).await.map_err(|e| {
            anyhow::anyhow!("reading Kubernetes CA cert from '{ca_cert_path}': {e}")
        })?;
        let ca_cert = reqwest::Certificate::from_pem(&ca_bytes)
            .map_err(|e| anyhow::anyhow!("parsing Kubernetes CA cert: {e}"))?;

        let http = reqwest::Client::builder()
            .add_root_certificate(ca_cert)
            .build()
            .map_err(|e| anyhow::anyhow!("building HTTP client for Kubernetes auth: {e}"))?;

        let api_server = cfg.api_server.clone().unwrap_or_else(|| {
            let host = std::env::var("KUBERNETES_SERVICE_HOST")
                .unwrap_or_else(|_| "kubernetes.default.svc".to_owned());
            let port =
                std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".to_owned());
            format!("https://{host}:{port}")
        });

        let self_token_path = cfg
            .token_path
            .clone()
            .unwrap_or_else(|| IN_CLUSTER_TOKEN.to_owned());

        // Verify the file is readable on startup so misconfiguration fails fast.
        tokio::fs::read_to_string(&self_token_path)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "reading batlehub service account token from '{self_token_path}': {e}"
                )
            })?;

        let audiences = if cfg.audiences.is_empty() {
            vec!["batlehub".to_owned()]
        } else {
            cfg.audiences.clone()
        };

        let role_mappings = cfg
            .role_mappings
            .iter()
            .map(|(k, v)| {
                let role = v
                    .parse::<Role>()
                    .map_err(|e| anyhow::anyhow!("role_mappings.{k}: {e}"))?;
                Ok((k.clone(), role))
            })
            .collect::<anyhow::Result<HashMap<String, Role>>>()?;

        Ok(Self {
            name: cfg.name.clone(),
            http,
            tokenreview_url: format!("{api_server}/apis/authentication.k8s.io/v1/tokenreviews"),
            self_token_path,
            audiences,
            issuers: cfg.issuers.clone(),
            role_mappings,
            review_cache: Mutex::new(HashMap::new()),
        })
    }

    fn resolve_role(&self, username: &str, groups: &[String]) -> Role {
        // Check username (most specific) then groups. Take the highest role found.
        std::iter::once(username)
            .chain(groups.iter().map(String::as_str))
            .filter_map(|key| self.role_mappings.get(key))
            .cloned()
            .max()
            .unwrap_or(Role::Anonymous)
    }

    /// Whether the API server confirmed the token is bound to an audience we
    /// asked for.
    ///
    /// `spec.audiences` asks the authenticator to check the binding;
    /// `status.audiences` is how it reports back which of them it actually
    /// validated. A real API server echoes a non-empty intersection here, so an
    /// empty list means the authenticator ignored the request — and a token we
    /// cannot confirm is bound to us is exactly the default service account
    /// token that every pod in the cluster carries, whose audience is the API
    /// server rather than BatleHub.
    ///
    /// So: reject unless the intersection is non-empty. This is stricter than
    /// the reference webhook authenticator in `k8s.io/apiserver`, which falls
    /// back to a configured set of implicit audiences when the response is
    /// empty. We are the relying party, not the API server; we have no implicit
    /// audience to fall back to, and silently accepting is the wrong default.
    fn audiences_are_confirmed(&self, confirmed: &[String]) -> bool {
        confirmed.iter().any(|a| self.audiences.contains(a))
    }

    /// Whether a token's own claims say it could be meant for this provider.
    ///
    /// Decided locally, before any round trip, and deliberately in terms of the
    /// same two properties the API server is asked about:
    ///
    /// - **issuer** — checked only when `issuers` is configured, because there is
    ///   no way to guess a cluster's issuer URL (it is `kubernetes.default.svc`
    ///   in one deployment and an S3-hosted OIDC document in the next);
    /// - **audience** — always. A projected token minted for this server carries
    ///   `aud: ["batlehub"]`; a browser's ID token carries the OIDC client id and
    ///   the cluster's default service account token carries the API server's own
    ///   audience. This rejects exactly what `audiences_are_confirmed` would
    ///   reject after the round trip — `status.audiences` is the intersection of
    ///   `spec.audiences` and the token's `aud`, so a token with no audience in
    ///   common could never come back confirmed — which is what makes skipping
    ///   the call safe rather than merely cheap.
    ///
    /// A token whose payload cannot be read at all is refused here too: it is not
    /// something the API server could authenticate either.
    fn token_may_be_ours(&self, token: &str) -> bool {
        let Some(claims) = peek_claims(token) else {
            return false;
        };
        if !self.issuers.is_empty() {
            let issued_by_us = claims
                .iss
                .as_deref()
                .is_some_and(|iss| self.issuers.iter().any(|known| known == iss));
            if !issued_by_us {
                return false;
            }
        }
        claims.aud.iter().any(|a| self.audiences.contains(a))
    }

    /// The verdict a recent TokenReview produced for this token, if still fresh.
    ///
    /// Sweeps expired entries as it looks, so a churn of short-lived service
    /// account tokens cannot grow the map without bound. Each entry expires on
    /// its own kind's TTL — see [`TOKENREVIEW_REJECT_TTL`].
    fn cached_review(&self, token_hash: &str) -> Option<Verdict> {
        let now = Instant::now();
        let mut cache = self.review_cache.lock().expect("tokenreview cache mutex");
        cache.retain(|_, entry| now.duration_since(entry.at) < entry.ttl());
        cache.get(token_hash).map(|e| match &e.verdict {
            Verdict::Granted(identity) => Verdict::Granted(identity.clone()),
            Verdict::Refused => Verdict::Refused,
        })
    }

    fn cache_review(&self, token_hash: String, verdict: Verdict) {
        self.review_cache
            .lock()
            .expect("tokenreview cache mutex")
            .insert(
                token_hash,
                CachedReview {
                    verdict,
                    at: Instant::now(),
                },
            );
    }

    fn resolve_groups(&self, k8s_groups: &[String]) -> Vec<String> {
        // Groups in role_mappings are known/configured — keep them as-is.
        // Unmapped groups are prefixed with the provider name to avoid cross-provider collisions.
        k8s_groups
            .iter()
            .map(|g| {
                if self.role_mappings.contains_key(g) {
                    g.clone()
                } else {
                    format!("{}:{g}", self.name)
                }
            })
            .collect()
    }
}

#[async_trait]
impl AuthProvider for KubernetesAuthProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn authenticate(&self, req: &RawAuthRequest) -> Result<Option<Identity>, CoreError> {
        let Some(token) = req.bearer_token() else {
            return Ok(None);
        };

        // Never forward a credential that cannot be a service account token to
        // the API server — see `looks_like_a_jwt`, then `token_may_be_ours` for
        // the JWTs that are not ours either. Both run before the cache: a token
        // this provider will never ask about does not deserve an entry in it.
        if !looks_like_a_jwt(token) || !self.token_may_be_ours(token) {
            return Ok(None);
        }

        // Hashed, not stored raw: the cache outlives the request, the credential
        // should not.
        let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
        match self.cached_review(&token_hash) {
            Some(Verdict::Granted(identity)) => return Ok(Some(identity)),
            Some(Verdict::Refused) => return Ok(None),
            None => {}
        }

        // Re-read the service account token each call — Kubernetes rotates it.
        let self_token = tokio::fs::read_to_string(&self.self_token_path)
            .await
            .map_err(|e| CoreError::Auth(format!("reading service account token: {e}")))?;

        let body = TokenReviewRequest {
            api_version: "authentication.k8s.io/v1",
            kind: "TokenReview",
            spec: TokenReviewSpec {
                token: token.to_owned(),
                audiences: self.audiences.clone(),
            },
        };

        let resp: TokenReviewResponse = self
            .http
            .post(&self.tokenreview_url)
            .bearer_auth(self_token.trim())
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Auth(format!("Kubernetes TokenReview request failed: {e}")))?
            .json()
            .await
            .map_err(|e| {
                CoreError::Auth(format!("parsing Kubernetes TokenReview response: {e}"))
            })?;

        if !resp.status.authenticated {
            // Not a valid k8s token — let other providers have a turn, and
            // remember the "no" briefly so a client that keeps presenting it
            // cannot turn one request into one TokenReview each.
            self.cache_review(token_hash, Verdict::Refused);
            return Ok(None);
        }

        if !self.audiences_are_confirmed(&resp.status.audiences) {
            tracing::warn!(
                provider = %self.name,
                requested = ?self.audiences,
                confirmed = ?resp.status.audiences,
                "TokenReview authenticated a token the API server did not confirm \
                 is bound to a requested audience — rejecting"
            );
            self.cache_review(token_hash, Verdict::Refused);
            return Ok(None);
        }

        let user = resp.status.user.unwrap_or(UserInfo {
            username: String::new(),
            groups: vec![],
        });

        let role = self.resolve_role(&user.username, &user.groups);
        let groups = self.resolve_groups(&user.groups);

        let identity = Identity {
            user_id: Some(user.username),
            role,
            auth_provider: Some(self.name.clone()),
            groups,
        };
        self.cache_review(token_hash, Verdict::Granted(identity.clone()));
        Ok(Some(identity))
    }
}

/// Test-only constructor that skips TLS setup and filesystem validation.
#[cfg(test)]
impl KubernetesAuthProvider {
    fn for_testing(
        http: reqwest::Client,
        tokenreview_url: impl Into<String>,
        self_token_path: impl Into<String>,
        audiences: Vec<String>,
        role_mappings: HashMap<String, Role>,
    ) -> Self {
        Self::for_testing_named(
            "kubernetes",
            http,
            tokenreview_url,
            self_token_path,
            audiences,
            role_mappings,
        )
    }

    fn for_testing_named(
        name: impl Into<String>,
        http: reqwest::Client,
        tokenreview_url: impl Into<String>,
        self_token_path: impl Into<String>,
        audiences: Vec<String>,
        role_mappings: HashMap<String, Role>,
    ) -> Self {
        Self {
            name: name.into(),
            http,
            tokenreview_url: tokenreview_url.into(),
            self_token_path: self_token_path.into(),
            audiences,
            issuers: vec![],
            role_mappings,
            review_cache: Mutex::new(HashMap::new()),
        }
    }

    fn with_issuers(mut self, issuers: &[&str]) -> Self {
        self.issuers = issuers.iter().map(|s| (*s).to_owned()).collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;
    use std::collections::HashMap;

    pub(super) struct TempFile(pub String);
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    pub(super) async fn write_temp_token(content: &str) -> TempFile {
        let path = format!(
            "/tmp/k8s-test-token-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        tokio::fs::write(&path, content).await.unwrap();
        TempFile(path)
    }

    fn default_mappings() -> HashMap<String, Role> {
        [
            (
                "system:serviceaccount:prod:ci-deployer".to_owned(),
                Role::Admin,
            ),
            ("system:serviceaccounts:dev".to_owned(), Role::User),
            ("system:serviceaccounts".to_owned(), Role::Anonymous),
        ]
        .into()
    }

    pub(super) fn make_provider(server: &Server, token_path: &str) -> KubernetesAuthProvider {
        KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            format!(
                "{}/apis/authentication.k8s.io/v1/tokenreviews",
                server.url()
            ),
            token_path.to_owned(),
            vec!["batlehub".to_owned()],
            default_mappings(),
        )
    }

    pub(super) fn bearer(token: &str) -> RawAuthRequest {
        RawAuthRequest {
            headers: [("authorization".to_owned(), format!("Bearer {token}"))].into(),
            query_params: Default::default(),
            source_ip: None,
        }
    }

    /// An unsigned JWT carrying `claims`.
    ///
    /// The signature never matters — the API server is mocked and it is the one
    /// that would verify it. The *claims* do: `looks_like_a_jwt` and
    /// `token_may_be_ours` both decide from them whether the credential is
    /// offered to the API server at all.
    pub(super) fn jwt(claims: serde_json::Value) -> String {
        use base64::Engine as _;
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        format!(
            "{}.{}.{}",
            b64(br#"{"alg":"RS256","typ":"JWT"}"#),
            b64(&serde_json::to_vec(&claims).unwrap()),
            b64(b"not-a-real-signature")
        )
    }

    pub(super) const CLUSTER_ISSUER: &str = "https://kubernetes.default.svc.cluster.local";

    /// A projected service account token bound to this server's audience.
    pub(super) fn sa_token() -> String {
        jwt(serde_json::json!({
            "iss": CLUSTER_ISSUER,
            "aud": ["batlehub"],
            "sub": "system:serviceaccount:prod:ci-deployer",
        }))
    }

    /// A TokenReview response that authenticates `username` for our audience.
    pub(super) fn authenticated_body(username: &str) -> String {
        format!(
            r#"{{"status":{{"authenticated":true,"audiences":["batlehub"],"user":{{"username":"{username}","groups":[]}}}}}}"#
        )
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
        let server = Server::new_async().await;
        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        assert!(p.authenticate(&no_auth()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn basic_auth_header_returns_none() {
        let server = Server::new_async().await;
        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let req = RawAuthRequest {
            headers: [("authorization".to_owned(), "Basic dXNlcjpwYXNz".to_owned())].into(),
            query_params: Default::default(),
            source_ip: None,
        };
        assert!(p.authenticate(&req).await.unwrap().is_none());
    }

    // ── Non-JWT credentials never reach the API server ────────────────────────
    // `UserTokenAuthProvider` is appended after every configured provider, so in
    // an in-cluster deployment a BatleHub personal access token passes through
    // here first. It used to be posted verbatim to the cluster control plane in
    // a TokenReview body before its own provider ever saw it.

    #[tokio::test]
    async fn a_personal_access_token_is_never_sent_to_the_api_server() {
        let mut server = Server::new_async().await;
        // Any call to the mock is a failure: `expect(0)` makes that assertable.
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .expect(0)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":false}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);

        // 64 hex characters — the exact shape `generate_token` produces.
        let pat = "a".repeat(64);
        assert!(p.authenticate(&bearer(&pat)).await.unwrap().is_none());
        review.assert_async().await;
    }

    // ── JWTs that are not ours never reach the API server either ──────────────
    // `looks_like_a_jwt` only keeps *non*-JWTs out. An OIDC ID token is a JWT,
    // and with `type = "kubernetes"` ordered before `type = "oidc"` — the
    // natural order in a cluster — every browser request's ID token was POSTed
    // verbatim to the cluster control plane, on every single request, since only
    // successes were cached.

    #[tokio::test]
    async fn an_oidc_id_token_is_never_sent_to_the_api_server() {
        let mut server = Server::new_async().await;
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .expect(0)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":false}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);

        // What a browser session carries: a real JWT, audienced to the console's
        // OIDC client rather than to this server.
        let id_token = jwt(serde_json::json!({
            "iss": "https://login.example.com/",
            "aud": "batlehub-console",
            "sub": "user@example.com",
        }));
        assert!(p.authenticate(&bearer(&id_token)).await.unwrap().is_none());
        review.assert_async().await;
    }

    /// The audience check is not a guess about what the API server would say —
    /// it is the same one, moved earlier. `status.audiences` is the intersection
    /// of `spec.audiences` and the token's own `aud`, so a token with nothing in
    /// common could never come back confirmed.
    #[tokio::test]
    async fn a_token_bound_to_no_audience_of_ours_is_not_worth_a_round_trip() {
        let mut server = Server::new_async().await;
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .expect(0)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);

        for claims in [
            // The default service account token every pod carries: bound to the
            // API server, not to us.
            serde_json::json!({"iss": CLUSTER_ISSUER, "aud": ["https://kubernetes.default.svc"]}),
            // A legacy, unbound token: no audience at all.
            serde_json::json!({"iss": CLUSTER_ISSUER, "sub": "system:serviceaccount:x:y"}),
            // Not a readable payload — not something the API server could
            // authenticate either.
            serde_json::json!(null),
        ] {
            let token = jwt(claims);
            assert!(p.authenticate(&bearer(&token)).await.unwrap().is_none());
        }
        review.assert_async().await;
    }

    #[tokio::test]
    async fn a_configured_issuer_narrows_further_and_still_admits_its_own_tokens() {
        let mut server = Server::new_async().await;
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            // Exactly one: the token from the configured issuer. The other one
            // is refused before any request is made.
            .expect(1)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(authenticated_body("system:serviceaccount:prod:ci-deployer"))
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0).with_issuers(&[CLUSTER_ISSUER]);

        // Another cluster's token, bound to the same audience name — the case
        // the audience check alone cannot see.
        let foreign = jwt(serde_json::json!({
            "iss": "https://oidc.eks.eu-west-1.amazonaws.com/id/OTHER",
            "aud": ["batlehub"],
            "sub": "system:serviceaccount:prod:ci-deployer",
        }));
        assert!(p.authenticate(&bearer(&foreign)).await.unwrap().is_none());

        let id = p.authenticate(&bearer(&sa_token())).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Admin);
        review.assert_async().await;
    }

    #[test]
    fn peek_claims_reads_both_audience_spellings_and_nothing_else() {
        let one = peek_claims(&jwt(serde_json::json!({"aud": "batlehub"}))).unwrap();
        assert_eq!(one.aud, vec!["batlehub".to_owned()]);
        assert_eq!(one.iss, None);

        let many = peek_claims(&jwt(serde_json::json!({"iss": "x", "aud": ["a", "b"]}))).unwrap();
        assert_eq!(many.aud, vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(many.iss.as_deref(), Some("x"));

        assert!(peek_claims("not.a.jwt").is_none());
        assert!(peek_claims("only-one-part").is_none());
    }

    #[test]
    fn looks_like_a_jwt_accepts_only_three_non_empty_parts() {
        assert!(looks_like_a_jwt("header.payload.signature"));

        assert!(!looks_like_a_jwt(&"a".repeat(64)), "a PAT is not a JWT");
        assert!(!looks_like_a_jwt(""), "empty");
        assert!(!looks_like_a_jwt("only.two"), "two parts");
        assert!(!looks_like_a_jwt("a.b.c.d"), "four parts");
        assert!(!looks_like_a_jwt(".b.c"), "empty header");
        assert!(!looks_like_a_jwt("a..c"), "empty payload");
        assert!(
            !looks_like_a_jwt("a.b."),
            "empty signature — an alg=none token is not a service account token"
        );
    }

    // ── status.audiences is checked, not assumed ──────────────────────────────
    // `spec.audiences` only *asks* the authenticator for a bound-token check.
    // An authenticator that ignores it answers `authenticated: true` with no
    // audiences, and the token in hand is then whatever the caller had — for
    // every pod in the cluster, the default service account token bound to the
    // API server rather than to BatleHub.

    #[tokio::test]
    async fn authenticated_without_confirmed_audiences_is_rejected() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            // Authenticated, correctly mapped username — and no `audiences`.
            .with_body(r#"{"status":{"authenticated":true,"user":{"username":"system:serviceaccount:prod:ci-deployer","groups":[]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        assert!(
            p.authenticate(&bearer(&sa_token()))
                .await
                .unwrap()
                .is_none(),
            "an unconfirmed audience must not grant the admin role this username maps to"
        );
    }

    #[tokio::test]
    async fn authenticated_for_a_different_audience_is_rejected() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            // Valid token, bound to the API server rather than to us.
            .with_body(r#"{"status":{"authenticated":true,"audiences":["https://kubernetes.default.svc"],"user":{"username":"system:serviceaccount:prod:ci-deployer","groups":[]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        assert!(p
            .authenticate(&bearer(&sa_token()))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn one_matching_audience_among_several_is_enough() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":true,"audiences":["other","batlehub"],"user":{"username":"system:serviceaccount:prod:ci-deployer","groups":[]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let id = p.authenticate(&bearer(&sa_token())).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Admin);
    }

    #[test]
    fn audiences_are_confirmed_requires_a_non_empty_intersection() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec!["batlehub".to_owned(), "batlehub-staging".to_owned()],
            HashMap::new(),
        );
        assert!(p.audiences_are_confirmed(&["batlehub".to_owned()]));
        assert!(p.audiences_are_confirmed(&["batlehub-staging".to_owned()]));
        assert!(!p.audiences_are_confirmed(&[]), "no confirmation at all");
        assert!(!p.audiences_are_confirmed(&["something-else".to_owned()]));
    }

    // ── resolve_role ──────────────────────────────────────────────────────────

    #[test]
    fn username_alone_maps_to_admin() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        assert_eq!(
            p.resolve_role("system:serviceaccount:prod:ci-deployer", &[]),
            Role::Admin
        );
    }

    #[test]
    fn group_maps_to_user_when_username_unmapped() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        let groups = vec!["system:serviceaccounts:dev".to_owned()];
        assert_eq!(
            p.resolve_role("system:serviceaccount:staging:other", &groups),
            Role::User
        );
    }

    #[test]
    fn highest_role_wins_across_multiple_groups() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        let groups = vec![
            "system:serviceaccounts".to_owned(),     // → Anonymous
            "system:serviceaccounts:dev".to_owned(), // → User
        ];
        // User > Anonymous, so User wins
        assert_eq!(p.resolve_role("unmapped-user", &groups), Role::User);
    }

    #[test]
    fn username_beats_group_when_username_has_higher_role() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        // username → Admin, group → User: Admin should win
        let groups = vec!["system:serviceaccounts:dev".to_owned()];
        assert_eq!(
            p.resolve_role("system:serviceaccount:prod:ci-deployer", &groups),
            Role::Admin
        );
    }

    #[test]
    fn no_match_at_all_returns_anonymous() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        let groups = vec!["system:authenticated".to_owned()];
        assert_eq!(p.resolve_role("unknown-user", &groups), Role::Anonymous);
    }

    #[test]
    fn empty_mappings_always_returns_anonymous() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            HashMap::new(),
        );
        let groups = vec!["system:serviceaccounts:prod".to_owned()];
        assert_eq!(
            p.resolve_role("system:serviceaccount:prod:ci-deployer", &groups),
            Role::Anonymous
        );
    }

    // ── Full authenticate flow ────────────────────────────────────────────────

    #[tokio::test]
    async fn authenticated_token_username_maps_to_admin() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":true,"audiences":["batlehub"],"user":{"username":"system:serviceaccount:prod:ci-deployer","groups":["system:serviceaccounts","system:serviceaccounts:prod","system:authenticated"]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let id = p.authenticate(&bearer(&sa_token())).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Admin);
        assert_eq!(
            id.user_id.as_deref(),
            Some("system:serviceaccount:prod:ci-deployer")
        );
        assert_eq!(id.auth_provider.as_deref(), Some("kubernetes"));
    }

    #[tokio::test]
    async fn authenticated_token_group_maps_to_user() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":true,"audiences":["batlehub"],"user":{"username":"system:serviceaccount:dev:my-app","groups":["system:serviceaccounts:dev","system:serviceaccounts","system:authenticated"]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let id = p.authenticate(&bearer(&sa_token())).await.unwrap().unwrap();
        assert_eq!(id.role, Role::User);
    }

    #[tokio::test]
    async fn unauthenticated_response_returns_none() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":false}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        assert!(p
            .authenticate(&bearer(&sa_token()))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn unmapped_service_account_defaults_to_anonymous() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":true,"audiences":["batlehub"],"user":{"username":"system:serviceaccount:unknown-ns:pod","groups":["system:authenticated"]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let id = p.authenticate(&bearer(&sa_token())).await.unwrap().unwrap();
        assert_eq!(id.role, Role::Anonymous);
    }

    #[tokio::test]
    async fn k8s_api_server_error_propagates_as_auth_error() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(500)
            .with_body("Internal Server Error")
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        assert!(p.authenticate(&bearer(&sa_token())).await.is_err());
    }

    #[tokio::test]
    async fn tokenreview_request_sends_correct_audience() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"spec":{"audiences":["batlehub"]}}),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":false}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let _ = p.authenticate(&bearer(&sa_token())).await;
        _m.assert_async().await;
    }

    #[tokio::test]
    async fn provider_name_defaults_to_kubernetes() {
        let server = Server::new_async().await;
        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        assert_eq!(p.name(), "kubernetes");
    }

    #[test]
    fn provider_name_is_configurable() {
        let p = KubernetesAuthProvider::for_testing_named(
            "k8s-prod",
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            HashMap::new(),
        );
        assert_eq!(p.name(), "k8s-prod");
    }

    // ── resolve_groups ────────────────────────────────────────────────────────

    #[test]
    fn mapped_group_stored_without_prefix() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        // "system:serviceaccounts:dev" is in default_mappings → no prefix
        let groups = p.resolve_groups(&["system:serviceaccounts:dev".to_owned()]);
        assert_eq!(groups, vec!["system:serviceaccounts:dev".to_owned()]);
    }

    #[test]
    fn unmapped_group_gets_provider_name_prefix() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        // "system:authenticated" is not in role_mappings → prefixed with provider name
        let groups = p.resolve_groups(&["system:authenticated".to_owned()]);
        assert_eq!(groups, vec!["kubernetes:system:authenticated".to_owned()]);
    }

    #[test]
    fn named_provider_uses_its_name_as_prefix() {
        let p = KubernetesAuthProvider::for_testing_named(
            "k8s-prod",
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        let groups = p.resolve_groups(&["team-a".to_owned()]);
        assert_eq!(groups, vec!["k8s-prod:team-a".to_owned()]);
    }

    #[test]
    fn mixed_groups_prefix_only_unmapped() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        let raw = vec![
            "system:serviceaccounts:dev".to_owned(), // mapped → no prefix
            "system:serviceaccounts".to_owned(),     // mapped → no prefix
            "system:authenticated".to_owned(),       // unmapped → kubernetes:
            "team-a".to_owned(),                     // unmapped → kubernetes:
        ];
        let groups = p.resolve_groups(&raw);
        assert!(groups.contains(&"system:serviceaccounts:dev".to_owned()));
        assert!(groups.contains(&"system:serviceaccounts".to_owned()));
        assert!(groups.contains(&"kubernetes:system:authenticated".to_owned()));
        assert!(groups.contains(&"kubernetes:team-a".to_owned()));
        assert!(
            !groups.contains(&"team-a".to_owned()),
            "unprefixed team-a should not exist"
        );
    }

    #[test]
    fn empty_groups_yields_empty_result() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec![],
            default_mappings(),
        );
        assert!(p.resolve_groups(&[]).is_empty());
    }

    // ── Full authenticate flow — groups field ─────────────────────────────────

    #[tokio::test]
    async fn authenticate_populates_identity_groups() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            // Groups: one mapped ("system:serviceaccounts:dev"), one unmapped ("team-a")
            .with_body(r#"{"status":{"authenticated":true,"audiences":["batlehub"],"user":{"username":"system:serviceaccount:dev:my-app","groups":["system:serviceaccounts:dev","team-a","system:authenticated"]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let id = p.authenticate(&bearer(&sa_token())).await.unwrap().unwrap();

        assert!(
            id.groups.contains(&"system:serviceaccounts:dev".to_owned()),
            "mapped group stored without prefix"
        );
        assert!(
            id.groups.contains(&"kubernetes:team-a".to_owned()),
            "unmapped group stored with provider name prefix"
        );
        assert!(
            id.groups
                .contains(&"kubernetes:system:authenticated".to_owned()),
            "standard k8s group stored with provider name prefix"
        );
        assert!(
            !id.groups.contains(&"team-a".to_owned()),
            "unprefixed unmapped group must not exist"
        );
    }

    #[tokio::test]
    async fn authenticate_groups_empty_when_tokenreview_returns_no_groups() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":true,"audiences":["batlehub"],"user":{"username":"system:serviceaccount:prod:ci-deployer","groups":[]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let id = p.authenticate(&bearer(&sa_token())).await.unwrap().unwrap();
        assert!(id.groups.is_empty());
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    // `pub(super)` on these makes them visible throughout this module tree.
    use super::tests::{authenticated_body, bearer, make_provider, sa_token, write_temp_token};
    use mockito::Server;

    // ── TokenReview caching ───────────────────────────────────────────────────
    // One API server round trip per proxied request made BatleHub an amplifier
    // pointed at the cluster control plane, and made the control plane a hard
    // dependency of every artifact download.

    #[tokio::test]
    async fn a_repeated_token_is_reviewed_once() {
        let mut server = Server::new_async().await;
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .expect(1)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(authenticated_body("system:serviceaccount:prod:ci-deployer"))
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let token = sa_token();

        for _ in 0..5 {
            let id = p.authenticate(&bearer(&token)).await.unwrap().unwrap();
            assert_eq!(id.role, Role::Admin, "the cached verdict is the same one");
        }
        review.assert_async().await;
    }

    #[tokio::test]
    async fn a_different_token_is_reviewed_separately() {
        let mut server = Server::new_async().await;
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .expect(2)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(authenticated_body("system:serviceaccount:prod:ci-deployer"))
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);

        // Same audience and issuer, different subject.
        p.authenticate(&bearer(&sa_token())).await.unwrap();
        let other = super::tests::jwt(serde_json::json!({
            "iss": super::tests::CLUSTER_ISSUER,
            "aud": ["batlehub"],
            "sub": "system:serviceaccount:dev:my-app",
        }));
        p.authenticate(&bearer(&other)).await.unwrap();
        review.assert_async().await;
    }

    #[tokio::test]
    async fn a_repeated_rejection_costs_one_review_not_one_per_request() {
        // Not caching a "no" at all was one TokenReview per request for as long
        // as a client kept presenting the same refused credential — which is
        // what a misconfigured CI job does, forever. `TOKENREVIEW_REJECT_TTL`
        // keeps the lockout window after a RoleBinding lands to ten seconds.
        let mut server = Server::new_async().await;
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .expect(1)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":false}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let token = sa_token();
        for _ in 0..3 {
            assert!(p.authenticate(&bearer(&token)).await.unwrap().is_none());
        }
        review.assert_async().await;
    }

    /// A token the API server authenticated but did not confirm an audience for
    /// is refused — and that refusal is remembered too, or the narrowest case
    /// left is still one round trip per request.
    #[tokio::test]
    async fn an_unconfirmed_audience_is_refused_once() {
        let mut server = Server::new_async().await;
        let review = server
            .mock("POST", "/apis/authentication.k8s.io/v1/tokenreviews")
            .expect(1)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":{"authenticated":true,"user":{"username":"system:serviceaccount:prod:ci-deployer","groups":[]}}}"#)
            .create_async()
            .await;

        let tf = write_temp_token("self-token").await;
        let p = make_provider(&server, &tf.0);
        let token = sa_token();
        for _ in 0..3 {
            assert!(p.authenticate(&bearer(&token)).await.unwrap().is_none());
        }
        review.assert_async().await;
    }

    #[test]
    fn the_cache_is_keyed_by_hash_not_by_the_token() {
        let p = KubernetesAuthProvider::for_testing(
            reqwest::Client::new(),
            String::new(),
            String::new(),
            vec!["batlehub".to_owned()],
            HashMap::new(),
        );
        let hash = hex::encode(Sha256::digest(b"a-secret-token"));
        p.cache_review(hash.clone(), Verdict::Granted(Identity::anonymous()));

        let keys: Vec<String> = p.review_cache.lock().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec![hash]);
        assert!(
            !keys.iter().any(|k| k.contains("a-secret-token")),
            "the credential itself must not outlive the request"
        );
    }
}
