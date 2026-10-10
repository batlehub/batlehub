use serde::Deserialize;

// ── IP-based blocking ─────────────────────────────────────────────────────────

/// Global fail2ban-style IP blocking configuration.
///
/// The middleware counts "violation events" (responses with status codes in
/// `trigger_on_status`) per client IP. When the count exceeds
/// `violation_threshold` within `violation_window_secs`, the IP is automatically
/// blocked for `ban_duration_secs`. Blocked IPs receive HTTP 403 immediately,
/// before auth or rate-limit checks.
///
/// ```toml
/// [ip_blocking]
/// enabled               = true
/// violation_threshold   = 10
/// violation_window_secs = 300
/// ban_duration_secs     = 3600
/// trigger_on_status     = [429, 401]
/// # List the exact IPs of your reverse proxies so that X-Forwarded-For is
/// # trusted only from those hosts. Leave empty (the default) to always use
/// # the TCP peer address — required when the server is exposed directly.
/// trusted_proxies       = ["10.0.0.1", "10.0.0.2"]
/// ```
/// The longest `violation_window_secs` accepted: housekeeping deletes counter
/// rows older than 30 days (`crates/adapters/src/db/housekeeping.rs`), so a
/// longer window would lose its count mid-window and never ban.
pub const MAX_VIOLATION_WINDOW_SECS: u32 = 30 * 24 * 3600;

#[derive(Debug, Clone, Deserialize)]
pub struct IpBlockingConfig {
    /// Enable IP-based blocking.
    #[serde(default)]
    pub enabled: bool,
    /// Number of violations in the window before auto-blocking the IP.
    #[serde(default = "default_violation_threshold")]
    pub violation_threshold: u32,
    /// Length of the violation counting window in seconds, at most
    /// [`MAX_VIOLATION_WINDOW_SECS`].
    #[serde(default = "default_violation_window")]
    pub violation_window_secs: u32,
    /// How long to keep a blocked IP banned, in seconds.
    #[serde(default = "default_ban_duration")]
    pub ban_duration_secs: u64,
    /// HTTP status codes that increment the violation counter for the source IP.
    #[serde(default = "default_trigger_on_status")]
    pub trigger_on_status: Vec<u16>,
    /// **Deprecated** — use [`ServerConfig::trusted_proxies`] instead.
    ///
    /// IPs (or, since proxy trust became server-level, CIDR ranges) of trusted
    /// reverse proxies. When the TCP peer address falls inside one of these, the
    /// first entry of `X-Forwarded-For` is used as the client IP. When empty
    /// (the default), the TCP peer address is always used and `X-Forwarded-For`
    /// is ignored — preventing spoofed-header bypass.
    ///
    /// Still honoured as a fallback: when `[server].trusted_proxies` is absent
    /// this list governs the forwarded host and scheme as well as the client IP,
    /// and it satisfies the trust-policy requirement that host-based routing
    /// imposes. When both keys are set, `[server]` wins and a config warning
    /// names this one.
    ///
    /// [`ServerConfig::trusted_proxies`]: super::server::ServerConfig::trusted_proxies
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

impl Default for IpBlockingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            violation_threshold: default_violation_threshold(),
            violation_window_secs: default_violation_window(),
            ban_duration_secs: default_ban_duration(),
            trigger_on_status: default_trigger_on_status(),
            trusted_proxies: Vec::new(),
        }
    }
}

fn default_violation_threshold() -> u32 {
    10
}
fn default_violation_window() -> u32 {
    300
}
fn default_ban_duration() -> u64 {
    3600
}
fn default_trigger_on_status() -> Vec<u16> {
    vec![429, 401]
}

// ── Rate limiting ─────────────────────────────────────────────────────────────

/// How to enforce rate-limit violations.
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitEnforcement {
    /// Reject requests that exceed the rate limit with HTTP 429.
    #[default]
    Block,
    /// Allow the request but include a warning header in the response.
    Warn,
}

/// Per-registry request rate limiting.
///
/// Example TOML:
/// ```toml
/// [registries.rate_limit]
/// requests_per_window = 100
/// window_secs         = 60
/// enforcement         = "block"
///
/// [[registries.rate_limit.groups]]
/// name                = "ci-bots"
/// requests_per_window = 5000   # shared pool for all ci-bots members combined
/// window_secs         = 60
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    /// Maximum number of requests a single user (or IP for anonymous) may make within the window.
    pub requests_per_window: u32,
    /// Length of the sliding window in seconds.
    pub window_secs: u32,
    /// Whether to hard-block (429) or just warn on rate-limit overrun.
    #[serde(default)]
    pub enforcement: RateLimitEnforcement,
    /// Optional per-group rate limits. Each entry defines a shared request pool for all
    /// members of the named group. A user's request is checked against both their personal
    /// bucket and every group bucket they belong to; all must have tokens available.
    #[serde(default)]
    pub groups: Vec<GroupRateLimitConfig>,
}

/// A shared request pool for all members of a named group.
///
/// The `name` is matched against the namespaced group strings in `Identity.groups`
/// (e.g. `"oidc:ci-bots"` or `"*:ci-bots"` for a wildcard provider prefix).
///
/// Example TOML:
/// ```toml
/// [[registries.rate_limit.groups]]
/// name                = "oidc:ci-bots"
/// requests_per_window = 5000
/// window_secs         = 60
/// enforcement         = "block"   # optional; inherits parent enforcement when omitted
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct GroupRateLimitConfig {
    /// Group name to match against `Identity.groups` (exact string match).
    pub name: String,
    /// Maximum requests the entire group may collectively make within the window.
    pub requests_per_window: u32,
    /// Length of the sliding window in seconds.
    pub window_secs: u32,
    /// Override enforcement for this group. Inherits the parent `enforcement` when absent.
    pub enforcement: Option<RateLimitEnforcement>,
}

