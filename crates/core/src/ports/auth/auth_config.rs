//! Plain deserialized config structs consumed by auth adapters (`crates/adapters/src/auth/*`).
//!
//! These live in `core` (rather than `crates/config`, which re-exports them for
//! backwards-compatible import paths) so `adapters` doesn't need a dependency on
//! the `config` crate — per the workspace's dependency-direction rule, `config`
//! is read only by `server` and `web`.

use serde::Deserialize;

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ConditionMatchType {
    #[default]
    Auto,
    Glob,
    Regex,
}

#[derive(Debug, Deserialize, Clone)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Condition {
    pub claim: String,
    pub pattern: String,
    #[serde(default)]
    pub match_type: ConditionMatchType,
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RuleMatch {
    #[default]
    All,
    Any,
}

#[derive(Debug, Deserialize, Clone)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActionsGroupRule {
    pub group: Option<String>,
    /// Template rendered from JWT claims, e.g. `"{name}/{repository}/{ref_name}"`.
    /// `{name}` = provider name; `{ref_name}` = branch/tag stripped of `refs/heads/`/`refs/tags/`;
    /// any other `{key}` maps to that JWT claim value. All substituted values have `/` → `-`.
    pub group_template: Option<String>,
    /// Role granted when this rule matches. When absent the rule contributes groups
    /// without affecting role elevation — useful for pure group-assignment rules.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    #[serde(default, rename = "match")]
    pub match_mode: RuleMatch,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActionsOidcAuthConfig {
    #[serde(default = "default_actions_oidc_name")]
    pub name: String,
    /// Whether an unreachable identity provider at startup is fatal.
    ///
    /// Defaults to `false` here and `true` for `[[auth]] type = "oidc"`: a CI
    /// provider being down means jobs cannot publish, while a human SSO provider
    /// being down means nobody can log in *and* every request that would have
    /// been authenticated silently becomes anonymous.
    #[serde(default)]
    pub required: bool,
    pub issuer_url: String,
    /// Value the token's `aud` claim must equal. **Required.**
    ///
    /// Not optional the way `OidcAuthConfig::audiences` is, because the issuer
    /// here is shared: `https://token.actions.githubusercontent.com` signs
    /// tokens for every repository on GitHub, and `aud` is the one claim the
    /// calling workflow chooses. Without checking it, `iss` proves only that the
    /// caller is *a* GitHub Actions job, and the whole barrier is whatever
    /// `rules` happen to say.
    ///
    /// Set it to something specific to this deployment — its URL is the usual
    /// choice — and have workflows request it:
    /// `core.getIDToken('https://batlehub.example.com')`.
    pub audience: String,
    #[serde(default = "default_sub")]
    pub user_id_claim: String,
    #[serde(default)]
    pub rules: Vec<ActionsGroupRule>,
}

