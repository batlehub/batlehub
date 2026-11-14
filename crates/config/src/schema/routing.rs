//! Host-based (subdomain) registry routing — see `docs/rfc/0001-subdomain-routing.md`.
//!
//! A registry is always reachable at `/proxy/{name}/…`. This module adds the
//! optional second ingress: one or more **hostnames** whose root is that
//! registry, so `https://npm.acme.io/lodash` means exactly what
//! `https://hub.example.com/proxy/npm1/lodash` means.
//!
//! Two sources of hostnames, resolved into one flat table:
//!
//! * `[subdomain_routing]` derives `"{name}.{base_domain}"` for every registry
//!   whose name is a usable DNS label;
//! * each registry's `hosts = [...]` adds explicit vanity hosts.

use serde::Deserialize;

/// Global wildcard host derivation.
///
/// ```toml
/// [subdomain_routing]
/// enabled     = true
/// base_domain = "hub.example.com"   # npm1.hub.example.com -> registry "npm1"
/// scheme      = "https"             # only used to render public URLs
/// ```
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct SubdomainRoutingConfig {
    /// Derive a host for every registry from its name. When `false` (or the
    /// whole section is absent) only explicit `hosts` entries route.
    #[serde(default)]
    pub enabled: bool,
    /// The domain wildcard hosts hang off. Required when `enabled = true`.
    #[serde(default)]
    pub base_domain: Option<String>,
    /// Scheme used when *advertising* a registry's public URL in the API and UI.
    /// Never affects routing — a request's own scheme decides that.
    #[serde(default = "default_scheme")]
    pub scheme: String,
}

fn default_scheme() -> String {
    "https".to_owned()
}

impl Default for SubdomainRoutingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_domain: None,
            scheme: default_scheme(),
        }
    }
}

/// One `host -> registry` binding declared by the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryHostBinding {
    /// Normalised host (see [`normalise_host`]), ready to be used as a lookup key.
    pub host: String,
    /// The registry this host serves.
    pub registry: String,
    /// `true` for a `hosts = [...]` entry, `false` for a wildcard-derived host.
    /// Explicit hosts win over a wildcard *within the same registry*, and are the
    /// preferred public URL.
    pub explicit: bool,
}

/// Normalise a host for table lookups: trimmed, lowercased, port stripped,
/// trailing dot stripped. `NPM.Acme.io:8443.` and `npm.acme.io` are the same host.
///
/// Shared by config validation and the request-time middleware so a host that
/// validates is exactly the host that routes. IPv6 literals keep their brackets
/// (`[::1]:8080` → `[::1]`) — the colons inside them are not a port separator.
pub fn normalise_host(host: &str) -> String {
    let host = host.trim();
    let without_port = if host.starts_with('[') {
        match host.find(']') {
            Some(close) => &host[..=close],
            None => host,
        }
    } else {
        host.split(':').next().unwrap_or(host)
    };
    without_port.trim_end_matches('.').to_ascii_lowercase()
}

/// Why a configured `hosts` entry is not a hostname.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSyntaxError {
    /// Empty, or nothing left after trimming and normalising.
    Empty,
    /// Contains a path separator — a URL was pasted where a hostname belongs.
    ContainsSlash,
    /// Starts with `http://`, `https://`, or any other `scheme://` prefix.
    HasSchemePrefix,
    /// Contains whitespace or another character a hostname cannot hold.
    InvalidCharacter,
}

impl std::fmt::Display for HostSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Empty => "empty after trimming",
            Self::ContainsSlash => "contains '/': expected a bare hostname, not a URL or path",
            Self::HasSchemePrefix => {
                "has a scheme prefix: expected a bare hostname like \
                                      'npm.acme.io', not 'https://npm.acme.io'"
            }
            Self::InvalidCharacter => "contains a character a hostname cannot hold",
        };
        f.write_str(msg)
    }
}

