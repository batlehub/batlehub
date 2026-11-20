use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use jsonwebtoken::{decode, decode_header, Validation};

use batlehub_core::ports::ActionsOidcAuthConfig;
use batlehub_core::{
    entities::{Identity, Role},
    error::CoreError,
    ports::{AuthProvider, RawAuthRequest},
};
use tokio::sync::RwLock;

mod evaluate;
mod jwks;
mod rules;

use evaluate::evaluate_auth_rules;
use jwks::{fetch_jwks, find_key, JwksCache, OidcDiscovery, JWKS_MIN_REFRESH};
use rules::CompiledRule;

#[cfg(test)]
use batlehub_core::ports::{ConditionMatchType, RuleMatch};
#[cfg(test)]
use jsonwebtoken::jwk::JwkSet;
#[cfg(test)]
use rules::{detect_is_regex, render_group_template, CompiledCondition};

#[cfg(test)]
mod tests;

/// Audience every test provider requires and every test token carries.
#[cfg(test)]
pub(crate) const TEST_AUDIENCE: &str = "https://batlehub.test";

pub struct ActionsOidcAuthProvider {
    name: String,
    issuer: String,
    /// The `aud` this provider requires. Mandatory in config, and load-bearing
    /// in a way it is not for a normal OIDC provider: `token.actions.
    /// githubusercontent.com` issues for every repository on GitHub, so `iss`
    /// alone narrows the caller down to "some GitHub Actions job somewhere".
    audience: String,
    user_id_claim: String,
    rules: Vec<CompiledRule>,
    http: reqwest::Client,
    jwks_uri: String,
    cache: Arc<RwLock<JwksCache>>,
}

impl ActionsOidcAuthProvider {
    pub async fn new(cfg: &ActionsOidcAuthConfig) -> anyhow::Result<Self> {
        // Checked before any network call: a config error should not be
        // reported as "the identity provider was unreachable".
        if cfg.audience.trim().is_empty() {
            anyhow::bail!(
                "[[auth]] type = \"actions-oidc\" name = \"{}\": `audience` must not be empty. \
                 The issuer is shared by every repository on the forge, so `audience` is what \
                 identifies tokens minted for this deployment.",
                cfg.name,
            );
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

        let keys = fetch_jwks(&http, &discovery.jwks_uri).await.map_err(|e| {
            anyhow::anyhow!("fetching initial JWKS from {}: {e}", discovery.jwks_uri)
        })?;

        let rules = cfg
            .rules
            .iter()
            .map(CompiledRule::compile)
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(Self {
            name: cfg.name.clone(),
            issuer: discovery.issuer,
            audience: cfg.audience.clone(),
            user_id_claim: cfg.user_id_claim.clone(),
            rules,
            http,
            jwks_uri: discovery.jwks_uri,
            cache: Arc::new(RwLock::new(JwksCache {
                keys,
                fetched_at: Instant::now(),
            })),
        })
    }

    async fn get_decoding_key(
        &self,
        kid: Option<&str>,
    ) -> Result<jsonwebtoken::DecodingKey, CoreError> {
        {
            let cache = self.cache.read().await;
            if let Some(key) = find_key(&cache.keys, kid) {
                return Ok(key);
            }
            if cache.fetched_at.elapsed() < JWKS_MIN_REFRESH {
                return Err(CoreError::Auth("unknown JWT signing key".to_owned()));
            }
        }

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
}

#[async_trait]
impl AuthProvider for ActionsOidcAuthProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn authenticate(&self, req: &RawAuthRequest) -> Result<Option<Identity>, CoreError> {
        let Some(token) = req.bearer_token() else {
            return Ok(None);
        };

        let header = decode_header(token)
            .map_err(|e| CoreError::Auth(format!("invalid JWT header: {e}")))?;

        let decoding_key = self.get_decoding_key(header.kid.as_deref()).await?;

        // `aud` is the whole identity check here beyond the signature: any
        // workflow on the forge can obtain a correctly-signed token from this
        // issuer, and `audience` is the value it has to have asked for.
        let mut validation = Validation::new(header.alg);
        validation.set_audience(&[&self.audience]);
        if !self.issuer.is_empty() {
            validation.set_issuer(&[&self.issuer]);
        }
        // Required, not merely checked-if-present — see `required_spec_claims`.
        validation.set_required_spec_claims(&crate::auth::oidc::required_spec_claims(
            !self.issuer.is_empty(),
        ));

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
        let (role, groups) = evaluate_auth_rules(&self.rules, &claims, &self.name);
        // A token no rule matched is any workflow on the forge that asked for
        // our audience: it gets no principal, so it cannot match a `user:`
        // grant or claim its own rate-limit bucket by pushing branches.
        let matched = role != Role::Anonymous || !groups.is_empty();
        let user_id = claims
            .get(&self.user_id_claim)
            .and_then(|v| v.as_str())
            .filter(|_| matched)
            .map(str::to_owned);

        Ok(Some(Identity {
            user_id,
            role,
            auth_provider: Some(self.name.clone()),
            groups,
        }))
    }
}

#[cfg(test)]
impl ActionsOidcAuthProvider {
    fn for_testing(
        name: impl Into<String>,
        user_id_claim: impl Into<String>,
        rules: Vec<CompiledRule>,
        jwks: JwkSet,
    ) -> Self {
        Self {
            name: name.into(),
            issuer: String::new(),
            audience: TEST_AUDIENCE.to_owned(),
            user_id_claim: user_id_claim.into(),
            rules,
            http: reqwest::Client::new(),
            jwks_uri: String::new(),
            cache: Arc::new(RwLock::new(JwksCache {
                keys: jwks,
                fetched_at: Instant::now(),
            })),
        }
    }

    /// Like `for_testing` but with a backdated `fetched_at` so the JWKS refresh
    /// path is exercisable without sleeping through `JWKS_MIN_REFRESH`.
    fn for_testing_stale(
        name: impl Into<String>,
        user_id_claim: impl Into<String>,
        rules: Vec<CompiledRule>,
        initial_jwks: JwkSet,
        jwks_uri: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            issuer: String::new(),
            audience: TEST_AUDIENCE.to_owned(),
            user_id_claim: user_id_claim.into(),
            rules,
            http: reqwest::Client::new(),
            jwks_uri: jwks_uri.into(),
            cache: Arc::new(RwLock::new(JwksCache {
                keys: initial_jwks,
                fetched_at: Instant::now()
                    .checked_sub(JWKS_MIN_REFRESH + std::time::Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            })),
        }
    }
}
