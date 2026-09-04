use std::net::IpAddr;

use ipnet::IpNet;
use serde::Deserialize;

// ── Server ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    /// What this process does (RFC 0018 §4.1): `proxy` serves requests and
    /// enqueues scan jobs, `worker` dequeues and scans. Default both — the
    /// embedded worker — so every existing deployment keeps working.
    #[serde(default = "super::security::default_roles")]
    pub roles: Vec<super::security::ProcessRole>,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Directory from which to serve the built SPA (optional).
    pub static_dir: Option<String>,
    /// Path to the `batlehub-cli` binary to serve via `GET /api/v1/cli/download`.
    /// When absent the endpoint returns 404.
    ///
    /// ```toml
    /// [server]
    /// cli_binary_path = "/usr/local/bin/batlehub-cli"
    /// ```
    #[serde(default)]
    pub cli_binary_path: Option<String>,
    /// Allowed CORS origins. When set, only the listed origins receive
    /// Access-Control-Allow-Origin headers. When absent, all origins are
    /// allowed (suitable for development; restrict in production).
    #[serde(default)]
    pub cors_allowed_origins: Option<Vec<String>>,
    /// CIDR ranges (or bare IPs) of the reverse proxies in front of BatleHub.
    /// `Forwarded` / `X-Forwarded-Host` / `X-Forwarded-Proto` / `X-Forwarded-For`
    /// are honoured only when the TCP peer falls inside one of these.
    ///
    /// ```toml
    /// [server]
    /// trusted_proxies = ["10.42.0.0/16", "192.168.1.10"]
    /// ```
    ///
    /// Three distinguishable states, which is why this is an `Option`:
    ///
    /// | Value    | Behaviour                                                        |
    /// |----------|------------------------------------------------------------------|
    /// | absent   | legacy permissive — forwarded host/scheme believed unconditionally, `X-Forwarded-For` ignored. A hard error once host-based routing is configured. |
    /// | `[]`     | forwarded headers ignored entirely; the `Host` header and the connection decide. |
    /// | `[nets]` | forwarded headers honoured only from peers inside those prefixes. |
    ///
    /// A bare address is accepted and treated as a `/32` (`/128` for IPv6), so
    /// every value that was valid for the deprecated
    /// `[ip_blocking].trusted_proxies` stays valid here.
    #[serde(default)]
    pub trusted_proxies: Option<Vec<String>>,
    /// Signing material for RFC 0012 download URLs. Absent means the feature is
    /// unavailable, and any registry with `signed_downloads = true` is a
    /// startup error rather than a registry that quietly serves nothing.
    #[serde(default)]
    pub signed_urls: Option<SignedUrlsConfig>,
}

/// `[server.signed_urls]` — the instance secret that signs download URLs.
///
/// Global rather than per-registry because the key is a property of the
/// instance, not of a registry; the per-registry switch is
/// `[[registries]].signed_downloads`.
///
/// ```toml
/// [server.signed_urls]
/// # The loader interpolates ${VAR}; a signing key does not belong in a file
/// # that gets committed. See docs/guide/configuration.md, "Sensitive values".
/// secret           = "${BATLEHUB_URL_SIGNING_SECRET}"
/// ttl_seconds      = 300   # default; hard-capped at 3600
/// # Only while rotating. `${VAR}` interpolation *fails the whole config load*
/// # when the variable is unset (`expand_braced_var`), so once the old secret is
/// # retired either remove this line or export the variable as an empty string —
/// # an entry that interpolates to empty is dropped.
/// previous_secrets = ["${BATLEHUB_URL_SIGNING_SECRET_OLD}"]
/// ```
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SignedUrlsConfig {
    /// HMAC signing secret, 32 bytes minimum.
    pub secret: String,
    /// Lifetime of a minted URL. Terraform follows one within milliseconds, so
    /// the margin is for a slow runner rather than for a human.
    #[serde(default = "default_signed_url_ttl")]
    pub ttl_seconds: u64,
    /// Verified against but never minted with, so a secret can be rotated
    /// without a flag day. An entry that interpolates to empty is dropped, so
    /// `BATLEHUB_URL_SIGNING_SECRET_OLD=""` is a valid steady state — but the
    /// variable must still *exist*: `${VAR}` expansion happens before parsing
    /// and `expand_braced_var` refuses an unset variable, so leaving the line in
    /// place with the variable unset fails the config load outright rather than
    /// being ignored. Remove the line or export it empty once rotation is done.
    #[serde(default)]
    pub previous_secrets: Vec<String>,
}