/// Check that `entry` is a bare hostname, before normalisation.
pub fn validate_host_entry(entry: &str) -> Result<(), HostSyntaxError> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Err(HostSyntaxError::Empty);
    }
    if trimmed.contains("://") {
        return Err(HostSyntaxError::HasSchemePrefix);
    }
    if trimmed.contains('/') {
        return Err(HostSyntaxError::ContainsSlash);
    }
    if trimmed
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '?' | '#' | '@' | '\\'))
    {
        return Err(HostSyntaxError::InvalidCharacter);
    }
    if normalise_host(trimmed).is_empty() {
        return Err(HostSyntaxError::Empty);
    }
    Ok(())
}

/// Whether `name` can be used as a single DNS label, i.e. whether a wildcard host
/// can be derived from it.
///
/// Letters, digits and inner hyphens only, at most 63 characters. Case is
/// irrelevant (DNS is case-insensitive and the derived host is lowercased), so
/// `Foo` is usable while `my_registry` and `Foo.Bar` are not.
pub fn is_dns_label(name: &str) -> bool {
    if name.is_empty() || name.len() > 63 {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') {
        return false;
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The wildcard host for `registry_name` under `base_domain`, or `None` when the
/// name is not a usable DNS label.
pub fn wildcard_host(registry_name: &str, base_domain: &str) -> Option<String> {
    if !is_dns_label(registry_name) {
        return None;
    }
    let base = normalise_host(base_domain);
    if base.is_empty() {
        return None;
    }
    Some(format!("{}.{base}", registry_name.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdomain_routing_defaults() {
        let c: SubdomainRoutingConfig = toml::from_str("").unwrap();
        assert!(!c.enabled);
        assert!(c.base_domain.is_none());
        assert_eq!(c.scheme, "https");
    }

    #[test]
    fn subdomain_routing_parses_full_block() {
        let c: SubdomainRoutingConfig =
            toml::from_str("enabled = true\nbase_domain = \"hub.example.com\"\nscheme = \"http\"")
                .unwrap();
        assert!(c.enabled);
        assert_eq!(c.base_domain.as_deref(), Some("hub.example.com"));
        assert_eq!(c.scheme, "http");
    }

    #[test]
    fn normalise_host_is_case_port_and_dot_insensitive() {
        assert_eq!(normalise_host("NPM.Acme.io:8443."), "npm.acme.io");
        assert_eq!(normalise_host("  npm.acme.io  "), "npm.acme.io");
    }

    #[test]
    fn normalise_host_keeps_ipv6_brackets() {
        assert_eq!(normalise_host("[2001:DB8::1]:8443"), "[2001:db8::1]");
    }

    #[test]
    fn host_entry_syntax_rejects_urls_and_blanks() {
        assert_eq!(validate_host_entry("npm.acme.io"), Ok(()));
        assert_eq!(validate_host_entry("  "), Err(HostSyntaxError::Empty));
        assert_eq!(
            validate_host_entry("https://npm.acme.io"),
            Err(HostSyntaxError::HasSchemePrefix)
        );
        assert_eq!(
            validate_host_entry("npm.acme.io/proxy"),
            Err(HostSyntaxError::ContainsSlash)
        );
        assert_eq!(
            validate_host_entry("npm acme.io"),
            Err(HostSyntaxError::InvalidCharacter)
        );
        // A port is tolerated and stripped, matching request-time normalisation.
        assert_eq!(validate_host_entry("npm.acme.io:8443"), Ok(()));
    }

    #[test]
    fn dns_label_rules() {
        assert!(is_dns_label("npm1"));
        assert!(is_dns_label("my-registry"));
        assert!(is_dns_label("Foo"), "case is irrelevant for DNS");
        assert!(!is_dns_label("my_registry"));
        assert!(!is_dns_label("Foo.Bar"));
        assert!(!is_dns_label("-lead"));
        assert!(!is_dns_label("trail-"));
        assert!(!is_dns_label(""));
        assert!(!is_dns_label(&"a".repeat(64)));
    }

    #[test]
    fn wildcard_host_derivation() {
        assert_eq!(
            wildcard_host("npm1", "hub.example.com").as_deref(),
            Some("npm1.hub.example.com")
        );
        assert_eq!(
            wildcard_host("NPM1", "HUB.Example.com.").as_deref(),
            Some("npm1.hub.example.com")
        );
        assert_eq!(wildcard_host("my_registry", "hub.example.com"), None);
        assert_eq!(wildcard_host("npm1", "  "), None);
    }
}
