use serde::Deserialize;

// ── Auth ──────────────────────────────────────────────────────────────────────

// `OidcAuthConfig`, `KubernetesAuthConfig`, `ActionsOidcAuthConfig` and their
// supporting types live in `batlehub_core::ports` so that `adapters` (which
// constructs auth providers from them) doesn't need a dependency on this
// crate — `config` is meant to be read only by `server` and `web`. Re-exported
// here so existing `batlehub_config::schema::...` import paths keep working.
pub use batlehub_core::ports::{
    ActionsGroupRule, ActionsOidcAuthConfig, Condition, ConditionMatchType, KubernetesAuthConfig,
    OidcAuthConfig, RuleMatch,
};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthConfig {
    Token(TokenAuthConfig),
    Oidc(OidcAuthConfig),
    Kubernetes(KubernetesAuthConfig),
    #[serde(rename = "actions-oidc")]
    ActionsOidc(ActionsOidcAuthConfig),
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TokenAuthConfig {
    #[serde(default)]
    pub tokens: Vec<TokenEntry>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TokenEntry {
    pub value: String,
    pub role: String,
    pub user_id: Option<String>,
}
