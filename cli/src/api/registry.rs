use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::BatleHubClient;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub registry_type: String,
    pub mode: String,
    /// The registry's own hostname-rooted URL (`https://npm.acme.io`) when
    /// host-based routing advertises one, otherwise absent — the server omits
    /// the field on a path-routed deployment (RFC 0001).
    #[serde(default)]
    pub public_url: Option<String>,
    /// The first upstream the registry proxies (`https://api.github.com`), or
    /// absent for a kind the server names none for, and on servers older
    /// than this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Whether the caller holds every action a package manager needs to
    /// install from this registry; absent on servers older than the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installable: Option<bool>,
}

impl RegistryInfo {
    /// Base URL a package manager should be pointed at.
    ///
    /// The two forms are not interchangeable. On a registry's own host the
    /// server prefixes `/proxy/{name}` to *every* path itself, so
    /// `{public_url}/proxy/{name}/…` arrives as `/proxy/{name}/proxy/{name}/…`
    /// and 404s; and a registry configured with `path_routing = false` serves
    /// nothing but its own host, so there the subpath form is not merely
    /// longer — it is gone.
    pub fn base_url(&self, server: &str) -> String {
        match self.public_url.as_deref().map(str::trim) {
            Some(url) if !url.is_empty() => url.trim_end_matches('/').to_owned(),
            _ => format!("{}/proxy/{}", server.trim_end_matches('/'), self.name),
        }
    }
}

/// Hostname of `url` — what a `~/.netrc` `machine` line is matched on.
///
/// Falls back to the input when it does not parse, so a half-written server URL
/// shows up in the output as itself rather than silently disappearing.
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

/// Resolves "which URL does *this* package manager talk to" against the
/// registries a server actually has.
///
/// Built from a live registry list when the server can be reached, and from an
/// empty one otherwise — in which case every lookup degrades to the
/// `{server}/proxy/<registry>` placeholder the setup output has always printed.
pub struct RegistryTargets<'a> {
    server: &'a str,
    registries: &'a [RegistryInfo],
}

impl<'a> RegistryTargets<'a> {
    pub fn new(server: &'a str, registries: &'a [RegistryInfo]) -> Self {
        Self {
            server: server.trim_end_matches('/'),
            registries,
        }
    }

    /// The first configured registry of `registry_type`, in server order.
    pub fn registry_for(&self, registry_type: &str) -> Option<&'a RegistryInfo> {
        self.registries
            .iter()
            .find(|r| r.registry_type == registry_type)
    }

    /// Base URL for `registry_type`: the configured registry's own URL, or the
    /// `<registry>` placeholder form when the server has none (or was not
    /// reachable).
    pub fn base_for(&self, registry_type: &str) -> String {
        match self.registry_for(registry_type) {
            Some(registry) => registry.base_url(self.server),
            None => format!("{}/proxy/<registry>", self.server),
        }
    }

    /// Hosts a client will present credentials to when following the
    /// instructions for `registry_types`, in first-seen order.
    ///
    /// `.netrc` is matched by hostname, so this is one entry per host and not
    /// per registry: host-routed registries each contribute their own subdomain,
    /// while everything still on `/proxy/{name}` collapses onto the main host.
    pub fn netrc_hosts(&self, registry_types: &[&str]) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();
        for registry_type in registry_types {
            let host = host_of(&self.base_for(registry_type));
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        hosts
    }
}

impl BatleHubClient {
    pub async fn list_registries(&self) -> Result<Vec<RegistryInfo>> {
        self.get("/api/v1/registries").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(name: &str, ty: &str, public_url: Option<&str>) -> RegistryInfo {
        RegistryInfo {
            name: name.to_owned(),
            registry_type: ty.to_owned(),
            mode: "proxy".to_owned(),
            public_url: public_url.map(str::to_owned),
            upstream: None,
            installable: None,
        }
    }

    #[test]
    fn base_url_falls_back_to_the_proxy_subpath() {
        let r = reg("npm1", "npm", None);
        assert_eq!(
            r.base_url("https://batlehub.example.com/"),
            "https://batlehub.example.com/proxy/npm1"
        );
    }

    #[test]
    fn base_url_prefers_the_advertised_host_without_the_proxy_prefix() {
        let r = reg("npm1", "npm", Some("https://npm1.batlehub.example.com/"));
        assert_eq!(
            r.base_url("https://batlehub.example.com"),
            "https://npm1.batlehub.example.com"
        );
    }

    /// A server that sends `public_url: ""` must not produce `"/npm/"`.
    #[test]
    fn base_url_ignores_an_empty_public_url() {
        let r = reg("npm1", "npm", Some("  "));
        assert_eq!(
            r.base_url("https://batlehub.example.com"),
            "https://batlehub.example.com/proxy/npm1"
        );
    }

    #[test]
    fn base_for_uses_the_placeholder_when_no_registry_matches() {
        let registries = vec![reg("npm1", "npm", None)];
        let targets = RegistryTargets::new("https://batlehub.example.com", &registries);
        assert_eq!(
            targets.base_for("cargo"),
            "https://batlehub.example.com/proxy/<registry>"
        );
    }

    #[test]
    fn netrc_hosts_lists_each_subdomain_once() {
        let registries = vec![
            reg("npm1", "npm", Some("https://npm1.batlehub.example.com")),
            reg(
                "cargo1",
                "cargo",
                Some("https://cargo1.batlehub.example.com"),
            ),
            reg("pypi1", "pypi", None),
            reg("maven1", "maven", None),
        ];
        let targets = RegistryTargets::new("https://batlehub.example.com", &registries);
        assert_eq!(
            targets.netrc_hosts(&["npm", "cargo", "pypi", "maven"]),
            vec![
                "npm1.batlehub.example.com",
                "cargo1.batlehub.example.com",
                // Both path-routed registries answer on the main host, which is
                // one `machine` line, not two.
                "batlehub.example.com",
            ]
        );
    }

    #[test]
    fn host_of_strips_scheme_port_and_path() {
        assert_eq!(
            host_of("https://npm.acme.io:8443/proxy/npm1"),
            "npm.acme.io"
        );
    }

    /// `public_url` is `Option` on the wire *and* absent on a path-routed
    /// server; neither shape may fail to deserialize.
    #[test]
    fn registry_info_deserializes_without_public_url() {
        let r: RegistryInfo =
            serde_json::from_str(r#"{"name":"npm1","type":"npm","mode":"proxy"}"#).unwrap();
        assert_eq!(r.public_url, None);
    }
}