fn default_signed_url_ttl() -> u64 {
    300
}

impl SignedUrlsConfig {
    /// Previous secrets with the empties removed.
    pub fn active_previous_secrets(&self) -> Vec<String> {
        self.previous_secrets
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Parse `trusted_proxies` entries into CIDR prefixes.
///
/// A bare address is widened to its host prefix (`/32` for IPv4, `/128` for
/// IPv6) so the deprecated `[ip_blocking].trusted_proxies` exact-IP values keep
/// matching exactly what they matched before.
pub fn parse_trusted_proxies(entries: &[String]) -> Result<Vec<IpNet>, String> {
    entries
        .iter()
        .map(|entry| {
            let trimmed = entry.trim();
            trimmed
                .parse::<IpNet>()
                .or_else(|_| trimmed.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| {
                    format!(
                        "invalid trusted proxy entry '{entry}': expected an IP address \
                         (10.0.0.1) or a CIDR range (10.42.0.0/16)"
                    )
                })
        })
        .collect()
}

pub(super) fn default_host() -> String {
    "0.0.0.0".to_owned()
}

pub(super) fn default_port() -> u16 {
    8080
}

// ── Database ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    #[serde(rename = "type")]
    pub db_type: String,
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// Minimum number of idle connections the pool keeps warm.
    #[serde(default = "default_min_connections")]
    pub min_connections: u32,
    /// How long to wait for a connection to become available before erroring out.
    #[serde(default = "default_acquire_timeout_secs")]
    pub acquire_timeout_secs: u64,
}

pub(super) fn default_max_connections() -> u32 {
    10
}

pub(super) fn default_min_connections() -> u32 {
    1
}

pub(super) fn default_acquire_timeout_secs() -> u64 {
    30
}

// ── Cache backend ─────────────────────────────────────────────────────────────

/// Selects the metadata cache backend.
///
/// In TOML:
/// ```toml
/// [cache]
/// type = "postgres"   # "memory" (default) | "postgres" | "redis"
///
/// # Required when type = "redis":
/// url = "redis://localhost:6379"
/// ```
#[derive(Debug, Deserialize)]
pub struct CacheConfig {
    /// `"memory"` (default) uses an in-process HashMap; no persistence between restarts.
    /// `"postgres"` stores entries in the `metadata_cache` table; survives restarts and
    /// is shared across multiple server instances.
    /// `"redis"` stores entries in Redis; survives restarts and is shared across instances.
    #[serde(rename = "type", default = "default_cache_type")]
    pub cache_type: String,
    /// Connection URL for the Redis cache backend (required when `type = "redis"`).
    /// Format: `redis://[:<password>@]<host>[:<port>][/<db>]`
    /// or `rediss://...` for TLS.
    pub url: Option<String>,
}

pub(super) fn default_cache_type() -> String {
    "memory".to_owned()
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            cache_type: default_cache_type(),
            url: None,
        }
    }
}

// ── OTel ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct OtelConfig {
    /// OTLP endpoint, e.g. `http://localhost:4317`.
    pub endpoint: String,
    #[serde(default = "default_service_name")]
    pub service_name: String,
}

pub fn default_service_name() -> String {
    "batlehub".to_owned()
}

