pub mod oidc;
pub mod tokens;

use std::collections::HashSet;
use std::sync::Arc;

/// The names of every configured OIDC auth provider (`[[auth]] type = "oidc"`).
///
/// `create_token` consults this set to decide whether the caller is an
/// interactive SSO session, which is the only kind of credential allowed to mint
/// a personal access token — a machine credential (static token, Kubernetes
/// service account, CI OIDC, or another PAT) must not be able to issue a
/// longer-lived one.
///
/// It exists because the provider name is operator-chosen: `name` defaults to
/// `"oidc"` but a deployment may call it `"authentik"`, or configure several
/// providers at once (`"oidc1"`, `"oidc2"`). The check used to compare against
/// the literal `"oidc"`, so every deployment that renamed its provider got a
/// blanket 403 on token creation with no indication why, and fell back to
/// non-expiring static tokens in `config.toml`.
///
/// An empty set means no OIDC provider is configured, and therefore nobody can
/// mint a PAT: absent configuration denies rather than admits.
#[derive(Clone, Debug, Default)]
pub struct OidcProviderNames(Arc<HashSet<String>>);

impl OidcProviderNames {
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self(Arc::new(names.into_iter().map(Into::into).collect()))
    }

    /// Whether `name` identifies one of the configured OIDC providers.
    ///
    /// Matching is exact: a provider of some other kind that an operator happens
    /// to have named `"oidc"` is not an OIDC session and does not pass.
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty_and_admits_nothing() {
        let names = OidcProviderNames::default();
        assert!(names.is_empty());
        assert!(!names.contains("oidc"));
    }

    #[test]
    fn contains_only_configured_names() {
        let names = OidcProviderNames::new(["authentik", "keycloak"]);
        assert!(!names.is_empty());
        assert!(names.contains("authentik"));
        assert!(names.contains("keycloak"));
        // The historical hardcoded value is not special.
        assert!(!names.contains("oidc"));
        assert!(!names.contains("kubernetes"));
    }
}