fn default_actions_oidc_name() -> String {
    "actions-oidc".to_owned()
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OidcAuthConfig {
    /// Unique name for this provider instance.
    /// Used as the prefix for unmapped groups: e.g. `name = "oidc1"` → group `"oidc1:team-a"`.
    /// Also used as `"*:team-a"` wildcard target in `[registries.rbac.groups]`.
    /// Defaults to `"oidc"`.
    #[serde(default = "default_oidc_name")]
    pub name: String,
    /// Whether an unreachable identity provider at startup is fatal.
    ///
    /// Defaults to **true**. Coming up without it looks healthy and is not:
    /// every request that would have carried an identity resolves to anonymous
    /// instead, which reads as a permissions problem rather than an outage. Set
    /// `required = false` to keep the old warn-and-continue behaviour.
    #[serde(default = "default_true")]
    pub required: bool,
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Redirect URI registered with the OIDC provider.
    /// Required for the browser-based SSO login flow.
    /// Example: `"https://batlehub.example.com/api/v1/auth/oidc/callback"`.
    pub redirect_uri: Option<String>,
    /// Base URL of the SPA frontend.  After a successful OIDC callback the
    /// browser is redirected to `{frontend_url}/?oidc_access_token=...`.
    /// Defaults to `""` (same origin as the backend — correct for production).
    /// In development set this to `"http://localhost:5173"`.
    #[serde(default)]
    pub frontend_url: String,
    /// OAuth2 scopes to request.  Defaults to `["openid", "profile", "email"]`.
    #[serde(default = "default_oidc_scopes")]
    pub scopes: Vec<String>,
    /// Values the token's `aud` claim is accepted for. Defaults to
    /// `[client_id]`, which is the audience of an ID token issued to this
    /// client.
    ///
    /// Set it explicitly when the identity provider mints access tokens for a
    /// separate API audience (Auth0's `audience` parameter, Okta's authorization
    /// server `aud`) and that is what reaches this server.
    ///
    /// Leaving audience unchecked means any token the issuer signed for *any*
    /// of its clients authenticates here, so there is no way to switch this off.
    #[serde(default)]
    pub audiences: Vec<String>,
    /// JWT claim used as `user_id` (default: `"sub"`).
    #[serde(default = "default_sub")]
    pub user_id_claim: String,
    /// JWT claim to inspect for role mapping (default: `"role"`).
    /// The claim value may be a string or an array of strings; the highest
    /// matching role in `role_mappings` wins.
    #[serde(default = "default_role_claim")]
    pub role_claim: String,
    /// Maps JWT claim values → proxy role names (`"admin"`, `"user"`).
    /// Claim values not present here default to the `anonymous` role.
    #[serde(default)]
    pub role_mappings: std::collections::HashMap<String, String>,
}

fn default_true() -> bool {
    true
}

fn default_oidc_name() -> String {
    "oidc".to_owned()
}

fn default_oidc_scopes() -> Vec<String> {
    vec![
        "openid".to_owned(),
        "profile".to_owned(),
        "email".to_owned(),
    ]
}

fn default_sub() -> String {
    "sub".to_owned()
}

fn default_role_claim() -> String {
    "role".to_owned()
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct KubernetesAuthConfig {
    /// Unique name for this provider instance.
    /// Used as the prefix for unmapped groups: e.g. `name = "k8s-prod"` → group `"k8s-prod:team-a"`.
    /// Defaults to `"kubernetes"`.
    #[serde(default = "default_kubernetes_name")]
    pub name: String,
    /// Kubernetes API server URL.
    /// Defaults to `https://<KUBERNETES_SERVICE_HOST>:<KUBERNETES_SERVICE_PORT>`
    /// (the env vars injected by Kubernetes for in-cluster use).
    pub api_server: Option<String>,
    /// Path to the CA certificate PEM file for the Kubernetes API server.
    /// Defaults to `/var/run/secrets/kubernetes.io/serviceaccount/ca.crt`.
    pub ca_cert_path: Option<String>,
    /// Path to the batlehub's own service account token used to authenticate
    /// TokenReview API calls.
    /// Defaults to `/var/run/secrets/kubernetes.io/serviceaccount/token`.
    pub token_path: Option<String>,
    /// Audiences passed to the TokenReview API for bound-token validation.
    /// Defaults to `["batlehub"]` when empty.
    #[serde(default)]
    pub audiences: Vec<String>,
    /// Token issuers (`iss`) this provider will spend a TokenReview on.
    ///
    /// Empty (the default) means "any issuer": the provider then narrows on the
    /// audience alone, which is enough to keep a browser's OIDC ID token out of
    /// a TokenReview body but still lets any *other* cluster's bound token be
    /// forwarded. Set it to the cluster's issuer —
    /// `kubectl get --raw /.well-known/openid-configuration | jq -r .issuer`,
    /// typically `https://kubernetes.default.svc.cluster.local` — when this
    /// server sees tokens from more than one issuer.
    ///
    /// Read from the token without verifying its signature, and used only to
    /// decide whether to *ask* the API server. Nothing is granted on it: the
    /// TokenReview verdict is still what authenticates.
    #[serde(default)]
    pub issuers: Vec<String>,
    /// Maps Kubernetes usernames or group names to proxy roles.
    ///
    /// Kubernetes populates:
    /// - username: `"system:serviceaccount:<namespace>:<name>"`
    /// - groups:   `["system:serviceaccounts", "system:serviceaccounts:<namespace>", ...]`
    ///
    /// Values not listed here default to the `anonymous` role.
    /// When a token matches multiple keys, the highest role wins.
    #[serde(default)]
    pub role_mappings: std::collections::HashMap<String, String>,
}

fn default_kubernetes_name() -> String {
    "kubernetes".to_owned()
}