/// Whether an OIDC `issuer_url` is safe to fetch a discovery document from.
///
/// HTTPS anywhere, or plain HTTP on loopback only. Loopback is exempt because
/// that is how the test suites and a developer's local Keycloak run, and there
/// is no network path for anyone to sit on.
///
/// Deliberately a string check rather than a URL parse: the only question is
/// which transport will be used, and a parser would introduce its own opinions
/// about hosts this function has none about.
pub fn is_secure_issuer_url(url: &str) -> bool {
    // Scheme and host are case-insensitive (RFC 3986 §3.1, §3.2.2), and the
    // comparisons below were not: `HTTPS://idp.example.com` is a URL `reqwest`
    // dials over TLS, and this refused it with "must use https" — a valid config
    // that will not boot. Lowercased for the *decision* only; the URL itself is
    // untouched, because a path is case-sensitive.
    let lowered = url.to_ascii_lowercase();
    let url = lowered.as_str();
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        // Neither scheme: `OidcAuthProvider::new` will fail to fetch it anyway,
        // and reporting "must use https" is the more useful message.
        return false;
    };
    // `\` ends the authority for a special scheme exactly as `/` does (WHATWG
    // URL §4.4), and userinfo is everything before the last `@` — both are how
    // an authority is read past. `http://localhost:8080@evil.example/realm`
    // split on `:` alone yields the host `localhost`, so this answered "safe"
    // for a URL `reqwest` dials in cleartext to `evil.example`, which is the one
    // thing the function exists to refuse.
    let authority = rest.split(['/', '\\', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // The port, without mistaking an IPv6 literal's own colons for one: a
    // bracketed host ends at `]`, and `http://[::1]/realm` — no port at all —
    // read as the host `[:` before this.
    let host = match authority.strip_prefix('[') {
        Some(inner) => inner
            .split_once(']')
            .map_or(authority, |(h, _)| &authority[..h.len() + 2]),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

#[cfg(test)]
mod issuer_url_tests {
    use super::is_secure_issuer_url;

    /// Scheme and host are case-insensitive; the check was not, and refused a
    /// URL `reqwest` dials over TLS.
    #[test]
    fn the_scheme_and_the_loopback_host_are_case_insensitive() {
        assert!(is_secure_issuer_url("HTTPS://idp.example.com/realms/main"));
        assert!(is_secure_issuer_url("HtTpS://idp.example.com"));
        assert!(is_secure_issuer_url("HTTP://LOCALHOST:8080/realms/main"));
        // And still no widening: a non-loopback host over plain HTTP is refused
        // however it is spelled.
        assert!(!is_secure_issuer_url("HTTP://idp.example.com"));
        assert!(!is_secure_issuer_url("HTTP://localhost.evil.example"));
    }

    #[test]
    fn https_is_always_fine() {
        assert!(is_secure_issuer_url("https://idp.example.com"));
        assert!(is_secure_issuer_url("https://idp.example.com/realms/main"));
        assert!(is_secure_issuer_url("https://idp.example.com:8443"));
    }

    #[test]
    fn plain_http_is_loopback_only() {
        assert!(is_secure_issuer_url("http://localhost:8080/realms/main"));
        assert!(is_secure_issuer_url("http://127.0.0.1:9000"));
        assert!(is_secure_issuer_url("http://[::1]:9000"));

        assert!(!is_secure_issuer_url("http://idp.example.com"));
        assert!(!is_secure_issuer_url("http://10.0.0.5:8080"));
        assert!(
            !is_secure_issuer_url("http://localhost.evil.example"),
            "a host that merely starts with localhost is not loopback"
        );
    }

    #[test]
    fn a_missing_scheme_is_refused() {
        assert!(!is_secure_issuer_url("idp.example.com"));
        assert!(!is_secure_issuer_url(""));
    }

    /// Userinfo is not the host. `http://localhost:8080@evil.example/realm` is
    /// a cleartext fetch from `evil.example` that read as loopback while the
    /// port was split off the whole authority.
    #[test]
    fn userinfo_cannot_impersonate_loopback() {
        assert!(!is_secure_issuer_url(
            "http://localhost:8080@evil.example/x"
        ));
        assert!(!is_secure_issuer_url("http://127.0.0.1@evil.example/x"));
        assert!(!is_secure_issuer_url("http://[::1]@evil.example/x"));
        // A `\` ends the authority for a special scheme just as `/` does
        // (WHATWG URL §4.4), so this one really *is* loopback — `@evil.example`
        // is path, and that is where `reqwest` puts it too. Asserted so the two
        // readings are pinned as agreeing rather than left to chance.
        assert!(is_secure_issuer_url(r"http://localhost\@evil.example/x"));
        assert!(!is_secure_issuer_url(r"http://evil.example\@localhost/x"));
    }

    /// A loopback literal with no port is loopback. `rsplit_once(':')` on
    /// `[::1]` read the host as `[:` and refused a legitimate URL.
    #[test]
    fn a_bracketed_ipv6_without_a_port_is_still_loopback() {
        assert!(is_secure_issuer_url("http://[::1]/realms/main"));
        assert!(is_secure_issuer_url("http://[::1]"));
        assert!(!is_secure_issuer_url("http://[2001:db8::1]/realms/main"));
    }
}