// ── Upstream auth ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum UpstreamAuthConfig {
    Bearer(BearerAuthConfig),
    Basic(BasicAuthConfig),
    Header(HeaderAuthConfig),
}

#[derive(Debug, Deserialize)]
pub struct BearerAuthConfig {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct BasicAuthConfig {
    pub username: String,
    pub password: String,
}

/// Sends a single custom HTTP header on every upstream request.
/// Useful for registries that use `X-API-Key` or similar schemes.
#[derive(Debug, Deserialize)]
pub struct HeaderAuthConfig {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpstreamTlsConfig {
    /// Path to a PEM-encoded CA certificate to trust for this registry's upstream.
    pub ca_cert_path: Option<String>,
}

/// HTTP/SOCKS proxy to use when connecting to upstream registries.
///
/// ```toml
/// [registries.proxy]
/// url = "http://proxy.corp.example.com:3128"
/// # username = "proxyuser"   # optional; alternative to embedding in the URL
/// # password = "proxypass"   # optional
/// # no_proxy = "localhost,internal.example.com"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamProxyConfig {
    /// Proxy URL. Supports `http://`, `https://`, and `socks5://` schemes.
    /// Credentials can be embedded: `http://user:pass@proxy:3128`.
    pub url: String,
    /// Optional proxy username (sets Basic auth; overrides any credentials in `url`).
    #[serde(default)]
    pub username: Option<String>,
    /// Optional proxy password (sets Basic auth; overrides any credentials in `url`).
    #[serde(default)]
    pub password: Option<String>,
    /// Comma-separated list of hosts or domains to bypass the proxy for
    /// (e.g. `"localhost,10.0.0.0/8,internal.example.com"`).
    /// Equivalent to the `NO_PROXY` environment variable.
    #[serde(default)]
    pub no_proxy: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_blocking_defaults() {
        let c = IpBlockingConfig::default();
        assert!(!c.enabled);
        assert_eq!(c.violation_threshold, 10);
        assert_eq!(c.violation_window_secs, 300);
        assert_eq!(c.ban_duration_secs, 3600);
        assert_eq!(c.trigger_on_status, vec![429, 401]);
        assert!(c.trusted_proxies.is_empty());
    }

    #[test]
    fn ip_blocking_deserializes_overrides_and_defaults() {
        let c: IpBlockingConfig = toml::from_str(
            "enabled = true\nviolation_threshold = 3\ntrigger_on_status = [500]\ntrusted_proxies = [\"10.0.0.1\"]",
        )
        .unwrap();
        assert!(c.enabled);
        assert_eq!(c.violation_threshold, 3);
        assert_eq!(c.trigger_on_status, vec![500]);
        assert_eq!(c.trusted_proxies, vec!["10.0.0.1".to_string()]);
        // Unspecified fields fall back to defaults.
        assert_eq!(c.ban_duration_secs, 3600);
        assert_eq!(c.violation_window_secs, 300);
    }

    #[test]
    fn rate_limit_enforcement_default_and_parse() {
        assert_eq!(RateLimitEnforcement::default(), RateLimitEnforcement::Block);
        #[derive(serde::Deserialize)]
        struct W {
            e: RateLimitEnforcement,
        }
        let w: W = toml::from_str("e = \"warn\"").unwrap();
        assert_eq!(w.e, RateLimitEnforcement::Warn);
    }

    #[test]
    fn rate_limit_config_with_group() {
        let c: RateLimitConfig = toml::from_str(
            "requests_per_window = 100\nwindow_secs = 60\n\n[[groups]]\nname = \"ci\"\nrequests_per_window = 5000\nwindow_secs = 60",
        )
        .unwrap();
        assert_eq!(c.requests_per_window, 100);
        assert_eq!(c.window_secs, 60);
        // Enforcement defaults to Block when omitted.
        assert_eq!(c.enforcement, RateLimitEnforcement::Block);
        assert_eq!(c.groups.len(), 1);
        assert_eq!(c.groups[0].name, "ci");
        assert_eq!(c.groups[0].requests_per_window, 5000);
        assert!(c.groups[0].enforcement.is_none());
    }

    #[test]
    fn upstream_auth_variants_deserialize() {
        let b: UpstreamAuthConfig = toml::from_str("type = \"bearer\"\ntoken = \"t\"").unwrap();
        assert!(matches!(b, UpstreamAuthConfig::Bearer(x) if x.token == "t"));
        let basic: UpstreamAuthConfig =
            toml::from_str("type = \"basic\"\nusername = \"u\"\npassword = \"p\"").unwrap();
        assert!(
            matches!(basic, UpstreamAuthConfig::Basic(x) if x.username == "u" && x.password == "p")
        );
        let h: UpstreamAuthConfig =
            toml::from_str("type = \"header\"\nname = \"X-Api-Key\"\nvalue = \"k\"").unwrap();
        assert!(
            matches!(h, UpstreamAuthConfig::Header(x) if x.name == "X-Api-Key" && x.value == "k")
        );
    }

    #[test]
    fn upstream_proxy_and_tls_deserialize() {
        let p: UpstreamProxyConfig = toml::from_str("url = \"http://proxy:3128\"").unwrap();
        assert_eq!(p.url, "http://proxy:3128");
        assert!(p.username.is_none() && p.password.is_none() && p.no_proxy.is_none());
        let t = UpstreamTlsConfig::default();
        assert!(t.ca_cert_path.is_none());
    }
}
