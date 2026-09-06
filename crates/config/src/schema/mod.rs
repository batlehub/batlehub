pub mod air_gap;
pub mod auth;
pub mod flag_sources;
pub mod forge;
pub mod network;
pub mod notifications;
pub mod registry;
pub mod routing;
pub mod rules;
pub mod security;
pub mod server;
pub mod storage;
pub mod warnings;

pub use air_gap::{valid_ed25519_hex_key, AirGapConfig};
pub use auth::{
    ActionsGroupRule, ActionsOidcAuthConfig, AuthConfig, Condition, ConditionMatchType,
    KubernetesAuthConfig, OidcAuthConfig, RuleMatch, TokenAuthConfig, TokenEntry,
};

/// A key in an error message: enough to recognise, never the whole of a value
/// an operator may have pasted from a secret store by mistake.
fn truncate_key(key: &str) -> String {
    let head: String = key.chars().take(12).collect();
    if key.chars().count() > 12 {
        format!("{head}…")
    } else {
        head
    }
}
pub use flag_sources::FlagSourceConfig;
pub use forge::{valid_repo_glob, ApiReadsConfig, RawConfig, RefsConfig, MIN_BRANCH_TTL_SECS};
pub use network::{
    BasicAuthConfig, BearerAuthConfig, GroupRateLimitConfig, HeaderAuthConfig, IpBlockingConfig,
    RateLimitConfig, RateLimitEnforcement, UpstreamAuthConfig, UpstreamProxyConfig,
    UpstreamTlsConfig,
};
pub use notifications::{
    EmailChannelConfig, InboundWebhookConfig, NotificationChannelConfig, NotificationsConfig,
    SlackChannelConfig, TeamsChannelConfig, WebhookChannelConfig,
};
pub use registry::{
    default_true, BetaChannelConfig, CachePolicy, FeatureFlagsConfig, GrantsShadowConfig,
    Immutable, IntegrityConfig, NamespaceConfig, QuotaConfig, QuotaEnforcement, ReadmeConfig,
    RegistryConfig, RegistryMode, RepoSigningConfig, RetentionConfig, SbomConfig, SigningConfig,
    UpstreamDetailConfig, VersioningPolicy,
};
pub use routing::{
    is_dns_label, normalise_host, validate_host_entry, wildcard_host, HostSyntaxError,
    RegistryHostBinding, SubdomainRoutingConfig,
};
pub use rules::{
    CveGateConfig, DenyLatestConfig, ExploreRbacConfig, LicenseGateConfig, RbacConfig,
    ReleaseAgeGateConfig, RequireSignedReleaseConfig, RuleConfig, TrustedPublisherConfig,
    VersionGateConfig,
};
pub use security::{
    default_roles, EscalationConfig, ProcessRole, SandboxConfig, ScannerConfig, SecurityConfig,
    SecurityRescanConfig, WorkerConfig, MIN_AGE_FLOOR_SECS,
};
pub use server::{
    default_service_name, is_secure_issuer_url, parse_trusted_proxies, CacheConfig, DatabaseConfig,
    OtelConfig, ServerConfig, SignedUrlsConfig,
};
pub use warnings::ConfigWarning;

pub use storage::{
    FilesystemStorageConfig, MultiStorageConfig, NamedStorageConfig, S3StorageConfig,
    StorageBackendConfig, StoragesConfig,
};

use anyhow::{bail, Result};
use batlehub_core::entities::Severity;
use batlehub_core::ports::LICENSE_EXTRACTION_TYPES;
use serde::Deserialize;

// ── Top-level ─────────────────────────────────────────────────────────────────

/// The current config schema version this binary understands. Bump this only
/// for changes that would silently break an existing config file if applied
/// unchanged (removing/renaming a field, changing a default's meaning) — see
/// "Config versioning" in `docs/guide/configuration.md`.
pub const CURRENT_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    /// Config schema version. Optional; absent is treated as
    /// [`CURRENT_CONFIG_VERSION`] so every existing config file keeps working
    /// unchanged. An explicit value newer than this binary supports is
    /// rejected at startup rather than silently misbehaving.
    #[serde(default)]
    pub config_version: Option<u32>,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    #[serde(default)]
    pub auth: Vec<AuthConfig>,
    pub storage: StoragesConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub registries: Vec<RegistryConfig>,
    /// RFC 0015 §4.1's instance tier — grants that apply above every registry.
    ///
    /// Where the control-surface verbs are delegated: `config:read`,
    /// `system:read`, `blocks:write` and the rest guard endpoints that name no
    /// registry, so a registry-tier block cannot express them. §10 rule 5 already
    /// grants all of them to `role:admin`, so this block is only ever needed to
    /// give one of them to somebody *else*.
    ///
    /// Unioned on top of that translation, never replacing it — a grant only ever
    /// adds (§4.3).
    #[serde(default)]
    pub grants: Option<std::collections::HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub otel: Option<OtelConfig>,
    #[serde(default)]
    pub limits: LimitsConfig,
    /// Optional global IP-based blocking (fail2ban) configuration.
    #[serde(default)]
    pub ip_blocking: Option<IpBlockingConfig>,
    /// Optional webhook and notification configuration.
    #[serde(default)]
    pub notifications: Option<NotificationsConfig>,
    /// Third parties that may push vulnerability flags (RFC 0002 §4.3).
    #[serde(default)]
    pub flag_sources: Vec<FlagSourceConfig>,
    /// A server that will not dial out (RFC 0008 §4.1). Absent means today's
    /// behaviour.
    #[serde(default)]
    pub air_gap: Option<AirGapConfig>,
    /// Global HTTP/SOCKS proxy applied to all registry upstreams that do not
    /// define their own `[registries.proxy]` section.
    ///
    /// Can be overridden at runtime via `PROXY_CACHE__PROXY__URL` (and related
    /// variables) without changing the config file.
    #[serde(default)]
    pub proxy: Option<UpstreamProxyConfig>,
    /// Optional periodic re-check of cached SBOMs against the OSV vulnerability
    /// database. When absent or `enabled = false`, no background scan runs.
    #[serde(default)]
    pub vulnerability_scan: Option<VulnerabilityScanConfig>,
    /// Optional periodic sweep for storage blobs no artifact-meta row points at.
    /// When absent or `enabled = false`, orphans are only collected when an
    /// operator asks for it.
    #[serde(default)]
    pub cache_coherence: Option<CacheCoherenceConfig>,
    /// The upstream-disappearance audit (RFC 0014). Absent means off.
    #[serde(default)]
    pub upstream_audit: UpstreamAuditConfig,
    /// Optional wildcard host derivation for host-based registry routing.
    /// Absent or `enabled = false` derives no wildcard hosts; a registry can
    /// still declare explicit `hosts`.
    #[serde(default)]
    pub subdomain_routing: Option<SubdomainRoutingConfig>,
    /// What numbers this instance keeps and publishes. Absent means today's
    /// behaviour: `/metrics` served, history recorded, 30 days retained.
    #[serde(default)]
    pub stats: StatsConfig,
    /// What the catalogue's search box can see. Absent means names only, which
    /// is what it has always matched.
    #[serde(default)]
    pub search: SearchConfig,
    /// `[scanners.<name>]` — the scanners registries opt into by name
    /// (RFC 0018 §4.1). Absent means only the implicit `osv`.
    #[serde(default)]
    pub scanners: std::collections::HashMap<String, ScannerConfig>,
    /// `[worker]` — how the scan worker role is tuned (RFC 0018 §4.1).
    #[serde(default)]
    pub worker: WorkerConfig,
}

// ── Search ────────────────────────────────────────────────────────────────────

fn default_text_config() -> String {
    batlehub_adapters_text_config().to_owned()
}

/// The default Postgres text search configuration, named in one place.
///
/// `english`, and RFC 0007-bis was drafted arguing for `simple`. The draft's
/// reasoning was that stemming mangles identifiers — `axios` becomes `axio` —
/// which is true and does not follow: the *query* is stemmed by the same
/// configuration, so it still matches. Measured, `english` answered all seven
/// test queries and `simple` failed two, including `retry` against a README that
/// says `retrying` (RFC 0007-bis §13.3).
fn batlehub_adapters_text_config() -> &'static str {
    "english"
}

/// What the catalogue's search matches (RFC 0007-bis §4.1).
///
/// ```toml
/// [search]
/// readmes     = true        # match README prose as well as package names
/// text_config = "english"   # the Postgres text search configuration
/// ```
///
/// **Off by default**, unlike README *capture*, which RFC 0007 defaulted on
/// because it costs one already-parsed field. This builds an index over prose,
/// and the cost is storage plus write amplification on every capture. An
/// operator should choose it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// Match README prose as well as package names.
    ///
    /// With this off, `?in=readme` and `?in=both` are accepted and answer
    /// exactly as `?in=name` does, plus a response field saying so. A parameter
    /// that silently means something else is the failure this whole RFC family
    /// keeps finding; one that says *"prose search is not enabled on this
    /// instance"* is one an operator can act on.
    #[serde(default)]
    pub readmes: bool,

    /// The Postgres text search configuration the index is built with.
    ///
    /// Settable because an estate whose internal packages are documented in
    /// another language is precisely the kind of deployment that self-hosts.
    /// Changing it **rebuilds the generated column** — `to_tsvector` in one must
    /// be IMMUTABLE, so the configuration has to be a literal — which makes this
    /// a decision to take at install rather than to tune later. The server does
    /// the rebuild on startup and says so in the log rather than leaving an
    /// operator to find out during a migration.
    #[serde(default = "default_text_config")]
    pub text_config: String,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            readmes: false,
            text_config: default_text_config(),
        }
    }
}

// ── Stats ─────────────────────────────────────────────────────────────────────

fn default_history_retention_days() -> u32 {
    30
}

/// Both of the instance's statistical outputs, in one block.
///
/// They live together because an operator deciding "do I want this instance
/// keeping numbers" is asking one question, not two (RFC 0004 R9) — even though
/// the two flags answer different halves of it: `metrics_enabled` is about
/// *exposure*, `history_enabled` is about *storage*.
///
/// ```toml
/// [stats]
/// # The rollup behind the dashboard's trend.
/// history_enabled        = true   # default
/// history_retention_days = 30     # 0 disables retention pruning
///
/// # The Prometheus recorder and the /metrics endpoint.
/// metrics_enabled        = true   # default: today's behaviour
/// ```
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsConfig {
    /// Record the hourly cache-statistics rollup that backs the admin
    /// dashboard's trend. `false` restores the pre-RFC-0004 dashboard, which
    /// shows only counters since the current process started.
    #[serde(default = "default_true")]
    pub history_enabled: bool,

    /// How many days of rollup rows to keep. `0` disables pruning rather than
    /// disabling history — that is what `history_enabled = false` is for.
    ///
    /// One row per registry per hour is under 9 000 rows a year per registry,
    /// so this is a tidiness setting, not a storage argument (RFC 0004 R9).
    #[serde(default = "default_history_retention_days")]
    pub history_retention_days: u32,

    /// Install the Prometheus recorder and serve `/metrics`.
    ///
    /// **This is a security control, not a preference** (RFC 0004 §7).
    /// `/metrics` is unauthenticated and, before RFC 0004, unconditional: it
    /// publishes cache hit rates, per-registry pull volumes and upstream
    /// latencies to anyone who can reach the port. That is defensible behind an
    /// ingress that does not route it, and indefensible for a self-hoster who
    /// had no way to turn it off. Defaults to `true` so no existing scrape
    /// breaks on upgrade.
    #[serde(default = "default_true")]
    pub metrics_enabled: bool,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            history_enabled: true,
            history_retention_days: default_history_retention_days(),
            metrics_enabled: true,
        }
    }
}

// ── Vulnerability scan ──────────────────────────────────────────────────────────

fn default_vuln_interval_secs() -> u64 {
    86_400
}

fn default_vuln_batch_size() -> usize {
    100
}

/// Periodic SBOM-vs-CVE re-check configuration.
///
/// ```toml
/// [vulnerability_scan]
/// enabled       = true
/// interval_secs = 86400          # daily
/// osv_api_url   = "https://api.osv.dev"
/// batch_size    = 100
/// ```
#[derive(Debug, Deserialize)]
pub struct VulnerabilityScanConfig {
    /// Enable the periodic background scan.
    #[serde(default)]
    pub enabled: bool,
    /// Seconds between scan runs. Defaults to one day.
    #[serde(default = "default_vuln_interval_secs")]
    pub interval_secs: u64,
    /// Base URL of the OSV API. Defaults to `https://api.osv.dev` when absent.
    #[serde(default)]
    pub osv_api_url: Option<String>,
    /// Number of SBOMs processed per page. Defaults to 100.
    #[serde(default = "default_vuln_batch_size")]
    pub batch_size: usize,
}

// ── Upstream audit (RFC 0014) ─────────────────────────────────────────────────

fn default_audit_interval_secs() -> u64 {
    21_600
}
fn default_confirm_after() -> u32 {
    3
}
fn default_confirm_min_age_secs() -> u64 {
    86_400
}
fn default_outage_ratio() -> f64 {
    0.25
}
fn default_on_confirmed() -> String {
    "audit".to_owned()
}
fn default_true_audit() -> bool {
    true
}

/// `[upstream_audit]` (RFC 0014 §4.1): the periodic sweep that asks each
/// proxy/hybrid upstream whether the artifacts cached from it still exist,
/// confirms a disappearance across sweeps, and holds a confirmed artifact
/// back from eviction.
///
/// `deny_unknown_fields`: a typo in `confirm_after` must fail the load, not
/// silently take the default. Concurrency is `[worker].max_concurrent` —
/// the sweep runs on the worker role (0014 §13).
///
/// ```toml
/// [upstream_audit]
/// enabled              = true
/// interval_secs        = 21600
/// confirm_after        = 3
/// confirm_min_age_secs = 86400
/// outage_ratio         = 0.25
/// on_confirmed         = "audit"
/// retain_disappeared   = true
/// skip_recently_seen   = true
/// registries           = []
/// ```
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpstreamAuditConfig {
    /// Nothing sweeps unless asked: scheduled outbound traffic to third
    /// parties is opt-in, like `[vulnerability_scan]`.
    #[serde(default)]
    pub enabled: bool,
    /// Seconds between sweeps. Default six hours; floor five minutes.
    #[serde(default = "default_audit_interval_secs")]
    pub interval_secs: u64,
    /// Consecutive valid-sweep misses a disappearance must survive.
    #[serde(default = "default_confirm_after")]
    pub confirm_after: u32,
    /// …and at least this long since the first miss. Both floors must clear.
    #[serde(default = "default_confirm_min_age_secs")]
    pub confirm_min_age_secs: u64,
    /// Above this fraction of a registry's probed packages missing, the
    /// sweep is void for that registry: an outage, not an unpublish.
    #[serde(default = "default_outage_ratio")]
    pub outage_ratio: f64,
    /// `"audit"` (report and hold) or `"block"` (also refuse on the wire —
    /// RFC 0014 phase 6, refused until it lands).
    #[serde(default = "default_on_confirmed")]
    pub on_confirmed: String,
    /// Hold confirmed artifacts back from TTL, idle and keep-latest-N
    /// eviction (not from the LRU size cap), and pin their metadata.
    #[serde(default = "default_true_audit")]
    pub retain_disappeared: bool,
    /// A package re-cached from upstream since the last sweep started was
    /// present then; skip its probe.
    #[serde(default = "default_true_audit")]
    pub skip_recently_seen: bool,
    /// Empty = every `proxy`/`hybrid` registry; else only these names.
    #[serde(default)]
    pub registries: Vec<String>,
}

impl Default for UpstreamAuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_audit_interval_secs(),
            confirm_after: default_confirm_after(),
            confirm_min_age_secs: default_confirm_min_age_secs(),
            outage_ratio: default_outage_ratio(),
            on_confirmed: default_on_confirmed(),
            retain_disappeared: true,
            skip_recently_seen: true,
            registries: Vec::new(),
        }
    }
}

/// The floor under `[upstream_audit].interval_secs`: below five minutes a
/// sweep is an unintentional denial of service against a third party's
/// registry, from a config typo (RFC 0014 §4.4).
pub const MIN_UPSTREAM_AUDIT_INTERVAL_SECS: u64 = 300;

// ── Cache coherence ───────────────────────────────────────────────────────────

fn default_coherence_interval_secs() -> u64 {
    86_400
}

/// Periodic collection of storage blobs nothing references.
///
/// An artifact is cached in two steps — the bytes, then the row that points at
/// them. A process killed between the two leaves a blob no request can reach and
/// **no eviction strategy will ever consider**, because every strategy reads the
/// table the row is missing from. This is the sweep that collects those.
///
/// ```toml
/// [cache_coherence]
/// enabled       = true
/// interval_secs = 86400          # daily
/// ```
///
/// # Off by default
///
/// It deletes data on a timer with nobody watching, so it is opt-in — the rule
/// every other destructive policy in this file follows. A deployment that never
/// enables it can still sweep on demand through
/// `POST /api/v1/admin/registries/{registry}/coherence`.
///
/// # Why the interval is not short
///
/// Two costs and one safety property push the same way. Each run lists **every
/// cached object** of every registry (a paginated `ListObjectsV2`, or a full
/// directory walk) — that is not a query to run every minute. And a blob is only
/// deleted if the *previous* run saw it orphaned too, so the interval is also
/// the grace window a cache write in flight gets: shortening it buys faster
/// collection of bytes nobody can reach, at the cost of narrowing the margin on
/// the one thing this must never delete.
#[derive(Debug, Deserialize)]
pub struct CacheCoherenceConfig {
    /// Enable the periodic background sweep.
    #[serde(default)]
    pub enabled: bool,
    /// Seconds between sweeps. Defaults to one day.
    #[serde(default = "default_coherence_interval_secs")]
    pub interval_secs: u64,
}

// ── Limits ────────────────────────────────────────────────────────────────────

/// Upload size limits.
///
/// ```toml
/// [limits]
/// max_artifact_size_bytes = 524288000  # 500 MiB
/// versions_per_page       = 100        # rows in one package-detail answer
/// packages_per_page       = 20         # rows in one catalog answer
/// ```
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// Maximum artifact size for proxy downloads and local publishes.
    /// Defaults to 500 MiB when absent.
    pub max_artifact_size_bytes: Option<u64>,
    /// How many versions `GET /api/v1/explore/packages/{registry}/{name}`
    /// returns in one answer.
    ///
    /// Two things at once, deliberately: the number a caller that asks for no
    /// `per_page` gets, **and** the most any caller may ask for. A ceiling and a
    /// default expressed as two keys would let them contradict each other, and
    /// the question an operator actually has is one question — how much of a
    /// version list this server is willing to build, hold in memory and
    /// serialise for one request. `@babel/plugin-transform-runtime` has 169
    /// versions and the enrichment behind each row is a database read.
    ///
    /// The console asks for the number of rows it draws, which is its own
    /// business and smaller than this; it reads back the `per_page` the server
    /// actually applied rather than assuming it got what it asked for.
    pub versions_per_page: u64,
    /// How many packages `GET /api/v1/explore/packages` returns in one answer.
    ///
    /// The same two readings as `versions_per_page` — the unasked-for default
    /// and the ceiling — for the other list, and a separate key because the two
    /// are not the same question. A catalog row is a name and a few counts; 20
    /// of them is a screenful, which is why that is the default and has always
    /// been what the console drew. A version row costs a vulnerability read and
    /// a licence read. An operator sizing a screen should not be sizing a query
    /// at the same time.
    ///
    /// The console does **not** ask for a number here, unlike the version table:
    /// the catalog *is* the list, so the operator's number is the right one, and
    /// the console reads it back out of the answer to size its pager.
    pub packages_per_page: u64,
}

/// Re-exported from `core`, which owns it because `HotConfig` has to answer with
/// the same number when a test or an embedder builds one without a config file.
/// A second literal here would be a second source of truth for one default.
pub use batlehub_core::services::hot_config::{
    DEFAULT_PACKAGES_PER_PAGE, DEFAULT_VERSIONS_PER_PAGE,
};

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_artifact_size_bytes: None,
            versions_per_page: DEFAULT_VERSIONS_PER_PAGE,
            packages_per_page: DEFAULT_PACKAGES_PER_PAGE,
        }
    }
}

impl AppConfig {
    /// The effective reverse-proxy trust list, or `None` when no policy is
    /// configured anywhere.
    ///
    /// `[server].trusted_proxies` is authoritative; the deprecated
    /// `[ip_blocking].trusted_proxies` is the fallback so existing configs keep
    /// working unchanged. An empty `[ip_blocking].trusted_proxies` does *not*
    /// count as a policy: it is that section's serde default, so it carries no
    /// operator intent (`[server].trusted_proxies = []`, being an `Option`,
    /// does — it means "trust nobody").
    pub fn effective_trusted_proxies(&self) -> Option<&[String]> {
        if let Some(list) = self.server.trusted_proxies.as_deref() {
            return Some(list);
        }
        self.ip_blocking
            .as_ref()
            .map(|c| c.trusted_proxies.as_slice())
            .filter(|l| !l.is_empty())
    }

    /// True when the deprecated `[ip_blocking].trusted_proxies` is the list
    /// actually in force (i.e. `[server].trusted_proxies` is absent).
    pub fn uses_deprecated_trusted_proxies(&self) -> bool {
        self.server.trusted_proxies.is_none()
            && self
                .ip_blocking
                .as_ref()
                .is_some_and(|c| !c.trusted_proxies.is_empty())
    }

    /// True when both trusted-proxy keys carry a list, so `[server]` shadows the
    /// deprecated one (surfaced as a warning, see [`AppConfig::warnings`]).
    pub fn shadows_deprecated_trusted_proxies(&self) -> bool {
        self.server.trusted_proxies.is_some()
            && self
                .ip_blocking
                .as_ref()
                .is_some_and(|c| !c.trusted_proxies.is_empty())
    }

    // ── Host-based routing ────────────────────────────────────────────────────

    /// The `base_domain` wildcard hosts hang off, when wildcard derivation is on.
    fn wildcard_base_domain(&self) -> Option<&str> {
        self.subdomain_routing
            .as_ref()
            .filter(|s| s.enabled)
            .and_then(|s| s.base_domain.as_deref())
    }

    /// Scheme used when advertising registry public URLs. Never affects routing.
    pub fn subdomain_scheme(&self) -> &str {
        self.subdomain_routing
            .as_ref()
            .map_or("https", |s| s.scheme.as_str())
    }

    /// Every `host -> registry` binding this config declares, explicit `hosts`
    /// entries before wildcard-derived ones.
    ///
    /// Materialised once at config-load time so a request-time lookup is a single
    /// hash lookup with no suffix parsing. Registry names that are not usable DNS
    /// labels simply contribute no wildcard binding (and a warning); duplicates
    /// across registries are rejected by [`AppConfig::validate`], not here.
    pub fn registry_host_bindings(&self) -> Vec<RegistryHostBinding> {
        let base_domain = self.wildcard_base_domain();
        let mut bindings = Vec::new();
        for registry in &self.registries {
            for host in &registry.hosts {
                let normalised = normalise_host(host);
                if normalised.is_empty() {
                    continue;
                }
                bindings.push(RegistryHostBinding {
                    host: normalised,
                    registry: registry.name.clone(),
                    explicit: true,
                });
            }
        }
        if let Some(base) = base_domain {
            for registry in &self.registries {
                if let Some(host) = wildcard_host(&registry.name, base) {
                    bindings.push(RegistryHostBinding {
                        host,
                        registry: registry.name.clone(),
                        explicit: false,
                    });
                }
            }
        }
        bindings
    }

    /// True when any host-based routing is configured — wildcard derivation is
    /// enabled, or some registry declares `hosts`.
    ///
    /// This is the trigger for the strict proxy-trust requirement: once a header
    /// selects a *registry*, a deployment with no stated trust policy is not a
    /// state we let it reach.
    pub fn host_routing_configured(&self) -> bool {
        self.subdomain_routing.as_ref().is_some_and(|s| s.enabled)
            || self.registries.iter().any(|r| !r.hosts.is_empty())
    }

    /// The preferred public URL of each registry that has one: the first explicit
    /// host when present, otherwise the wildcard host, prefixed with
    /// [`AppConfig::subdomain_scheme`]. This is what the API and UI advertise.
    pub fn registry_public_urls(&self) -> Vec<(String, String)> {
        let scheme = self.subdomain_scheme();
        let base_domain = self.wildcard_base_domain();
        self.registries
            .iter()
            .filter_map(|registry| {
                let host = registry
                    .hosts
                    .iter()
                    .map(|h| normalise_host(h))
                    .find(|h| !h.is_empty())
                    .or_else(|| base_domain.and_then(|b| wildcard_host(&registry.name, b)))?;
                Some((registry.name.clone(), format!("{scheme}://{host}")))
            })
            .collect()
    }

    /// Registry names that are reachable *only* by host (`path_routing = false`).
    pub fn host_only_registries(&self) -> Vec<String> {
        self.registries
            .iter()
            .filter(|r| !r.path_routing)
            .map(|r| r.name.clone())
            .collect()
    }

    /// Non-fatal configuration problems, a sibling of [`AppConfig::validate`].
    ///
    /// `validate` refuses to start; this reports states that degrade instead —
    /// a shadowed deprecated key, a permissive default left in place. Emitted as
    /// `tracing::warn!` at startup and on every reload, and served from
    /// `GET /api/v1/admin/config/warnings` so an operator sees them without
    /// grepping logs.
    pub fn warnings(&self) -> Vec<ConfigWarning> {
        let mut out = Vec::new();
        self.proxy_trust_warnings(&mut out);
        self.subdomain_warnings(&mut out);
        self.cors_warnings(&mut out);
        self.license_gate_warnings(&mut out);
        self.readme_warnings(&mut out);
        self.upstream_detail_warnings(&mut out);
        self.search_warnings(&mut out);
        self.retention_warnings(&mut out);
        self.signed_url_warnings(&mut out);
        self.require_signed_release_warnings(&mut out);
        self.vsx_signing_warnings(&mut out);
        self.forge_warnings(&mut out);
        self.security_warnings(&mut out);
        self.upstream_audit_warnings(&mut out);
        self.tiered_policy_warnings(&mut out);
        self.dry_run_warnings(&mut out);
        self.coherence_warnings(&mut out);
        self.sdkman_warnings(&mut out);
        self.flag_source_warnings(&mut out);
        self.air_gap_warnings(&mut out);
        out
    }

    /// RFC 0010 §4.5: SDKMAN versions its API in the path, so an `upstreams`
    /// entry without the `/2` is more likely a typo than a choice — but it
    /// is not ours to reject, because an operator pointing at a mirror may
    /// have mounted it anywhere.
    fn sdkman_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, reg) in self.registries.iter().enumerate() {
            if reg.registry_type != batlehub_core::entities::RegistryKind::Sdkman.as_str() {
                continue;
            }
            for (i, upstream) in reg.upstreams.iter().enumerate() {
                let path = upstream.trim_end_matches('/');
                if path.ends_with("/2") {
                    continue;
                }
                out.push(ConfigWarning::new(
                    warnings::SDKMAN_UPSTREAM_WITHOUT_API_VERSION,
                    format!("registries[{index}].upstreams[{i}]"),
                    format!(
                        "registry '{}': sdkman upstream '{upstream}' does not end in '/2'. \
                         SDKMAN's candidates API is versioned in the path \
                         (https://api.sdkman.io/2); the URL is served as given, so `sdk list` \
                         will answer 404 if this is a typo",
                        reg.name
                    ),
                ));
            }
        }
    }

    /// The periodic orphan sweep's grace window is its interval — see
    /// [`warnings::COHERENCE_INTERVAL_TOO_SHORT`].
    fn coherence_warnings(&self, out: &mut Vec<ConfigWarning>) {
        /// Below this, the gap between two sweeps stops being a comfortable
        /// margin over a cache write's store-then-record window. Five minutes:
        /// long enough that a slow multi-gigabyte upload finishing its second
        /// step is never the thing being collected.
        const MIN_COMFORTABLE_INTERVAL_SECS: u64 = 300;

        let Some(coh) = self.cache_coherence.as_ref().filter(|c| c.enabled) else {
            return;
        };
        if coh.interval_secs >= MIN_COMFORTABLE_INTERVAL_SECS {
            return;
        }
        out.push(ConfigWarning::new(
            warnings::COHERENCE_INTERVAL_TOO_SHORT,
            "cache_coherence.interval_secs",
            format!(
                "the orphan sweep runs every {}s. A blob is deleted only if the previous sweep \
                 also saw it orphaned, so that interval is the whole margin a cache write in \
                 flight gets between storing its bytes and recording its row — and each run also \
                 lists every cached object of every registry. Consider {MIN_COMFORTABLE_INTERVAL_SECS}s \
                 or more; the on-demand endpoint covers a one-off sweep.",
                coh.interval_secs,
            ),
        ));
    }

    /// RFC 0015 §4.7's shadow warnings, on **every** reload.
    ///
    /// Unlike [`Self::tiered_policy_warnings`], whose entries are legal configs
    /// that do nothing, every entry here is a legal config that is *actively
    /// not enforcing something*. §4.7 asks for it by name: "every reload logs a
    /// warning naming each node in grant dry-run and its expiry, and the
    /// config-warnings endpoint carries it, so it appears on the Config Reload
    /// page rather than only in a log nobody tails".
    fn dry_run_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            if let Some(shadow) = &registry.grants_shadow {
                out.push(ConfigWarning::new(
                    warnings::GRANTS_IN_SHADOW,
                    format!("registries[{index}].grants_shadow"),
                    format!(
                        "registry '{}' has its grants in SHADOW until {}: every request its \
                         grants would refuse is being served, and the refusal is only recorded. \
                         This is an authorization bypass with an expiry date — check the \
                         authorization page's Shadow panel before it lapses, because on {} it \
                         starts enforcing.",
                        registry.name, shadow.until, shadow.until,
                    ),
                ));
            }
            for (n, ns) in registry.namespaces.iter().enumerate() {
                if let Some(shadow) = &ns.grants_shadow {
                    out.push(ConfigWarning::new(
                        warnings::GRANTS_IN_SHADOW,
                        format!("registries[{index}].namespaces[{n}].grants_shadow"),
                        format!(
                            "registry '{}', namespace \"{}\" has its grants in SHADOW until {}: \
                             every request they would refuse is being served, and the refusal is \
                             only recorded.",
                            registry.name, ns.match_prefix, shadow.until,
                        ),
                    ));
                }
                if ns.versioning.as_ref().is_some_and(|v| v.dry_run) {
                    out.push(ConfigWarning::new(
                        warnings::VERSIONING_IN_DRY_RUN,
                        format!("registries[{index}].namespaces[{n}].versioning"),
                        format!(
                            "registry '{}', namespace \"{}\" evaluates its versioning policy and \
                             does not enforce it: a badly-named, duplicate or out-of-order \
                             version is accepted and only recorded.",
                            registry.name, ns.match_prefix,
                        ),
                    ));
                }
            }
            if registry.versioning.as_ref().is_some_and(|v| v.dry_run) {
                out.push(ConfigWarning::new(
                    warnings::VERSIONING_IN_DRY_RUN,
                    format!("registries[{index}].versioning"),
                    format!(
                        "registry '{}' evaluates its versioning policy and does not enforce it: \
                         a badly-named, duplicate or out-of-order version is accepted and only \
                         recorded.",
                        registry.name,
                    ),
                ));
            }
        }
    }

    /// RFC 0015 §4.9's warnings for the tiered-policy blocks.
    ///
    /// Every one of these is a **legal configuration that does nothing**, which
    /// is the category §4.9 reserves warnings for: a rejection would break an
    /// upgrade or refuse a config that is merely redundant, while silence leaves
    /// an operator believing a setting is in force. The rejections live at
    /// config load in `server/src/grants.rs`, beside the namespace checks they
    /// extend.
    fn tiered_policy_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            let registry_visibility = registry.visibility.unwrap_or_default();
            Self::registry_policy_warnings(index, registry, registry_visibility, out);
            for (n, ns) in registry.namespaces.iter().enumerate() {
                Self::namespace_policy_warnings(index, n, registry, ns, registry_visibility, out);
            }
        }
    }

    /// The registry-tier half of [`Self::tiered_policy_warnings`].
    fn registry_policy_warnings(
        index: usize,
        registry: &RegistryConfig,
        registry_visibility: batlehub_core::entities::Visibility,
        out: &mut Vec<ConfigWarning>,
    ) {
        let path = |suffix: &str| format!("registries[{index}].{suffix}");

        // `prerelease_visibility` on a registry that publishes nothing.
        if registry.prerelease_visibility.is_some() && registry.mode == RegistryMode::Proxy {
            out.push(ConfigWarning::new(
                warnings::PRERELEASE_VISIBILITY_PROXY_MODE,
                path("prerelease_visibility"),
                format!(
                    "registry '{}' is in proxy mode and publishes nothing, so \
                     prerelease_visibility has no versions of its own to apply to. Accepted \
                     rather than refused because [registries.beta_channel] carries no mode \
                     restriction today and translates into this setting — refusing it would \
                     stop an existing instance from booting.",
                    registry.name,
                ),
            ));
        }

        // A pre-release audience wider than the release audience.
        if let Some(pre) = registry
            .prerelease_visibility
            .filter(|p| *p < registry_visibility)
        {
            out.push(ConfigWarning::new(
                warnings::PRERELEASE_VISIBILITY_WIDER,
                path("prerelease_visibility"),
                format!(
                    "registry '{}' shows pre-releases to a WIDER audience ({pre}) than \
                     releases ({registry_visibility}). Legal, and almost always a typo: the \
                     setting exists to do the opposite.",
                    registry.name,
                ),
            ));
        }

        if let Some(versioning) = &registry.versioning {
            Self::versioning_warnings(
                versioning,
                registry.grants.as_ref(),
                &path("versioning"),
                &format!("registry '{}'", registry.name),
                out,
            );
        }
    }

    /// The namespace-tier half of [`Self::tiered_policy_warnings`], for one
    /// namespace of one registry.
    ///
    /// `registry_visibility` is passed in rather than re-derived: it is the
    /// default a namespace inherits when it declares no visibility of its own,
    /// and computing it twice is how the two tiers would come to disagree.
    fn namespace_policy_warnings(
        index: usize,
        n: usize,
        registry: &RegistryConfig,
        ns: &NamespaceConfig,
        registry_visibility: batlehub_core::entities::Visibility,
        out: &mut Vec<ConfigWarning>,
    ) {
        use batlehub_core::entities::Visibility;

        let ns_path = |suffix: &str| format!("registries[{index}].namespaces[{n}].{suffix}");
        let node = format!(
            "registry '{}', namespace \"{}\"",
            registry.name, ns.match_prefix
        );

        // Grants decided who; nothing decided how wide.
        let has_grants = ns.grants.as_ref().is_some_and(|g| !g.is_empty());
        if has_grants && ns.visibility.is_none() && registry_visibility == Visibility::Public {
            out.push(ConfigWarning::new(
                warnings::NAMESPACE_GRANTS_WITHOUT_VISIBILITY,
                ns_path("grants"),
                format!(
                    "{node} names who may reach it but leaves its packages readable by \
                     everyone: the registry default is public and this namespace sets no \
                     visibility. Grants only widen (§4.3) — they cannot narrow the \
                     audience a package already has. Set visibility on the namespace if \
                     the grants were meant to be the whole answer.",
                ),
            ));
        }

        let ns_visibility = ns.visibility.unwrap_or(registry_visibility);
        if let Some(pre) = ns.prerelease_visibility.filter(|p| *p < ns_visibility) {
            out.push(ConfigWarning::new(
                warnings::PRERELEASE_VISIBILITY_WIDER,
                ns_path("prerelease_visibility"),
                format!(
                    "{node} shows pre-releases to a WIDER audience ({pre}) than \
                     releases ({ns_visibility}). Legal, and almost always a typo.",
                ),
            ));
        }

        if let Some(versioning) = &ns.versioning {
            Self::versioning_warnings(
                versioning,
                ns.grants.as_ref(),
                &ns_path("versioning"),
                &node,
                out,
            );
        }
    }

    /// The two `versioning` warnings, at whichever tier declared the block.
    fn versioning_warnings(
        versioning: &VersioningPolicy,
        grants: Option<&std::collections::HashMap<String, Vec<String>>>,
        path: &str,
        node: &str,
        out: &mut Vec<ConfigWarning>,
    ) {
        // An `always` node whose grants hand out a verb it makes inert.
        if versioning.immutable == Immutable::Always {
            let overwrites = grants.is_some_and(|g| {
                g.values()
                    .flatten()
                    .any(|v| v == "releases:overwrite" || v == "releases:*" || v == "*")
            });
            if overwrites {
                out.push(ConfigWarning::new(
                    warnings::IMMUTABLE_ALWAYS_WITH_OVERWRITE_GRANT,
                    path.to_owned(),
                    format!(
                        "{node} sets immutable = \"always\" and also grants \
                         releases:overwrite. Not a contradiction — immutability is a property of \
                         the resource and the verb is a property of the subject, and a replace \
                         needs both — but the grant is inert here. Whoever holds it cannot \
                         replace anything on this node.",
                    ),
                ));
            }
        }

        // `released` on a node that refuses pre-releases can never take its
        // second branch, so it is `always` written in two words.
        if versioning.immutable == Immutable::Released && !versioning.allow_prerelease {
            out.push(ConfigWarning::new(
                warnings::IMMUTABLE_RELEASED_WITHOUT_PRERELEASES,
                path.to_owned(),
                format!(
                    "{node} sets immutable = \"released\" beside allow_prerelease = false. \
                     `released` means a release is immutable and a pre-release may be replaced, \
                     and this node publishes no pre-releases — so the second branch can never be \
                     taken. This is immutable = \"always\", written in two settings.",
                ),
            ));
        }
    }

    /// `require_signed_release` on a registry that also accepts publishes,
    /// without `[registries.signing] required = true` to match.
    ///
    /// The rule judges `PackageMetadata::is_signed`, and the two halves of a
    /// hybrid registry fill that field very differently. A proxied artifact gets
    /// whatever signal its adapter can find, and `None` where there is none —
    /// which the rule skips unless `deny_missing_signature` is set. A locally
    /// published row reports `Some(false)` as soon as it holds no signature
    /// bytes, and `RequireSignedReleaseRule` denies `Some(false)`
    /// *unconditionally*: `deny_missing_signature` never enters into it.
    ///
    /// The result is that turning this rule on to gate the proxied half also
    /// refuses every local publish, and refuses it at download time — so the
    /// consumer sees a `403` about a decision the publisher could have fixed.
    /// `signing.required` refuses the same artifact at the publish request
    /// instead, which is the same policy delivered to the person who can act
    /// on it.
    ///
    /// RFC 0019 §4.3: a forge registry with no upstream token.
    ///
    /// Anonymous GitHub is 60 requests an hour, and ref resolution spends one
    /// or two of them per new ref, so an unauthenticated registry is a few
    /// minutes of use before every branch resolution starts failing. Raised
    /// for every forge kind: GitLab.com is metered too, and a self-hosted
    /// Forgejo that allows anonymous reads still sees the proxy's requests as
    /// nobody's.
    fn forge_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            let Ok(kind) = registry
                .registry_type
                .parse::<batlehub_core::entities::RegistryKind>()
            else {
                continue;
            };
            if !kind.is_forge() {
                continue;
            }
            // RFC 0019 §4.1 phase 3 — raw is off unless written, and the
            // setup snippet this registry generates (`registry suggest`, the
            // console's Setup Guide) rewrites the forge's raw host at it
            // unconditionally. So the operator hands out a snippet pointing
            // at a path that refuses, and the first person to hit it reads a
            // `403` as a permissions problem.
            if registry.raw.as_ref().is_none_or(|r| !r.enabled) {
                out.push(ConfigWarning::new(
                    warnings::FORGE_RAW_DISABLED_BUT_LINKED,
                    format!("registries[{index}].raw"),
                    format!(
                        "registry '{}' ({kind}) serves no raw content — '[registries.raw]' is \
                         absent or disabled — while the setup snippet it generates rewrites the \
                         forge's raw host at it. Add '[registries.raw]' with enabled = true, or \
                         tell the people using this registry that raw URLs go direct \
                         (RFC 0019 §4.1).",
                        registry.name
                    ),
                ));
            }
            if registry.upstream_auth.is_some() {
                continue;
            }
            out.push(ConfigWarning::new(
                warnings::FORGE_ANONYMOUS_UPSTREAM,
                format!("registries[{index}].upstream_auth"),
                format!(
                    "registry '{}' ({kind}) has no [registries.upstream_auth], so every request \
                     to the forge is anonymous. GitHub allows 60 anonymous API requests an hour \
                     and ref resolution spends one or two per new branch or tag, so an \
                     unauthenticated forge registry runs out of budget within minutes of real \
                     use (RFC 0019 §4.2). Configure a token; the proxy fingerprints it for the \
                     shared rate-limit budget and never logs it.",
                    registry.name
                ),
            ));
        }
    }

    /// Only local and hybrid registries are considered: a proxy-mode registry
    /// accepts no publishes, so there is no second half to disagree with.
    /// RFC 0020 §4.3: a signing key on a registry that never publishes.
    fn vsx_signing_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            if registry.mode == RegistryMode::Proxy && registry.vsx_signing.is_some() {
                out.push(ConfigWarning::new(
                    warnings::VSX_SIGNING_PROXY_MODE,
                    format!("registries[{index}].vsx_signing"),
                    format!(
                        "registry '{}' is in proxy mode with a [registries.vsx_signing] key: \
                         nothing is published there, so the key signs nothing. An upstream's \
                         signature is relayed whether or not a key is configured.",
                        registry.name
                    ),
                ));
            }
        }
    }

    fn require_signed_release_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            if registry.mode == RegistryMode::Proxy {
                continue;
            }
            if registry.signing.as_ref().is_some_and(|s| s.required) {
                continue;
            }
            let Some(rule_index) = registry
                .rules
                .iter()
                .position(|r| matches!(r, RuleConfig::RequireSignedRelease(_)))
            else {
                continue;
            };
            out.push(ConfigWarning::new(
                warnings::REQUIRE_SIGNED_RELEASE_UNSIGNED_PUBLISHES,
                format!("registries[{index}].rules[{rule_index}]"),
                format!(
                    "registry '{}' is in {:?} mode with a require_signed_release rule, but \
                     [registries.signing] does not set required = true. A locally published \
                     artifact with no X-Artifact-Signature is recorded as unsigned, and this rule \
                     denies that outright — deny_missing_signature does not apply, it governs \
                     only artifacts whose signature state is unknown. Every publish that omits \
                     the header will therefore succeed and then fail to download, with the 403 \
                     landing on the consumer. Set signing.required = true so the refusal happens \
                     at publish time instead.",
                    registry.name, registry.mode
                ),
            ));
        }
    }

    /// RFC 0012 §7: the two states where signing is configured and achieves
    /// nothing. Neither is an error — both are legal, and both are almost
    /// always a migration that stopped halfway.
    fn signed_url_warnings(&self, out: &mut Vec<ConfigWarning>) {
        let any_signed = self.registries.iter().any(|r| r.signed_downloads);

        if self.server.signed_urls.is_some() && !any_signed {
            out.push(ConfigWarning::new(
                warnings::SIGNED_URLS_UNUSED,
                "server.signed_urls",
                "a signing secret is configured but no registry sets signed_downloads = true, \
                 so nothing is signed. Set it on the registry whose downloads arrive without \
                 credentials — a Terraform mirror is the case this exists for.",
            ));
        }

        for (i, reg) in self.registries.iter().enumerate() {
            if !reg.signed_downloads || reg.rbac.anonymous.is_empty() {
                continue;
            }
            out.push(ConfigWarning::new(
                warnings::SIGNED_URLS_ANONYMOUS_STILL_GRANTED,
                format!("registries[{i}].rbac.anonymous"),
                format!(
                    "registry '{}' signs its download URLs and still grants anonymous {:?}. \
                     Signing exists so that grant can be removed; while it stands, every read \
                     on this registry is open to everybody and the signatures close nothing. \
                     Empty the anonymous grant to complete the migration.",
                    reg.name, reg.rbac.anonymous
                ),
            ));
        }
    }

    /// Prose search enabled over a store nothing writes to.
    ///
    /// Only raised when **every** registry has README capture explicitly off: a
    /// single such registry is an ordinary choice, and warning about it would put
    /// a notice on the admin panel for a configuration that works.
    fn search_warnings(&self, out: &mut Vec<ConfigWarning>) {
        if !self.search.readmes || self.registries.is_empty() {
            return;
        }
        let any_capture = self
            .registries
            .iter()
            .any(|r| r.readme.as_ref().is_none_or(|c| c.enabled));
        if any_capture {
            return;
        }
        out.push(ConfigWarning::new(
            warnings::SEARCH_READMES_NOTHING_STORED,
            "search.readmes",
            "[search] readmes = true, but every registry has [registries.readme] enabled = false. \
             The index will be built and stay empty: nothing is ever stored to put in it, so the \
             search box gains an option that can only answer 'no package here says that'.",
        ));
    }

    /// Retention armed to actually do something.
    ///
    /// Raised on every reload for as long as the configuration stands, which is
    /// the point (RFC 0016 §4.6): unlike the inert-block warnings above, these
    /// are not saying a setting does nothing — they are saying it works, and
    /// that what it destroys does not come back.
    fn retention_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            let Some(ret) = &registry.retention else {
                continue;
            };
            if ret.dry_run {
                continue;
            }
            let path = format!("registries[{index}].retention");

            if let Some(days) = ret.tombstone_detail_for_days {
                out.push(ConfigWarning::new(
                    warnings::RETENTION_COMPACTION_LIVE,
                    path.clone(),
                    format!(
                        "registry '{}' will strip the detail of every tombstone deleted more than \
                         {days} days ago: the checksum, publisher, signature and index metadata \
                         of those versions are discarded and cannot be recovered. The coordinate \
                         claim is kept — a compacted tombstone still refuses a re-publish — and \
                         there is no setting that removes it. Set dry_run = true to report \
                         without stripping.",
                        registry.name,
                    ),
                ));
            }

            if !ret.reclaims_anything() {
                continue;
            }

            // The loud one. Ordered before the general reclamation warning so an
            // operator reading a list top-down meets the dangerous configuration
            // before the merely destructive one.
            if ret.keep_if_pulled_days.is_none() {
                out.push(ConfigWarning::new(
                    warnings::RETENTION_NO_PULL_VETO,
                    path.clone(),
                    format!(
                        "registry '{}' will reclaim locally published versions WITHOUT consulting \
                         the download signal: dry_run is off and keep_if_pulled_days is unset. A \
                         version the whole estate is pinned to will be destroyed the moment it \
                         falls outside the other conditions, and the first anyone hears of it is \
                         a build failing against a lockfile that resolved yesterday. Set \
                         keep_if_pulled_days so that whatever is actually being used stays.",
                        registry.name,
                    ),
                ));
            }

            out.push(ConfigWarning::new(
                warnings::RETENTION_RECLAMATION_LIVE,
                path,
                format!(
                    "registry '{}' will reclaim locally published versions on its next retention \
                     run. Unlike cache eviction this destroys the only copy — there is no \
                     upstream to re-fetch a locally published artifact from. The coordinates stay \
                     spent either way. Set dry_run = true to report without reclaiming.",
                    registry.name,
                ),
            ));
        }
    }

    /// A `[registries.upstream_detail]` block that cannot do what it says.
    ///
    /// Only raised for blocks the operator **wrote down**, for the same reason
    /// [`Self::readme_warnings`] is: the discovery read is on by default, so
    /// warning about the implicit default would put a notice on the admin panel
    /// for every `local`-mode registry in every deployment.
    fn upstream_detail_warnings(&self, out: &mut Vec<ConfigWarning>) {
        use batlehub_core::entities::{RegistryKind, UpstreamDetailSupport};

        for (index, registry) in self.registries.iter().enumerate() {
            let Some(detail) = &registry.upstream_detail else {
                continue;
            };
            if !detail.enabled {
                continue;
            }
            let path = format!("registries[{index}].upstream_detail");

            if registry.mode == RegistryMode::Local {
                out.push(ConfigWarning::new(
                    warnings::UPSTREAM_DETAIL_LOCAL_MODE,
                    path,
                    format!(
                        "registry '{}' is in local mode, so there is no upstream to ask. The \
                         block is accepted and inert: the package page is already complete from \
                         the versions published here.",
                        registry.name,
                    ),
                ));
                continue;
            }

            if let Ok(kind) = registry.registry_type.parse::<RegistryKind>() {
                if let UpstreamDetailSupport::None(reason) = kind.upstream_detail() {
                    out.push(ConfigWarning::new(
                        warnings::UPSTREAM_DETAIL_UNSUPPORTED_KIND,
                        path,
                        format!(
                            "registry '{}' has type '{kind}', which cannot be asked about a \
                             package — {reason}. The block is accepted and inert: the detail page \
                             answers from local rows only.",
                            registry.name,
                        ),
                    ));
                    continue;
                }
            }

            // An estate with no route off site is a supported deployment, so
            // this is a warning about what the operator will observe — one
            // failed attempt per TTL — rather than a refusal to start.
            if registry.upstreams.iter().all(|u| u.trim().is_empty())
                && registry
                    .registry_type
                    .parse::<RegistryKind>()
                    .is_ok_and(|k| k.requires_explicit_upstream_in_proxy_mode())
            {
                out.push(ConfigWarning::new(
                    warnings::UPSTREAM_DETAIL_NO_UPSTREAM,
                    path,
                    format!(
                        "registry '{}' has no upstream configured, so every discovery read will \
                         fail. The page falls back to local rows and says the upstream could not \
                         be reached; set enabled = false to stop it trying.",
                        registry.name,
                    ),
                ));
            }
        }
    }

    /// A `[registries.readme]` block that cannot do what it says.
    ///
    /// Only raised for blocks the operator **wrote down**. README capture is on
    /// by default (RFC 0007 §4.1), so warning about the implicit default would
    /// put a notice on the admin panel for every `maven` and every `github`
    /// registry in every deployment — noise, and the operator expressed no
    /// belief to correct. A block written by hand is a belief.
    fn readme_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            let Some(readme) = &registry.readme else {
                continue;
            };
            if !readme.enabled {
                continue;
            }
            let Ok(kind) = registry
                .registry_type
                .parse::<batlehub_core::entities::RegistryKind>()
            else {
                // An unknown type is a hard error from `validate()`; nothing
                // useful to say about its README support here.
                continue;
            };
            let path = format!("registries[{index}].readme");
            let support = kind.readme_support();

            if let batlehub_core::entities::ReadmeSupport::None(reason) = support {
                out.push(ConfigWarning::new(
                    warnings::README_UNSUPPORTED_TYPE,
                    path,
                    format!(
                        "registry '{}' has type '{kind}', which carries no README — {reason}. The \
                         block is accepted and inert: nothing will ever be stored for this \
                         registry, and the package page will say so rather than showing an empty \
                         panel.",
                        registry.name,
                    ),
                ));
                continue;
            }

            if !readme.from_archive {
                continue;
            }

            if !support.reads_the_archive() {
                out.push(ConfigWarning::new(
                    warnings::README_FROM_ARCHIVE_INERT,
                    path,
                    format!(
                        "registry '{}' has type '{kind}', whose README arrives in a metadata \
                         document the proxy already fetches. 'from_archive' is accepted and \
                         inert here — the artifact is never opened for it, and READMEs are \
                         stored either way.",
                        registry.name,
                    ),
                ));
            } else if registry.firewall_only {
                out.push(ConfigWarning::new(
                    warnings::README_FROM_ARCHIVE_FIREWALL_ONLY,
                    path,
                    format!(
                        "registry '{}' is firewall_only, so artifacts are streamed without ever \
                         being cached and there is nothing for 'from_archive' to read. {}",
                        registry.name,
                        if support.answers_for_unheld_versions() {
                            "Its metadata-borne READMEs still work; the archive-borne fallback \
                             never will."
                        } else {
                            "This registry type has no other source, so no README will ever be \
                             stored for it."
                        },
                    ),
                ));
            }
        }
    }

    /// A `license_gate` on a registry type with no manifest parser.
    ///
    /// Licence extraction covers five of the twenty-one registry types
    /// (`LICENSE_EXTRACTION_TYPES`). On the other sixteen the licence is
    /// permanently unknown, so the gate either never fires or refuses
    /// everything — and which of those it does is `allow_unknown`. Both states
    /// are silent at runtime: nothing errors, the config is valid, and the rule
    /// is listed in the policy like any other.
    ///
    /// This is the same shape as the gates RFC 0004-bis §1 is about — a rule
    /// reporting the same green whether or not it can observe the condition it
    /// governs — so it is named at the point where the operator wrote it down.
    fn license_gate_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            let has_parser = LICENSE_EXTRACTION_TYPES.contains(&registry.registry_type.as_str());
            // The licence is recorded as a side effect of SBOM generation, so
            // SBOM being off makes even a supported registry type permanently
            // unknown. Checked first because it is the one an operator is most
            // likely to hit: the type is right, the rule is right, and nothing
            // happens.
            let sbom_on = registry.sbom.as_ref().is_some_and(|s| s.enabled);

            for (rule_index, rule) in registry.rules.iter().enumerate() {
                let RuleConfig::LicenseGate(cfg) = rule else {
                    continue;
                };
                let path = format!("registries[{index}].rules[{rule_index}]");
                let supported = LICENSE_EXTRACTION_TYPES.join(", ");

                if !sbom_on {
                    out.push(ConfigWarning::new(
                        warnings::LICENSE_GATE_SBOM_DISABLED,
                        path,
                        format!(
                            "registry '{}' has a license_gate but no enabled [registries.sbom] \
                             block. The licence is read out of the archive during SBOM \
                             generation, so with SBOM off nothing is ever extracted and this \
                             rule sees an unknown licence for every version — it will {}. Add \
                             [registries.sbom] with enabled = true.",
                            registry.name,
                            if cfg.block && !cfg.allow_unknown {
                                "therefore refuse every download"
                            } else {
                                "therefore never deny anything"
                            },
                        ),
                    ));
                    continue;
                }

                if has_parser {
                    continue;
                }

                if cfg.block && !cfg.allow_unknown {
                    out.push(ConfigWarning::new(
                        warnings::LICENSE_GATE_DENIES_EVERYTHING,
                        path,
                        format!(
                            "registry '{}' has type '{}', which has no manifest parser, so the \
                             licence of every version is unknown. With block = true and \
                             allow_unknown = false this rule refuses every download from this \
                             registry. Licence extraction currently covers: {supported}. Set \
                             allow_unknown = true, or remove the rule from this registry.",
                            registry.name, registry.registry_type,
                        ),
                    ));
                } else {
                    out.push(ConfigWarning::new(
                        warnings::LICENSE_GATE_NO_EXTRACTOR,
                        path,
                        format!(
                            "registry '{}' has type '{}', which has no manifest parser, so the \
                             licence of every version is unknown and this rule never denies \
                             anything. Licence extraction currently covers: {supported}. The \
                             allow/deny lists here have no effect — remove the rule, or keep it \
                             only as a record of intent.",
                            registry.name, registry.registry_type,
                        ),
                    ));
                }
            }
        }
    }

    /// `cors_allowed_origins = ["*"]` reopens what 1.1.0 closed by default. It is
    /// a legitimate choice for a public mirror, so this warns rather than
    /// refusing — but an operator who copied the wildcard from an old config
    /// without meaning to should see it named.
    fn cors_warnings(&self, out: &mut Vec<ConfigWarning>) {
        let origins = self
            .server
            .cors_allowed_origins
            .as_deref()
            .unwrap_or_default();
        if !origins.iter().any(|o| o == "*") {
            return;
        }
        out.push(ConfigWarning::new(
            warnings::CORS_ANY_ORIGIN,
            "server.cors_allowed_origins",
            "'*' allows any website to make cross-origin requests to this server and read \
             the responses. Credentials are never sent cross-origin, so this is not a \
             token-theft path, but on a private network it lets a public page enumerate \
             internal package metadata through a visitor's browser. Replace it with the \
             explicit origin(s) that serve the UI unless this is a public mirror.",
        ));
    }

    /// `[subdomain_routing]` is on but a registry name cannot become a DNS label.
    /// That registry gets no wildcard host; it stays reachable by path and by any
    /// explicit `hosts` entry, so this degrades rather than fails.
    fn subdomain_warnings(&self, out: &mut Vec<ConfigWarning>) {
        if self.wildcard_base_domain().is_none() {
            return;
        }
        for (index, registry) in self.registries.iter().enumerate() {
            if is_dns_label(&registry.name) {
                continue;
            }
            out.push(ConfigWarning::new(
                warnings::SUBDOMAIN_INVALID_DNS_LABEL,
                format!("registries[{index}].name"),
                format!(
                    "registry '{}' is not a valid DNS label, so no wildcard host is derived \
                     for it. It stays reachable at /proxy/{}/… and at any explicit 'hosts' \
                     entry. Rename it to letters, digits and inner hyphens only, or give it \
                     an explicit host.",
                    registry.name, registry.name
                ),
            ));
        }
    }

    fn proxy_trust_warnings(&self, out: &mut Vec<ConfigWarning>) {
        // Entries of the deprecated key that `validate` no longer rejects (see
        // there). Only reported when that key is the list actually in force: a
        // shadowed list changes nothing, and `PROXY_TRUST_SHADOWED_DEPRECATED_KEY`
        // already tells the operator to delete it — a second warning about an
        // entry of a list that has no effect is noise.
        for entry in self
            .ip_blocking
            .as_ref()
            .filter(|_| self.uses_deprecated_trusted_proxies())
            .map(|c| c.trusted_proxies.as_slice())
            .unwrap_or_default()
        {
            if parse_trusted_proxies(std::slice::from_ref(entry)).is_err() {
                out.push(ConfigWarning::new(
                    warnings::PROXY_TRUST_INVALID_DEPRECATED_ENTRY,
                    "ip_blocking.trusted_proxies",
                    format!(
                        "'{entry}' is not an IP address or CIDR range and is ignored. Replace it \
                         with the proxy's address(es) under [server].trusted_proxies; a hostname \
                         is not resolved."
                    ),
                ));
            }
        }
        if self.shadows_deprecated_trusted_proxies() {
            out.push(ConfigWarning::new(
                warnings::PROXY_TRUST_SHADOWED_DEPRECATED_KEY,
                "ip_blocking.trusted_proxies",
                "both [server].trusted_proxies and the deprecated \
                 [ip_blocking].trusted_proxies are set; [server] wins and this list is \
                 ignored entirely. Delete it.",
            ));
        } else if self.uses_deprecated_trusted_proxies() {
            out.push(ConfigWarning::new(
                warnings::PROXY_TRUST_DEPRECATED_KEY_ONLY,
                "ip_blocking.trusted_proxies",
                "proxy trust comes from the deprecated [ip_blocking].trusted_proxies. It is \
                 honoured — and now governs the forwarded host and scheme as well as the \
                 client IP — but move the list to [server].trusted_proxies.",
            ));
        } else if self.effective_trusted_proxies().is_none() && !self.host_routing_configured() {
            // With host routing on, no list at all is a hard error (see
            // `validate`), so this branch only ever describes the legacy case.
            out.push(ConfigWarning::new(
                warnings::PROXY_TRUST_UNCONFIGURED,
                "server.trusted_proxies",
                "no trusted-proxy list is configured, so Forwarded / X-Forwarded-Host / \
                 X-Forwarded-Proto are believed from any client and decide the URLs this \
                 server advertises. Set [server].trusted_proxies to your ingress's CIDR \
                 ranges, or to [] if BatleHub is exposed directly.",
            ));
        }
    }

    /// Every JWT-validating provider must fetch its keys over a channel that
    /// cannot be rewritten in flight.
    ///
    /// `issuer_url` is where the discovery document comes from, and that document
    /// names both the `iss` the provider will go on to enforce and the `jwks_uri`
    /// whose keys it will trust. Over plain HTTP, anyone on the path chooses both
    /// — which is to say, chooses who is allowed to authenticate.
    ///
    /// Loopback is allowed unencrypted, because that is how the test suites and a
    /// local Keycloak run and there is no network to be on the path of.
    fn validate_auth_issuers(&self) -> Result<()> {
        /// Why an OIDC issuer cannot be plain HTTP.
        const DISCOVERY: &str = "The discovery document it serves decides which issuer and which \
                                 signing keys this server trusts, so it cannot travel over plain \
                                 HTTP.";
        /// Why the Kubernetes API server cannot be either.
        ///
        /// A stronger case than the OIDC one, if anything: the request carries
        /// this server's own service account token, and the *answer* decides the
        /// caller's identity. Anyone on that path both learns the token and can
        /// reply `authenticated: true` with `system:serviceaccount:…`, which
        /// `resolve_role` will map straight to `Role::Admin`.
        const TOKENREVIEW: &str = "Every TokenReview carries this server's own service account \
                                   token, and the answer decides the caller's role — so a plain \
                                   HTTP path both leaks that token and lets anyone on it grant \
                                   themselves any role this provider maps.";

        for auth in &self.auth {
            let (kind, name, key, url, why) = match auth {
                AuthConfig::Oidc(cfg) => {
                    ("oidc", &cfg.name, "issuer_url", &cfg.issuer_url, DISCOVERY)
                }
                AuthConfig::ActionsOidc(cfg) => (
                    "actions-oidc",
                    &cfg.name,
                    "issuer_url",
                    &cfg.issuer_url,
                    DISCOVERY,
                ),
                // Only when it is set. An absent `api_server` means the
                // in-cluster default built from `KUBERNETES_SERVICE_HOST`, which
                // is `https://` by construction — there is nothing to check and
                // nothing an operator could have got wrong.
                AuthConfig::Kubernetes(cfg) => match cfg.api_server.as_ref() {
                    Some(api_server) => (
                        "kubernetes",
                        &cfg.name,
                        "api_server",
                        api_server,
                        TOKENREVIEW,
                    ),
                    None => continue,
                },
                AuthConfig::Token(_) => continue,
            };
            if !is_secure_issuer_url(url) {
                bail!(
                    "[[auth]] type = \"{kind}\" name = \"{name}\": {key} '{url}' must use https. \
                     {why} (http:// is accepted for localhost and 127.0.0.1 only.)"
                );
            }
        }
        Ok(())
    }

    /// An `actions-oidc` provider names a non-blank `audience`.
    ///
    /// `ActionsOidcAuthProvider::new` already refuses a blank one, but that error
    /// travels through `provider_unavailable(…, cfg.required, e)` in
    /// `server/src/setup.rs`, and `required` defaults to `false` for this kind —
    /// so a blank `audience` logged one warning and the server came up *without*
    /// the provider. Every CI caller then resolved to anonymous and publishes
    /// started returning `403` with nothing in the configuration to point at.
    ///
    /// Checked here instead, where it is a configuration error rather than an
    /// "identity provider unreachable" one, so `required` never gets a say:
    /// `docs/guide/configuration.md` says startup fails on a blank `audience`,
    /// and this is what makes that true. `serde` already rejects the key being
    /// absent — the field has no default — so only blank needs saying.
    fn validate_actions_oidc_audience(&self) -> Result<()> {
        for auth in &self.auth {
            let AuthConfig::ActionsOidc(cfg) = auth else {
                continue;
            };
            if cfg.audience.trim().is_empty() {
                bail!(
                    "[[auth]] type = \"actions-oidc\" name = \"{}\": `audience` must not be \
                     blank. The issuer is shared by every repository on the forge, so `audience` \
                     is the only claim that says a token was minted for this deployment — \
                     without it, `iss` proves nothing more than \"some CI job somewhere\".",
                    cfg.name
                );
            }
        }
        Ok(())
    }

    /// `[[auth]]` names are unique across every provider kind.
    ///
    /// The name is not a label: it is the identity a session is attributed to.
    /// `oidc_session_owner` keys stored refresh tokens and OIDC-issued PATs on
    /// `(provider_name, username)` and documents that a non-OIDC provider named
    /// `"oidc"` "does not pass" — but nothing made that true. Two providers of
    /// different kinds sharing one name let the weaker one act as the stronger:
    /// a `type = "kubernetes" name = "corp"` service account whose TokenReview
    /// returns a username an OIDC user also has could mint 90-day personal
    /// access tokens as that user and manage the tokens they own.
    ///
    /// It is also the prefix for unmapped groups (`"k8s-prod:team-a"`), so two
    /// providers sharing a name merge their group namespaces into one — which
    /// silently widens whatever `[registries.rbac.groups]` grants.
    fn validate_auth_names(&self) -> Result<()> {
        let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for auth in &self.auth {
            let (kind, name) = match auth {
                AuthConfig::Oidc(cfg) => ("oidc", cfg.name.as_str()),
                AuthConfig::ActionsOidc(cfg) => ("actions-oidc", cfg.name.as_str()),
                AuthConfig::Kubernetes(cfg) => ("kubernetes", cfg.name.as_str()),
                // The static-token provider carries no name of its own, so it
                // has no identity to collide with.
                AuthConfig::Token(_) => continue,
            };
            if let Some(first) = seen.insert(name, kind) {
                bail!(
                    "[[auth]] name = \"{name}\" is used by both type = \"{first}\" and type = \
                     \"{kind}\". The name is what a session, a stored refresh token and an \
                     unmapped group are attributed to, so two providers sharing one are one \
                     provider as far as everything downstream is concerned — give them distinct \
                     names."
                );
            }
        }
        Ok(())
    }

    /// Fail-fast checks for signed download URLs.
    ///
    /// RFC 0012 §4.1. Every one of these is an error rather than a warning for
    /// the same reason: a registry that believes it is closed and is not, or
    /// one that believes downloads are signed and serves nothing, is a state an
    /// operator will discover from a failed install rather than from the file.
    fn validate_signed_urls(&self) -> Result<()> {
        let signed_registries: Vec<&str> = self
            .registries
            .iter()
            .filter(|r| r.signed_downloads)
            .map(|r| r.name.as_str())
            .collect();

        let Some(cfg) = self.server.signed_urls.as_ref() else {
            if !signed_registries.is_empty() {
                bail!(
                    "registries {:?} set signed_downloads = true but [server.signed_urls] is \
                     absent; add a secret, or the registry cannot serve the downloads it is \
                     closing off",
                    signed_registries
                );
            }
            return Ok(());
        };

        // Length is measured in bytes, not characters: the secret is HMAC key
        // material, and a 32-character string of multi-byte characters is fine
        // while a 20-character ASCII one is not.
        let secret = cfg.secret.trim();
        if secret.is_empty() {
            bail!(
                "[server.signed_urls].secret is empty — if it interpolates ${{VAR}}, the variable \
                 is unset in this environment"
            );
        }
        if secret.len() < batlehub_core::services::SIGNED_URL_MIN_SECRET_BYTES {
            bail!(
                "[server.signed_urls].secret is {} bytes; {} is the minimum for an HMAC-SHA256 \
                 signing key",
                secret.len(),
                batlehub_core::services::SIGNED_URL_MIN_SECRET_BYTES
            );
        }
        if cfg.ttl_seconds == 0 {
            bail!("[server.signed_urls].ttl_seconds is 0; every minted URL would be born expired");
        }
        if cfg.ttl_seconds > batlehub_core::services::SIGNED_URL_MAX_TTL_SECONDS {
            bail!(
                "[server.signed_urls].ttl_seconds is {}; the ceiling is {} so a misconfiguration \
                 cannot mint a month-long bearer credential",
                cfg.ttl_seconds,
                batlehub_core::services::SIGNED_URL_MAX_TTL_SECONDS
            );
        }
        // Enumerate the *configured* array, not `active_previous_secrets()`,
        // and skip empties inside the loop. The active list has already dropped
        // them, so its indices no longer line up with the file the operator is
        // about to go and edit: `["${UNSET}", "tooshort"]` reported
        // `previous_secrets[0]`, which is the entry that is fine.
        for (i, prev) in cfg.previous_secrets.iter().enumerate() {
            let prev = prev.trim();
            if prev.is_empty() {
                continue;
            }
            if prev.len() < batlehub_core::services::SIGNED_URL_MIN_SECRET_BYTES {
                bail!(
                    "[server.signed_urls].previous_secrets[{i}] is {} bytes; {} is the minimum. \
                     An entry that interpolates to empty is ignored, but a short one is a \
                     mistake rather than a rotation in progress",
                    prev.len(),
                    batlehub_core::services::SIGNED_URL_MIN_SECRET_BYTES
                );
            }
        }
        Ok(())
    }

    /// Fail-fast checks for host-based routing (RFC 0001 §4.3).
    ///
    /// Every condition here is one where the deployment would come up looking
    /// healthy but route wrongly — a host silently bound to the last registry
    /// that claimed it, a registry nothing can reach, the admin API shadowed by a
    /// vanity host, or routing driven by a header the server has no policy about.
    fn validate_host_routing(&self) -> Result<()> {
        self.validate_base_domain()?;
        // Syntax first, so a pasted URL is reported as such rather than as a
        // mystery collision after normalisation.
        self.validate_host_syntax()?;
        self.validate_base_domain_not_claimed()?;

        let bindings = self.registry_host_bindings();
        Self::validate_one_host_one_registry(&bindings)?;
        self.validate_every_registry_reachable(&bindings)?;

        // Routing now depends on a header, so an unstated trust policy is not a
        // state we let a deployment reach. The deprecated key counts as a policy
        // (and warns), so an existing config adopting host routing changes nothing.
        if self.host_routing_configured() && self.effective_trusted_proxies().is_none() {
            bail!(
                "host-based routing is configured ([subdomain_routing] or a registry 'hosts' \
                 entry), which routes on the Forwarded / X-Forwarded-Host header. Declare which \
                 peers may set it:\n\n\
                 \x20   [server]\n\
                 \x20   trusted_proxies = [\"10.42.0.0/16\"]   # your ingress's CIDR ranges\n\n\
                 Use trusted_proxies = [] if BatleHub is exposed directly and no proxy sits in \
                 front of it."
            );
        }

        Ok(())
    }

    /// An enabled `[subdomain_routing]` names a base domain that is one.
    fn validate_base_domain(&self) -> Result<()> {
        let Some(subdomain) = self.subdomain_routing.as_ref() else {
            return Ok(());
        };
        if !subdomain.enabled {
            return Ok(());
        }
        let base = subdomain.base_domain.as_deref().unwrap_or_default();
        if normalise_host(base).is_empty() {
            bail!(
                "[subdomain_routing]: 'enabled = true' requires a non-empty 'base_domain' \
                 (e.g. base_domain = \"hub.example.com\"); without one no wildcard host \
                 is derived and the section routes nothing"
            );
        }
        // A pasted URL normalises to something non-empty ("https://hub.example.com" ->
        // "https"), so it would otherwise flow into the wildcard hosts, the public URLs
        // and the collision checks below as a plausible-looking domain.
        validate_host_entry(base).map_err(|e| {
            anyhow::anyhow!("[subdomain_routing]: invalid 'base_domain' '{base}': {e}")
        })?;
        Ok(())
    }

    /// Every `hosts` entry is a host rather than a pasted URL.
    fn validate_host_syntax(&self) -> Result<()> {
        for registry in &self.registries {
            for host in &registry.hosts {
                validate_host_entry(host).map_err(|e| {
                    anyhow::anyhow!(
                        "registry '{}': invalid 'hosts' entry '{host}': {e}",
                        registry.name
                    )
                })?;
            }
        }
        Ok(())
    }

    /// The bare base_domain stays the main host: it serves the admin API, the
    /// SPA, /healthz and /metrics. A vanity host equal to it would rewrite all
    /// of that into one registry and hide the admin API entirely.
    fn validate_base_domain_not_claimed(&self) -> Result<()> {
        let Some(base) = self
            .subdomain_routing
            .as_ref()
            .and_then(|s| s.base_domain.as_deref())
            .map(normalise_host)
            .filter(|d| !d.is_empty())
        else {
            return Ok(());
        };
        for registry in &self.registries {
            if registry.hosts.iter().any(|h| normalise_host(h) == base) {
                bail!(
                    "registry '{}': 'hosts' entry '{base}' is the [subdomain_routing] \
                     base_domain itself; that host serves the admin API and the SPA, and \
                     binding it to a registry would hide them",
                    registry.name
                );
            }
        }
        Ok(())
    }

    /// One host, one registry. Same-registry duplicates are fine — an explicit
    /// `hosts` entry that repeats that registry's own wildcard host just wins.
    fn validate_one_host_one_registry(bindings: &[RegistryHostBinding]) -> Result<()> {
        let mut claimed: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for binding in bindings {
            match claimed.get(binding.host.as_str()) {
                Some(&owner) if owner != binding.registry => bail!(
                    "host '{}' is claimed by both registry '{owner}' and registry '{}'; \
                     a host routes to exactly one registry",
                    binding.host,
                    binding.registry
                ),
                Some(_) => {}
                None => {
                    claimed.insert(binding.host.as_str(), binding.registry.as_str());
                }
            }
        }
        Ok(())
    }

    /// A registry with neither ingress is unreachable — catch it here rather
    /// than as a stream of 404s nobody can explain.
    fn validate_every_registry_reachable(&self, bindings: &[RegistryHostBinding]) -> Result<()> {
        for registry in &self.registries {
            if registry.path_routing {
                continue;
            }
            if !bindings.iter().any(|b| b.registry == registry.name) {
                bail!(
                    "registry '{}': 'path_routing = false' leaves it with no ingress — it has \
                     no 'hosts' entry, and no wildcard host is derived for it (either \
                     [subdomain_routing] is off, or the name is not a valid DNS label). Add a \
                     host, or drop 'path_routing = false'.",
                    registry.name
                );
            }
        }
        Ok(())
    }

    /// Refuse a `[registries.retention]` block that cannot mean what it says
    /// (RFC 0016 §4.6).
    ///
    /// One §4.6 rule is **not** implemented and is not deferred by oversight: a
    /// `tombstone_detail_for` window shorter than the registry's audit-retention
    /// window. There is no configured audit-retention window in this tree to
    /// compare against — the audit trail is purged by an operator-supplied
    /// cutoff through `AccessAction::AuditPurge`, not on a schedule — so the
    /// check has no second operand. The floor below is what stands in for it:
    /// it refuses the windows short enough to strip detail an investigation is
    /// plainly still using.
    fn validate_retention(&self, registry: &RegistryConfig) -> Result<()> {
        let Some(ret) = registry.retention.as_ref() else {
            return Ok(());
        };

        // A proxy-mode registry publishes nothing locally, so it has no versions
        // to reclaim and no tombstones to compact. The block would govern an empty
        // set in silence, and the operator who wrote it meant `[registries.cache]`.
        if registry.mode == RegistryMode::Proxy {
            bail!(
                "registry '{}': [registries.retention] governs locally published versions, and a \
                 'proxy' mode registry has none — every setting in the block would apply to an \
                 empty set. For the proxy cache, use the eviction keys on [registries.cache] \
                 (idle_days, keep_latest_n, max_size_bytes)",
                registry.name
            );
        }

        // **The rule that matters most in this function.** A retention block
        // whose only keep conditions are absent reclaims *everything* on its
        // first live run: the union of vetoes is empty, so nothing vetoes.
        // `keep_yanked` does not count — it defaults to true and would make an
        // otherwise-empty block look configured while still destroying every
        // unyanked version in the registry.
        if !ret.reclaims_anything() && ret.tombstone_detail_for_days.is_none() {
            bail!(
                "registry '{}': [registries.retention] has no setting that does anything, and an \
                 empty block is the one that would reclaim every version on its first live run. \
                 Set at least one keep condition (keep_versions, keep_for_days, \
                 keep_if_pulled_days) or tombstone_detail_for_days — or omit the block entirely \
                 to keep everything forever, which is the default",
                registry.name
            );
        }

        // Every keep condition is a *window*, and a zero-length one keeps
        // nothing. Distinguishable from "unset", which is the caller declining
        // to use that condition at all, so this is a typo rather than a policy.
        for (key, value) in [
            ("keep_versions", ret.keep_versions),
            ("keep_for_days", ret.keep_for_days),
            ("keep_if_pulled_days", ret.keep_if_pulled_days),
        ] {
            if value == Some(0) {
                bail!(
                    "registry '{}': [registries.retention] {key} = 0 keeps nothing, which is not \
                     what a keep condition set to zero looks like it means. Omit the key to \
                     disable this condition",
                    registry.name
                );
            }
        }

        // A short window strips the evidence while the incident that prompted the
        // deletion is still open. Thirty days is not a considered policy — it is
        // the floor below which the setting is more likely a units mistake
        // (days read as hours) than an intent.
        const MIN_DETAIL_DAYS: u32 = 30;
        if let Some(days) = ret.tombstone_detail_for_days {
            if days == 0 {
                bail!(
                    "registry '{}': [registries.retention] tombstone_detail_for_days = 0 would \
                     strip a tombstone's detail the moment it is created, so a deletion could \
                     never be investigated. Omit the key to keep detail forever",
                    registry.name
                );
            }
            if days < MIN_DETAIL_DAYS {
                bail!(
                    "registry '{}': [registries.retention] tombstone_detail_for_days = {days} is \
                     below the {MIN_DETAIL_DAYS}-day floor. Compaction discards the checksum, \
                     publisher and metadata of a deleted version — an auditor asking what was \
                     removed within the last month should still get an answer",
                    registry.name
                );
            }
        }
        Ok(())
    }

    /// Every fail-fast check the file has to pass before the server comes up.
    ///
    /// A list of delegations rather than the checks themselves: each `validate_*`
    /// below owns one subject and states its own reasons, and the order here is
    /// the order an operator meets the errors in. Adding a check means adding a
    /// method and a line, not another branch in a function nobody can hold.
    pub fn validate(&self) -> Result<()> {
        self.validate_config_version()?;
        // Reject malformed proxy-trust entries up front: a typo here silently
        // widens or narrows which peers may set `X-Forwarded-*`, and the
        // consequence (a spoofable routing header, or generated URLs pointing at
        // the internal service host) shows up far from the config file.
        if let Some(list) = self.server.trusted_proxies.as_deref() {
            parse_trusted_proxies(list)
                .map_err(|e| anyhow::anyhow!("[server].trusted_proxies: {e}"))?;
        }
        // The deprecated `[ip_blocking].trusted_proxies` is deliberately *not*
        // validated here. Before it fed the proxy-trust policy it was parsed with
        // `.parse::<IpAddr>().ok()`, so a hostname entry (an ingress DNS name, a
        // Service name) loaded fine and was dropped. Bailing on it now would turn
        // an upgrade into a boot failure for a config that never changed; the
        // entry is dropped as before and surfaced as
        // `PROXY_TRUST_INVALID_DEPRECATED_ENTRY` instead.
        self.validate_auth_issuers()?;
        self.validate_auth_names()?;
        self.validate_actions_oidc_audience()?;
        self.validate_host_routing()?;
        self.validate_signed_urls()?;
        self.validate_page_sizes()?;
        self.validate_search()?;
        self.validate_registries()?;
        self.validate_security_globals()?;
        self.validate_upstream_audit()?;
        Ok(())
    }

    /// The file does not declare a schema this binary is too old to read.
    fn validate_config_version(&self) -> Result<()> {
        if let Some(v) = self.config_version {
            if v > CURRENT_CONFIG_VERSION {
                bail!(
                    "config_version {v} is newer than this binary supports (max {CURRENT_CONFIG_VERSION}); \
                     upgrade batlehub-server, or lower config_version if you intended to target an \
                     older schema"
                );
            }
        }
        Ok(())
    }

    /// `[limits]` page sizes are answerable and bounded.
    ///
    /// A page size of zero is a list that can never answer, and the failure
    /// would land on a page rather than at startup. The ceiling is the same
    /// argument `upstream_detail.max_versions` makes one level up: every row
    /// is built, held in memory and serialised, and these keys are the *most*
    /// any caller may ask for.
    fn validate_page_sizes(&self) -> Result<()> {
        const PER_PAGE_CEILING: u64 = 1_000;
        for (key, value, default, empties) in [
            (
                "versions_per_page",
                self.limits.versions_per_page,
                DEFAULT_VERSIONS_PER_PAGE,
                "version list",
            ),
            (
                "packages_per_page",
                self.limits.packages_per_page,
                DEFAULT_PACKAGES_PER_PAGE,
                "package catalog",
            ),
        ] {
            if value == 0 {
                bail!(
                    "[limits].{key} = 0 would return an empty {empties} to every caller; \
                     omit the key for the default of {default}"
                );
            }
            if value > PER_PAGE_CEILING {
                bail!(
                    "[limits].{key} = {value} exceeds the {PER_PAGE_CEILING} ceiling; every row \
                     in the answer is built and serialised, and this is the most one request \
                     may ask for"
                );
            }
        }
        Ok(())
    }

    /// `[search]` has somewhere to put its index and something to build it with.
    ///
    /// The index is a Postgres generated column with a GIN index. There is no
    /// other backend to put it in, and failing at startup beats a search that
    /// quietly matches nothing (RFC 0007-bis §4.5).
    /// Both spellings, because `[database] type` is documented as
    /// `postgresql` and written as `postgres` about as often. Nothing else
    /// reads this field — the adapter layer is Postgres-only — so the check
    /// exists to give an operator who wrote something else an answer here
    /// rather than a search that quietly matches nothing.
    fn validate_search(&self) -> Result<()> {
        let postgres = matches!(
            self.database.db_type.to_ascii_lowercase().as_str(),
            "postgres" | "postgresql"
        );
        if self.search.readmes && !postgres {
            bail!(
                "[search] readmes = true needs a Postgres database — the README index is a \
                 generated tsvector column with a GIN index, and [database] type = '{}' has \
                 nowhere to put it",
                self.database.db_type
            );
        }
        if self.search.text_config.trim().is_empty() {
            bail!(
                "[search] text_config must name a Postgres text search configuration (e.g. \
                 \"english\", \"simple\", \"french\"); an empty value is not one"
            );
        }
        Ok(())
    }

    /// Every `[[registries]]` entry, in the order an operator meets the errors.
    fn validate_registries(&self) -> Result<()> {
        // The set of storage backend names a registry's `storage` field may
        // reference. In single-backend mode only the implicit "default" exists;
        // in multi mode it is the declared `[[storage.backends]]` names. A
        // registry pointing at an undeclared backend is rejected here rather
        // than silently falling back to the default backend at runtime (which
        // would write artifacts to the wrong place with no warning).
        let known_backends: std::collections::HashSet<&str> = match &self.storage {
            StoragesConfig::Single(_) => std::iter::once("default").collect(),
            StoragesConfig::Multi(multi) => {
                multi.backends.iter().map(|b| b.name.as_str()).collect()
            }
        };
        // Reject duplicate registry names: every downstream map (clients,
        // policies, rules, rate-limits, storage assignments) is keyed by name,
        // so a duplicate would silently shadow the earlier registry's config
        // (last-write-wins) with no error or log.
        let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for registry in &self.registries {
            if registry.name.is_empty() {
                bail!("registry is missing a 'name' field");
            }
            if !seen_names.insert(registry.name.as_str()) {
                bail!(
                    "duplicate registry name '{}': registry names must be unique",
                    registry.name
                );
            }
            self.validate_registry_storage(registry, &known_backends)?;

            let kind: batlehub_core::entities::RegistryKind =
                registry.registry_type.parse().map_err(anyhow::Error::msg)?;
            Self::validate_registry_mode(registry, kind)?;
            self.validate_retention(registry)?;
            Self::validate_registry_upstreams(registry, kind)?;
            Self::validate_registry_path_allow(registry, kind)?;
            Self::validate_registry_release_age(registry, kind)?;
            Self::validate_registry_broker_url(registry, kind)?;
            Self::validate_registry_warm_platforms(registry, kind)?;
            Self::validate_registry_refs(registry, kind)?;
            // Beside `refs`, not inside it: `[registries.raw]` and
            // `[registries.api_reads]` are independent sections, and calling
            // them from `validate_registry_refs` meant a config that wrote
            // either one without `[registries.refs]` got no validation at all —
            // so `scripts = "denied"` loaded and fell back to `warn`, the
            // opposite of what the operator asked for.
            Self::validate_registry_raw(registry, kind)?;
            Self::validate_registry_api_reads(registry, kind)?;
            self.validate_registry_security(registry)?;
            Self::validate_registry_readme(registry)?;
            Self::validate_registry_upstream_detail(registry)?;
            Self::validate_registry_versioning(registry)?;
            Self::validate_registry_vsx_signing(registry, kind)?;
        }
        Ok(())
    }

    /// A registry's `storage` names a backend that exists.
    fn validate_registry_storage(
        &self,
        registry: &RegistryConfig,
        known_backends: &std::collections::HashSet<&str>,
    ) -> Result<()> {
        let Some(backend) = &registry.storage else {
            return Ok(());
        };
        if known_backends.contains(backend.as_str()) {
            return Ok(());
        }
        match &self.storage {
            StoragesConfig::Single(_) => bail!(
                "registry '{}': 'storage = \"{}\"' requires a multi-backend \
                 [[storage.backends]] configuration; single-backend storage has no \
                 named backends to select",
                registry.name,
                backend
            ),
            StoragesConfig::Multi(_) => bail!(
                "registry '{}': 'storage = \"{}\"' does not match any backend name in \
                 [[storage.backends]]",
                registry.name,
                backend
            ),
        }
    }

    /// The declared `mode` is one this registry kind can actually serve.
    fn validate_registry_mode(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        if matches!(registry.mode, RegistryMode::Local | RegistryMode::Hybrid)
            && !kind.supports_local_mode()
        {
            bail!(
                "registry '{}': mode 'local'/'hybrid' is not supported for {} registries (no local publish model)",
                registry.name,
                kind
            );
        }
        if registry.mode == RegistryMode::Hybrid && registry.upstreams.is_empty() {
            bail!(
                "registry '{}': hybrid mode requires at least one upstream URL",
                registry.name
            );
        }
        Ok(())
    }

    /// A proxy-mode registry of a kind with no default upstream names one.
    ///
    /// deb/rpm have no universal default upstream, so proxy mode (which would
    /// otherwise fall back to an unreachable placeholder) also requires an
    /// explicit upstream. Caught at startup instead of every fetch failing.
    fn validate_registry_upstreams(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        if registry.mode == RegistryMode::Proxy
            && registry.upstreams.is_empty()
            && kind.requires_explicit_upstream_in_proxy_mode()
        {
            bail!(
                "registry '{}': {} proxy mode requires at least one upstream URL (no default upstream exists)",
                registry.name,
                kind
            );
        }
        Ok(())
    }

    /// `path_allow` is meaningful for this kind, present where it is mandatory,
    /// and made of globs that compile.
    fn validate_registry_path_allow(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        // `path_allow` gates a raw upstream path passthrough, so it only means
        // anything for the path-addressed kinds. Accepting it silently elsewhere
        // would read as a working restriction while gating nothing.
        if !registry.path_allow.is_empty() && !kind.is_path_addressed() {
            bail!(
                "registry '{}': 'path_allow' is only supported for path-addressed registry types \
                 (deb, rpm, pacman, jetbrains, generic), not {}",
                registry.name,
                kind
            );
        }
        // A `generic` registry mirrors an arbitrary file tree on a host that may
        // serve unrelated content, so the allowlist is mandatory rather than
        // opt-in. `["**"]` is the explicit way to mirror everything.
        if kind == batlehub_core::entities::RegistryKind::Generic && registry.path_allow.is_empty()
        {
            bail!(
                "registry '{}': generic registries require a non-empty 'path_allow' allowlist \
                 (use path_allow = [\"**\"] to mirror the whole upstream deliberately)",
                registry.name
            );
        }
        for pattern in &registry.path_allow {
            glob::Pattern::new(pattern).map_err(|e| {
                anyhow::anyhow!(
                    "registry '{}': invalid path_allow glob '{pattern}': {e}",
                    registry.name
                )
            })?;
        }
        Ok(())
    }

    /// RFC 0010 §4.5, §6.7: on a toolchain kind, an age gate must say what it
    /// does with a release that has no publish date.
    ///
    /// `ReleaseAgeGateConfig::deny_missing_timestamp` defaults to `false`
    /// everywhere else, and that default was written for npm, where every
    /// version carries a timestamp and the field decides nothing. On `nodedist`
    /// it decides the gate for every release `index.tab` no longer lists — and
    /// on `sdkman`, when it lands, for every artifact, because that protocol
    /// publishes no dates at all. "Quarantine everything undated" and "exempt
    /// it" are opposite security postures; inheriting one silently is how an
    /// operator ends up believing a toolchain is quarantined when it is not.
    /// So the field is mandatory here, and only here.
    ///
    /// Namespace rule overrides (RFC 0015 §4.1) are checked too: a namespace
    /// that re-tunes the gate re-inherits the same silent default.
    fn validate_registry_release_age(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        use batlehub_core::entities::RegistryKind;
        // What an undated release *is* on each kind, for the error: on
        // `nodedist` it is the exception, on `sdkman` it is every artifact.
        let consequence = match kind {
            RegistryKind::Nodedist => {
                "A Node release that index.tab no longer lists reaches the gate with no \
                 publish date: 'true' refuses it, 'false' serves it"
            }
            RegistryKind::Sdkman => {
                "SDKMAN publishes no dates at all, so every artifact reaches the gate without \
                 one: 'true' refuses every download on this registry, 'false' makes the gate \
                 inert"
            }
            _ => return Ok(()),
        };
        let namespace_rules = registry
            .namespaces
            .iter()
            .filter_map(|ns| ns.rules.as_deref())
            .flatten();
        for rule in registry.rules.iter().chain(namespace_rules) {
            let RuleConfig::ReleaseAgeGate(cfg) = rule else {
                continue;
            };
            if cfg.deny_missing_timestamp.is_none() {
                bail!(
                    "registry '{}': a release_age_gate rule on a {} registry must set \
                     'deny_missing_timestamp' explicitly. {consequence}, and neither is a \
                     default this server picks for you (RFC 0010 §6.7)",
                    registry.name,
                    kind
                );
            }
        }
        Ok(())
    }

    /// RFC 0010 §6.9: `warm_platforms` names the files warming fetches on
    /// the two platform-addressed kinds, and means nothing anywhere else.
    /// On `sdkman` the set is closed, so a misspelling is refused here
    /// rather than sent to the broker as a path segment.
    fn validate_registry_warm_platforms(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        use batlehub_core::entities::RegistryKind;
        if registry.cache.warm_platforms.is_empty() {
            return Ok(());
        }
        match kind {
            RegistryKind::Sdkman => {
                for p in &registry.cache.warm_platforms {
                    if batlehub_core::services::sdkman::parse_platform(p).is_none() {
                        bail!(
                            "registry '{}': cache.warm_platforms entry '{p}' is not an SDKMAN \
                             platform (one of {})",
                            registry.name,
                            batlehub_core::services::sdkman::PLATFORMS.join(", ")
                        );
                    }
                }
                Ok(())
            }
            RegistryKind::Nodedist => Ok(()),
            other => bail!(
                "registry '{}': 'cache.warm_platforms' is only meaningful on sdkman and \
                 nodedist registries, whose artifact is one file per platform, not {other}",
                registry.name
            ),
        }
    }

    /// RFC 0010 §4.5: `broker_url` is SDKMAN's second upstream and nobody
    /// else's. Same class as `index_url` on a non-cargo registry — a silently
    /// ignored option is a misconfiguration that looks like a proxy bug — and
    /// a relative value would fail at the first `sdk install` instead of at
    /// boot.
    fn validate_registry_broker_url(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        use batlehub_core::entities::RegistryKind;
        let Some(url) = registry.broker_url.as_deref() else {
            return Ok(());
        };
        if kind != RegistryKind::Sdkman {
            bail!(
                "registry '{}': 'broker_url' is only meaningful on an sdkman registry (it is \
                 SDKMAN's download broker), not {}",
                registry.name,
                kind
            );
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) || url.len() <= 8 {
            bail!(
                "registry '{}': 'broker_url' must be an absolute http(s) URL, got '{url}' — it \
                 is joined with '/download/{{candidate}}/{{version}}/{{platform}}' and fetched",
                registry.name
            );
        }
        Ok(())
    }

    /// `[registries.security]` (RFC 0018 §4.3), one row of the table each.
    fn validate_registry_security(&self, registry: &RegistryConfig) -> Result<()> {
        let Some(sec) = &registry.security else {
            return Ok(());
        };
        if sec.min_age_secs < MIN_AGE_FLOOR_SECS {
            bail!(
                "registry '{}': security.min_age_secs = {} is below the {}-second floor; the \
                 one-hour minimum is the point of the quarantine, and degrading it silently \
                 defeats it",
                registry.name,
                sec.min_age_secs,
                MIN_AGE_FLOOR_SECS
            );
        }
        if sec.mature_age_secs != 0 && sec.mature_age_secs < sec.min_age_secs {
            bail!(
                "registry '{}': security.mature_age_secs ({}) is below min_age_secs ({}); a \
                 version would be \"mature\" before it is servable at all",
                registry.name,
                sec.mature_age_secs,
                sec.min_age_secs
            );
        }
        if registry
            .rules
            .iter()
            .any(|r| matches!(r, RuleConfig::ReleaseAgeGate(_)))
        {
            bail!(
                "registry '{}': [registries.security] and a release_age_gate rule on one \
                 registry are two owners of the same gate with different defaults; move the \
                 rule's min_age_secs into security.min_age_secs and drop the rule",
                registry.name
            );
        }
        if Severity::parse(&sec.max_severity).is_none() {
            bail!(
                "registry '{}': security.max_severity '{}' is not one of unknown, low, medium, \
                 high, critical",
                registry.name,
                sec.max_severity
            );
        }
        for name in &sec.scanners {
            let Some(cfg) = self.scanner_named(name) else {
                bail!(
                    "registry '{}': security.scanners names '{name}', which is not declared under \
                     [scanners]; a typo here would silently scan with nothing",
                    registry.name
                );
            };
            if !cfg.available() {
                bail!(
                    "registry '{}': scanner '{name}' has type '{}', which ships in {} and cannot \
                     run in this build; listing it would hold every version of the registry \
                     forever",
                    registry.name,
                    cfg.type_name(),
                    cfg.ships_in()
                );
            }
            if cfg.missing_required_key() {
                bail!(
                    "registry '{}': scanner '{name}' (type '{}') has no api_key; it is a metered \
                     external service and would fail every scan with a 401 nobody reads (RFC \
                     0018 §4.4)",
                    registry.name,
                    cfg.type_name()
                );
            }
        }
        for name in &sec.required_scanners {
            if !sec.scanners.contains(name) {
                bail!(
                    "registry '{}': security.required_scanners names '{name}', which is not in \
                     security.scanners; a required scanner that never runs would quarantine \
                     forever",
                    registry.name
                );
            }
        }
        Ok(())
    }

    /// The implicit `osv` (RFC 0018 §4.1: *"already implicit today; now
    /// named"*) or a declared `[scanners.<name>]`.
    fn scanner_named(&self, name: &str) -> Option<std::borrow::Cow<'_, ScannerConfig>> {
        if let Some(cfg) = self.scanners.get(name) {
            return Some(std::borrow::Cow::Borrowed(cfg));
        }
        (name == "osv").then_some(std::borrow::Cow::Owned(ScannerConfig::Osv {
            api_url: None,
            escalation: None,
        }))
    }

    /// `[scanners]`: the key an external scanner cannot run without, the
    /// command a local one is, and the escalation rule's own arithmetic.
    fn validate_scanners(&self) -> Result<()> {
        for (name, cfg) in &self.scanners {
            match cfg {
                // One rule, `ScannerConfig::missing_required_key`, says which
                // external scanners cannot run without a key; mlab's API
                // answers unauthenticated and is not among them.
                cfg if cfg.missing_required_key() => {
                    bail!(
                        "[scanners.{name}] type = \"{}\" needs an api_key; without one every \
                         call fails with a 401 nobody reads",
                        cfg.type_name()
                    );
                }
                ScannerConfig::Postmortem { command, .. }
                | ScannerConfig::Guarddog { command, .. }
                    if !is_executable(command) =>
                {
                    bail!(
                        "[scanners.{name}] command '{command}' does not exist or is not \
                         executable; this is a scanner that opens untrusted archives, so it \
                         is checked at startup rather than at the first job"
                    );
                }
                _ => {}
            }
            let Some(esc) = cfg.escalation() else {
                continue;
            };
            if Severity::parse(&esc.from).is_none() || Severity::parse(&esc.to).is_none() {
                bail!("[scanners.{name}.escalation] from/to must be severities");
            }
            if esc.count == 0 {
                bail!("[scanners.{name}.escalation] count must be at least 1");
            }
        }
        Ok(())
    }

    /// RFC 0019 §4.3 — a raw ceiling above the global artifact limit is a
    /// number the global one would silently win over.
    fn validate_raw_ceilings(&self) -> Result<()> {
        let Some(global) = self.limits.max_artifact_size_bytes else {
            return Ok(());
        };
        for reg in &self.registries {
            let Some(raw) = &reg.raw else { continue };
            if raw.enabled && raw.max_size_bytes > global {
                bail!(
                    "registry '{}': raw.max_size_bytes = {} exceeds \
                     [limits].max_artifact_size_bytes = {global}; the global ceiling \
                     would win and the registry's number would be a lie",
                    reg.name,
                    raw.max_size_bytes
                );
            }
        }
        Ok(())
    }

    /// What a registry with `[registries.security]` requires of the rest of
    /// the config: signed inbound webhooks, and a real sandbox.
    fn validate_security_prerequisites(&self) -> Result<()> {
        if !self.registries.iter().any(|r| r.security.is_some()) {
            return Ok(());
        }
        for hook in self.notifications.iter().flat_map(|n| &n.inbound) {
            if hook.secret.as_deref().is_none_or(str::is_empty) {
                bail!(
                    "[[notifications.inbound]] '{}' has no secret while a registry has \
                     [registries.security]; a security.* event on an unsigned webhook \
                     would let anyone on the network deny packages (RFC 0018 §4.3)",
                    hook.name
                );
            }
        }
        if self.worker.sandbox.runtime == "none"
            && std::env::var("BATLEHUB_UNSAFE_NO_SANDBOX").as_deref() != Ok("1")
        {
            bail!(
                "[worker.sandbox] runtime = \"none\" is refused outside tests unless \
                 BATLEHUB_UNSAFE_NO_SANDBOX=1 is set"
            );
        }
        Ok(())
    }

    /// `[scanners]`, `[server].roles`, `[worker]` and the webhook secret
    /// (RFC 0018 §4.3): the rows that are about the whole config.
    fn validate_security_globals(&self) -> Result<()> {
        self.validate_scanners()?;
        if self.server.roles.is_empty() {
            bail!(
                "[server] roles is empty: a process that is neither proxy nor worker does nothing"
            );
        }
        self.validate_flag_sources()?;
        self.validate_air_gap()?;
        self.validate_raw_ceilings()?;
        for name in &self.worker.registries {
            if !self.registries.iter().any(|r| &r.name == name) {
                bail!("[worker] registries names '{name}', which is not a configured registry");
            }
        }
        self.validate_security_prerequisites()
    }

    /// RFC 0018 §4.3's warnings.
    /// The registries `[upstream_audit]` sweeps: every `proxy`/`hybrid`
    /// registry, or the operator's list (validated to name only those).
    pub fn upstream_audit_registries(&self) -> Vec<String> {
        let audited = |r: &RegistryConfig| r.mode != RegistryMode::Local;
        if self.upstream_audit.registries.is_empty() {
            self.registries
                .iter()
                .filter(|r| audited(r))
                .map(|r| r.name.clone())
                .collect()
        } else {
            self.registries
                .iter()
                .filter(|r| audited(r) && self.upstream_audit.registries.contains(&r.name))
                .map(|r| r.name.clone())
                .collect()
        }
    }

    /// `[upstream_audit]` (RFC 0014 §4.4).
    fn validate_upstream_audit(&self) -> Result<()> {
        let a = &self.upstream_audit;
        if a.confirm_after == 0 {
            bail!(
                "[upstream_audit] confirm_after must be at least 1: zero confirms a disappearance \
                 on the first miss, which is the failure the whole design exists to prevent"
            );
        }
        if !(a.outage_ratio > 0.0 && a.outage_ratio <= 1.0) {
            bail!(
                "[upstream_audit] outage_ratio must be in (0.0, 1.0]: 0.0 voids every sweep that \
                 contains a miss, and above 1.0 silently means \"never void\", which should be \
                 written as 1.0"
            );
        }
        if a.interval_secs < MIN_UPSTREAM_AUDIT_INTERVAL_SECS {
            bail!(
                "[upstream_audit] interval_secs must be at least {MIN_UPSTREAM_AUDIT_INTERVAL_SECS}: \
                 below five minutes the sweep is a denial of service against a third party's \
                 registry"
            );
        }
        self.validate_audited_registries()?;
        match a.on_confirmed.as_str() {
            "audit" => {}
            "block" if a.enabled => {}
            "block" => bail!(
                "[upstream_audit] on_confirmed = \"block\" with enabled = false: the key \
                 has no effect and reads as if blocking were active"
            ),
            other => bail!(
                "[upstream_audit] on_confirmed = \"{other}\" is not \"audit\" or \"block\"; a \
                 typo here must not fall back to either"
            ),
        }
        self.validate_registry_on_confirmed()
    }

    /// Every name in `[upstream_audit] registries` must be a configured
    /// registry, and one with an upstream to audit.
    fn validate_audited_registries(&self) -> Result<()> {
        for name in &self.upstream_audit.registries {
            match self.registries.iter().find(|r| &r.name == name) {
                None => bail!(
                    "[upstream_audit] registries names '{name}', which is not a configured registry"
                ),
                Some(r) if r.mode == RegistryMode::Local => bail!(
                    "[upstream_audit] registries names '{name}', a local registry: it has no \
                     upstream to audit"
                ),
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// RFC 0014 §13 O6: the registry-tier `on_confirmed`, held to the same
    /// rules as the estate key — a value that is not one of the two is a typo,
    /// and `"block"` on a registry the audit never sweeps reads as if blocking
    /// were active there.
    fn validate_registry_on_confirmed(&self) -> Result<()> {
        let enabled = self.upstream_audit.enabled;
        let audited = self.upstream_audit_registries();
        for r in &self.registries {
            let Some(value) = r.on_confirmed.as_deref() else {
                continue;
            };
            match value {
                "audit" => continue,
                "block" => {}
                other => bail!(
                    "[[registries]] '{}' on_confirmed = \"{other}\" is not \"audit\" or \
                     \"block\"; a typo here must not fall back to either",
                    r.name
                ),
            }
            if !enabled {
                bail!(
                    "[[registries]] '{}' sets on_confirmed = \"block\" while \
                     [upstream_audit] is not enabled: nothing sweeps, so nothing is \
                     ever confirmed or blocked",
                    r.name
                );
            }
            if !audited.contains(&r.name) {
                bail!(
                    "[[registries]] '{}' sets on_confirmed = \"block\" but is not \
                     audited: it is a local registry, or [upstream_audit] registries \
                     names others",
                    r.name
                );
            }
        }
        Ok(())
    }

    fn upstream_audit_warnings(&self, out: &mut Vec<ConfigWarning>) {
        let a = &self.upstream_audit;
        if !a.enabled {
            return;
        }
        if self.upstream_audit_registries().is_empty() {
            out.push(ConfigWarning::new(
                warnings::UPSTREAM_AUDIT_NOTHING_TO_AUDIT,
                "upstream_audit.enabled",
                "the upstream audit is enabled and no registry is in proxy or hybrid mode: every \
                 sweep will find nothing to probe"
                    .to_owned(),
            ));
        }
        // The estate key, or any registry-tier row (RFC 0014 §13 O6): one
        // `"block"` anywhere is enough for the hold to matter.
        let blocks_somewhere = a.on_confirmed == "block"
            || self
                .registries
                .iter()
                .any(|r| r.on_confirmed.as_deref() == Some("block"));
        if blocks_somewhere && !a.retain_disappeared {
            // RFC 0014 §4.4, §5.4: a blocked package is never read, so
            // `run_idle` evicts its bytes — the combination quietly deletes
            // what the block was keeping. Legal, and almost always a mistake.
            out.push(ConfigWarning::new(
                warnings::UPSTREAM_AUDIT_BLOCK_WITHOUT_HOLD,
                "upstream_audit.retain_disappeared",
                "on_confirmed = \"block\" with retain_disappeared = false: a blocked package is \
                 never read, so idle eviction deletes the last copy the block was keeping"
                    .to_owned(),
            ));
        }
        if !self.server.roles.contains(&ProcessRole::Worker) {
            out.push(ConfigWarning::new(
                warnings::UPSTREAM_AUDIT_NO_WORKER,
                "upstream_audit.enabled",
                "the upstream audit is enabled but this process has no worker role; the sweep \
                 runs on a worker, so unless another process has one, nothing is audited"
                    .to_owned(),
            ));
        }
    }

    /// RFC 0008 §4.5 — an air gap is a promise about the whole instance, so
    /// the things that contradict it are refused at load rather than
    /// discovered from a log.
    fn validate_air_gap(&self) -> Result<()> {
        self.validate_signing_keys()?;
        let Some(air_gap) = &self.air_gap else {
            return Ok(());
        };
        for key in &air_gap.bundle_trusted_keys {
            if !crate::schema::valid_ed25519_hex_key(key) {
                bail!(
                    "[air_gap] bundle_trusted_keys entry '{}' is not a hex-encoded 32-byte \
                     ed25519 public key (64 hex characters)",
                    truncate_key(key)
                );
            }
        }
        if !air_gap.enabled {
            if air_gap.synthesise_listings == Some(true) {
                bail!(
                    "[air_gap] synthesise_listings = true with enabled = false: the key has no \
                     effect on a connected instance and reads as if this one answered listings \
                     offline (RFC 0008-bis §4.5)"
                );
            }
            return Ok(());
        }
        if air_gap.bundle_trusted_keys.is_empty() {
            bail!(
                "[air_gap] enabled = true with no bundle_trusted_keys: import would accept any \
                 bundle, and an air-gapped instance whose only content path is unauthenticated \
                 is worse than one with no content path (RFC 0008 §4.5)"
            );
        }
        if self.proxy.is_some() {
            bail!(
                "[air_gap] enabled = true together with [proxy]: an egress proxy is a route off \
                 the site, and which one wins is not obvious enough to pick silently"
            );
        }
        self.validate_no_egress_per_registry()
    }

    /// The hex check applies whether or not the air gap is on: a malformed key
    /// must never read as "signing is configured". This is also the check
    /// `[registries.signing].trusted_keys` never had — it was parsed at verify
    /// time, so a typo surfaced as a `502` on the first download and named
    /// nothing.
    fn validate_signing_keys(&self) -> Result<()> {
        for (index, reg) in self.registries.iter().enumerate() {
            let Some(signing) = &reg.signing else {
                continue;
            };
            for key in &signing.trusted_keys {
                if !crate::schema::valid_ed25519_hex_key(key) {
                    bail!(
                        "registries[{index}] '{}': signing.trusted_keys entry '{}' is not a \
                         hex-encoded 32-byte ed25519 public key (64 hex characters); an \
                         unusable key reads as 'signing is configured' and fails only at the \
                         first download",
                        reg.name,
                        truncate_key(key)
                    );
                }
            }
        }
        Ok(())
    }

    /// Under an air gap, the per-registry keys that would dial out anyway.
    fn validate_no_egress_per_registry(&self) -> Result<()> {
        for (index, reg) in self.registries.iter().enumerate() {
            if reg.proxy.is_some() {
                bail!(
                    "[air_gap] enabled = true together with registries[{index}] '{}' \
                     [registries.proxy]: an egress proxy is a route off the site",
                    reg.name
                );
            }
            if !reg.cache.warm_packages.is_empty() || !reg.cache.warm_paths.is_empty() {
                bail!(
                    "[air_gap] enabled = true with warming configured on registries[{index}] \
                     '{}': warming fetches from an upstream this mode guarantees will never be \
                     dialled. Seed the instance with a bundle instead (RFC 0008 §4.3)",
                    reg.name
                );
            }
        }
        Ok(())
    }

    /// RFC 0002 §4.3: a flag source is a credential with a ceiling, and both
    /// halves are checked at load — a push endpoint whose secret is empty
    /// authenticates nobody, and a ceiling nobody can parse would fall to the
    /// weakest effect and silently turn every block into a note.
    fn validate_flag_sources(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for (i, src) in self.flag_sources.iter().enumerate() {
            let path = format!("flag_sources[{i}]");
            if !FlagSourceConfig::valid_name(&src.name) {
                bail!(
                    "{path}: name '{}' is not a valid source name (lower-case letters, digits, \
                     '-' and '_', at most 64 characters)",
                    src.name
                );
            }
            if !seen.insert(src.name.as_str()) {
                bail!("{path}: duplicate flag source name '{}'", src.name);
            }
            self.validate_flag_source(&path, src)?;
        }
        Ok(())
    }

    /// One `[[flag_sources]]` entry. Everything except the two checks its
    /// caller owns: the name's shape, and that no earlier entry claimed it.
    fn validate_flag_source(&self, path: &str, src: &FlagSourceConfig) -> Result<()> {
        if src.secret.trim().is_empty() {
            bail!(
                "{path}: '{}' has an empty secret; the HMAC signature is the only credential \
                     the push endpoint has, so an empty key lets anyone on the network flag \
                     (or block) packages",
                src.name
            );
        }
        if src.max_effect().is_none() {
            bail!(
                "{path}: unknown max_effect '{}' (expected inform, warn, gate or hard_block)",
                src.max_effect
            );
        }
        for reg in &src.registries {
            if !self.registries.iter().any(|r| &r.name == reg) {
                bail!("{path}: registries names '{reg}', which is not a configured registry");
            }
        }
        // A flag's identity is `(source, external_id)`, and a
        // `security.verdict` event stores its `hard_block` under the
        // *inbound webhook's* name as the source. Share a name between the
        // two blocks and the credentials stop matching the authority: the
        // flag-source secret — which may be capped to `gate` — revokes a
        // `hard_block` a security feed pushed through the webhook, because
        // `DELETE /api/v1/flags/{source}/{external_id}` authenticates
        // against `[[flag_sources]]` alone. A weaker key must not be able
        // to lift a stronger denial, so the collision is refused here.
        if self
            .notifications
            .iter()
            .flat_map(|n| &n.inbound)
            .any(|h| h.name == src.name)
        {
            bail!(
                "{path}: name '{}' is also a [[notifications.inbound]] webhook; a \
                     security.verdict event stores its hard_block under the webhook's name, and \
                     this source's secret would then revoke it — give them distinct names",
                src.name
            );
        }
        Ok(())
    }

    /// RFC 0008 §4.5 — two states that are legitimate and worth saying out
    /// loud, because in both the operator has configured something that does
    /// not mean what it looks like.
    fn air_gap_warnings(&self, out: &mut Vec<ConfigWarning>) {
        let Some(air_gap) = &self.air_gap else {
            return;
        };
        if air_gap.enabled {
            for (index, reg) in self.registries.iter().enumerate() {
                // RFC 0008-bis §4.5: the kinds whose listing this instance
                // cannot compose — a signed `Packages` index cannot be
                // re-signed here, a gallery answers by query — stay `503` on
                // a listing however many artifacts of theirs are held. Said
                // at startup, so that `503` is not read as synthesis failing.
                if air_gap.synthesises_listings()
                    && matches!(reg.mode, RegistryMode::Proxy)
                    && matches!(
                        reg.registry_type.as_str(),
                        "deb" | "rpm" | "pacman" | "jetbrains" | "generic"
                    )
                {
                    out.push(ConfigWarning::new(
                        warnings::AIR_GAP_LISTING_NOT_SYNTHESISED,
                        format!("registries[{index}].type"),
                        format!(
                            "registry '{}' is a {} registry under [air_gap]: its index is not \
                             synthesised from what this instance holds (RFC 0008-bis §4.3), so \
                             a listing it does not hold stays a 503 while a held file is served \
                             by path.",
                            reg.name, reg.registry_type
                        ),
                    ));
                }
                if matches!(reg.mode, RegistryMode::Hybrid) {
                    out.push(ConfigWarning::new(
                        warnings::AIR_GAP_HYBRID_REGISTRY,
                        format!("registries[{index}].mode"),
                        format!(
                            "registry '{}' is hybrid and [air_gap] is on, so its fall-through to \
                             upstream can never happen: it behaves as a local registry. \
                             Publishing to it still works — this is allowed, and is here so it \
                             is not a surprise.",
                            reg.name
                        ),
                    ));
                }
            }
        } else if !air_gap.bundle_trusted_keys.is_empty() {
            out.push(ConfigWarning::new(
                warnings::AIR_GAP_KEYS_UNUSED,
                "air_gap.bundle_trusted_keys",
                "[air_gap] has bundle_trusted_keys but enabled = false. The keys are kept — \
                 staging a bundle on a connected instance is how one is built — and they \
                 authorise imports only; nothing about serving changes.",
            ));
        }
    }

    /// RFC 0002 §4.3: a source allowed `hard_block` can refuse every
    /// download of what it names, on every registry it may flag. That is
    /// the point of the ceiling, and worth one line at startup.
    fn flag_source_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (i, src) in self.flag_sources.iter().enumerate() {
            if src.max_effect() == Some(batlehub_core::entities::FlagEffect::HardBlock) {
                let scope = if src.registries.is_empty() {
                    "every registry".to_owned()
                } else {
                    src.registries.join(", ")
                };
                out.push(ConfigWarning::new(
                    warnings::FLAG_SOURCE_CAN_HARD_BLOCK,
                    format!("flag_sources[{i}].max_effect"),
                    format!(
                        "flag source '{}' may hard-block: a push from it refuses downloads on {scope} \
                         with no threshold and no bypass, only a gate exemption on the version",
                        src.name
                    ),
                ));
            }
        }
    }

    fn security_warnings(&self, out: &mut Vec<ConfigWarning>) {
        for (index, registry) in self.registries.iter().enumerate() {
            let Some(sec) = &registry.security else {
                continue;
            };
            let path = format!("registries[{index}].security");
            if sec.mode == batlehub_core::entities::SecurityMode::Warn
                && sec.required_scanners.is_empty()
            {
                out.push(ConfigWarning::new(
                    warnings::SECURITY_UNPROTECTED,
                    path.clone(),
                    format!(
                        "registry '{}' has mode = \"warn\" and no required_scanners, so nothing \
                         can ever hold a version: the quarantine is on paper only",
                        registry.name
                    ),
                ));
            }
            for name in &sec.required_scanners {
                if self.scanner_named(name).is_some_and(|c| c.is_enrichment()) {
                    out.push(ConfigWarning::new(
                        warnings::SECURITY_ENRICHMENT_REQUIRED,
                        path.clone(),
                        format!(
                            "registry '{}' requires scanner '{name}', which only enriches other \
                             scanners' findings and never creates one; requiring it holds \
                             versions on a scanner that has nothing to say",
                            registry.name
                        ),
                    ));
                }
            }
            let Ok(kind) = registry
                .registry_type
                .parse::<batlehub_core::entities::RegistryKind>()
            else {
                continue;
            };
            if sec.hold_missing_timestamp && kind.is_path_addressed() {
                out.push(ConfigWarning::new(
                    warnings::SECURITY_TIMESTAMP_HOLD_UNAVAILABLE,
                    path,
                    format!(
                        "every version of '{}' will be held: this registry kind ({kind}) \
                         addresses a file tree by path and cannot supply a publish date, so \
                         hold_missing_timestamp = true (the default) holds every coordinate \
                         open-ended, with no available_at. Set hold_missing_timestamp = false \
                         on this registry, or leave it and accept that it serves nothing \
                         (RFC 0018 §4.3)",
                        registry.name
                    ),
                ));
            }
        }
    }

    /// `[registries.refs]` (RFC 0019 §4.3): forge kinds only, and a branch TTL
    /// that does not turn every request into an API call.
    fn validate_registry_refs(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        let Some(refs) = &registry.refs else {
            return Ok(());
        };
        if !kind.is_forge() {
            bail!(
                "registry '{}': '[registries.refs]' is only meaningful on a git-forge registry \
                 (github, gitlab, forgejo), not {}",
                registry.name,
                kind
            );
        }
        if refs.branch_ttl_secs < MIN_BRANCH_TTL_SECS {
            bail!(
                "registry '{}': refs.branch_ttl_secs = {} is below the {}-second floor; \
                 re-resolving a branch on every request is a rate-limit self-DoS",
                registry.name,
                refs.branch_ttl_secs,
                MIN_BRANCH_TTL_SECS
            );
        }
        // An unparsable action must not fall back to a default: `mutable_refs`
        // and `tag_moved` decide whether a request is served or refused, and
        // an operator who typed `"denied"` expecting refusals would get the
        // opposite of what they wrote.
        for (key, value) in [
            ("mutable_refs", &refs.mutable_refs),
            ("tag_moved", &refs.tag_moved),
        ] {
            if batlehub_core::entities::RefAction::parse(value).is_none() {
                bail!(
                    "registry '{}': refs.{key} = '{}' is not a valid action (expected \
                     \"warn\" or \"deny\")",
                    registry.name,
                    value
                );
            }
        }
        Ok(())
    }

    /// `[registries.raw]` (RFC 0019 §4.3): forge kinds only, a ceiling that
    /// means something, and an allowlist that cannot silently allow or refuse
    /// everything through a typo.
    fn validate_registry_raw(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        let Some(raw) = &registry.raw else {
            return Ok(());
        };
        if !kind.is_forge() {
            bail!(
                "registry '{}': '[registries.raw]' is only meaningful on a git-forge registry \
                 (github, gitlab, forgejo), not {}",
                registry.name,
                kind
            );
        }
        if raw.enabled && raw.max_size_bytes == 0 {
            bail!(
                "registry '{}': raw.max_size_bytes = 0 with raw.enabled = true; unbounded raw \
                 content is exactly what this section exists to close",
                registry.name
            );
        }
        if let Some(scripts) = &raw.scripts {
            if batlehub_core::entities::ScriptAction::parse(scripts).is_none() {
                bail!(
                    "registry '{}': raw.scripts = '{scripts}' is not a valid action (expected \
                     \"warn\", \"deny\" or \"ignore\")",
                    registry.name
                );
            }
        }
        for pattern in &raw.repos {
            if !crate::schema::valid_repo_glob(pattern) {
                bail!(
                    "registry '{}': raw.repos entry '{pattern}' is not an owner/repo glob; a typo \
                     here either allows every repository or none, silently",
                    registry.name
                );
            }
        }
        Ok(())
    }

    /// `[registries.api_reads]` (RFC 0019 §4.3): forge kinds only, and a
    /// closed family list — `contents` and `git/blobs` are raw by another
    /// door, and an unknown family would proxy writes.
    fn validate_registry_api_reads(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        let Some(api) = &registry.api_reads else {
            return Ok(());
        };
        if !kind.is_forge() {
            bail!(
                "registry '{}': '[registries.api_reads]' is only meaningful on a git-forge \
                 registry (github, gitlab, forgejo), not {}",
                registry.name,
                kind
            );
        }
        for family in &api.families {
            if batlehub_core::entities::ApiReadFamily::parse(family).is_none() {
                bail!(
                    "registry '{}': api_reads.families entry '{family}' is not one of tags, \
                     commits, branches",
                    registry.name
                );
            }
        }
        Ok(())
    }

    /// `[registries.readme]`: a policy that exists, and caps that mean something.
    fn validate_registry_readme(registry: &RegistryConfig) -> Result<()> {
        let Some(readme) = &registry.readme else {
            return Ok(());
        };
        // An unrecognised `remote_images` must not silently become the
        // default: the two behaviours differ in what leaves the network,
        // and an operator who typed `"allow"` expecting images believes
        // the opposite of what they would get.
        if batlehub_core::services::RemoteImagePolicy::parse(&readme.remote_images).is_none() {
            bail!(
                "registry '{}': invalid readme.remote_images '{}' (expected \"strip\" or \
                 \"proxy\"; there is no \"allow\" — the console's CSP is baked in at build \
                 time, so it could only ever show broken images)",
                registry.name,
                readme.remote_images
            );
        }
        if readme.enabled && readme.max_bytes == 0 {
            bail!(
                "registry '{}': readme.max_bytes = 0 with enabled = true stores nothing \
                 while claiming to be on; set enabled = false to turn the feature off",
                registry.name
            );
        }
        // The value is a row in a transactional store, read on a page
        // load and held in memory while it renders.
        const README_MAX_BYTES_CEILING: usize = 4 * 1024 * 1024;
        if readme.max_bytes > README_MAX_BYTES_CEILING {
            bail!(
                "registry '{}': readme.max_bytes = {} exceeds the {README_MAX_BYTES_CEILING} \
                 byte ceiling; a README is a database row read on every page load",
                registry.name,
                readme.max_bytes
            );
        }
        // The image cap is a separate number from `max_bytes` because
        // the two bound different things, and it gets the same two
        // guards for the same reasons (RFC 0007-bis §4.5).
        if readme.remote_images == "proxy" && readme.image_max_bytes == 0 {
            bail!(
                "registry '{}': readme.image_max_bytes = 0 with remote_images = \"proxy\" \
                 serves no image while claiming to render them; set remote_images = \
                 \"strip\" to chart them instead",
                registry.name
            );
        }
        // The bytes are buffered in memory to check the type and the cap
        // before anything is stored, so a ceiling makes that bound a
        // statement rather than a hope.
        const IMAGE_MAX_BYTES_CEILING: usize = 16 * 1024 * 1024;
        if readme.image_max_bytes > IMAGE_MAX_BYTES_CEILING {
            bail!(
                "registry '{}': readme.image_max_bytes = {} exceeds the \
                 {IMAGE_MAX_BYTES_CEILING} byte ceiling; an image is held in memory while \
                 its type and size are checked",
                registry.name,
                readme.image_max_bytes
            );
        }
        Ok(())
    }

    /// `[registries.upstream_detail]`: a fetch that shows something, bounded.
    fn validate_registry_upstream_detail(registry: &RegistryConfig) -> Result<()> {
        let Some(detail) = &registry.upstream_detail else {
            return Ok(());
        };
        if detail.enabled && detail.max_versions == 0 {
            bail!(
                "registry '{}': upstream_detail.max_versions = 0 with enabled = true \
                 attempts the fetch and discards every result — the egress happens and \
                 nothing is shown; set enabled = false instead",
                registry.name
            );
        }
        // One page's version table, held in memory and serialised to
        // JSON per request.
        const UPSTREAM_MAX_VERSIONS_CEILING: usize = 5_000;
        if detail.max_versions > UPSTREAM_MAX_VERSIONS_CEILING {
            bail!(
                "registry '{}': upstream_detail.max_versions = {} exceeds the \
                 {UPSTREAM_MAX_VERSIONS_CEILING} ceiling; one page's version table is \
                 held in memory and serialised to JSON on every request",
                registry.name,
                detail.max_versions
            );
        }
        Ok(())
    }

    /// `version_pattern` is a publish-time restriction (a security
    /// control), so an uncompilable regex must fail the config load
    /// rather than silently degrade to "allow every version" (fail-open).
    /// RFC 0020 §4.3: the key names a protocol's asset, so it belongs to the
    /// two kinds that speak it; a seed of the wrong length is a typo Ed25519
    /// would otherwise sign with; the key id is a URL path segment.
    fn validate_registry_vsx_signing(
        registry: &RegistryConfig,
        kind: batlehub_core::entities::RegistryKind,
    ) -> Result<()> {
        let Some(signing) = &registry.vsx_signing else {
            return Ok(());
        };
        if !matches!(
            kind,
            batlehub_core::entities::RegistryKind::VscodeMarketplace
                | batlehub_core::entities::RegistryKind::Openvsx
        ) {
            anyhow::bail!(
                "registry '{}': [registries.vsx_signing] applies to type = \"vscode-marketplace\" \
                 or \"openvsx\" only (it signs VSIX packages), not to '{}'",
                registry.name,
                registry.registry_type
            );
        }
        let seed = signing.seed_hex.trim();
        if seed.len() != 64 || !seed.bytes().all(|b| b.is_ascii_hexdigit()) {
            anyhow::bail!(
                "registry '{}': vsx_signing.seed_hex must be 64 hex characters (a 32-byte \
                 Ed25519 seed; `batlehub-cli vsx keygen` prints one), got {} characters",
                registry.name,
                seed.len()
            );
        }
        if let Some(id) = &signing.key_id {
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            {
                anyhow::bail!(
                    "registry '{}': vsx_signing.key_id must be non-empty and use only \
                     [A-Za-z0-9._-] — it is a path segment of the public-key URL",
                    registry.name
                );
            }
        }
        Ok(())
    }

    fn validate_registry_versioning(registry: &RegistryConfig) -> Result<()> {
        let Some(versioning) = &registry.versioning else {
            return Ok(());
        };
        let Some(pattern) = &versioning.version_pattern else {
            return Ok(());
        };
        regex::Regex::new(pattern).map_err(|e| {
            anyhow::anyhow!(
                "registry '{}': invalid version_pattern '{pattern}': {e}",
                registry.name
            )
        })?;
        Ok(())
    }

    /// Apply environment variable overrides on top of the file-based config.
    ///
    /// **Preferred approach for secrets:** use `${VAR_NAME}` placeholders directly
    /// inside the TOML file — they are expanded before parsing, so they work for
    /// any field, including `client_secret`, upstream auth `token`/`password`/`value`,
    /// and any other string field.  See the docs for details.
    ///
    /// This method handles a fixed set of named overrides for non-secret top-level
    /// fields as a convenience.  Convention: `PROXY_CACHE__<SECTION>__<FIELD>`
    /// (double-underscore separator).
    ///
    /// Supported variables:
    /// | Variable                              | Field                        |
    /// |---------------------------------------|------------------------------|
    /// | `PROXY_CACHE__SERVER__HOST`           | `server.host`                |
    /// | `PROXY_CACHE__SERVER__PORT`           | `server.port`                |
    /// | `PROXY_CACHE__SERVER__STATIC_DIR`     | `server.static_dir`          |
    /// | `PROXY_CACHE__DATABASE__URL`          | `database.url`               |
    /// | `PROXY_CACHE__DATABASE__MAX_CONNECTIONS` | `database.max_connections` |
    /// | `PROXY_CACHE__DATABASE__MIN_CONNECTIONS` | `database.min_connections` |
    /// | `PROXY_CACHE__DATABASE__ACQUIRE_TIMEOUT_SECS` | `database.acquire_timeout_secs` |
    /// | `PROXY_CACHE__STORAGE__PATH`          | `storage.path` (single filesystem backend only)  |
    /// | `PROXY_CACHE__STORAGE__BUCKET`        | `storage.bucket` (single S3 backend only)        |
    /// | `PROXY_CACHE__STORAGE__REGION`        | `storage.region` (single S3 backend only)        |
    /// | `PROXY_CACHE__STORAGE__ENDPOINT_URL`  | `storage.endpoint_url` (single S3 backend only)  |
    /// | `PROXY_CACHE__OTEL__ENDPOINT`         | `otel.endpoint`              |
    /// | `PROXY_CACHE__OTEL__SERVICE_NAME`     | `otel.service_name`          |
    pub fn apply_env_overrides(&mut self) {
        let env = |key: &str| std::env::var(key).ok();

        fn parse_env_or_warn<T: std::str::FromStr>(key: &str, v: &str) -> Option<T> {
            match v.parse() {
                Ok(parsed) => Some(parsed),
                Err(_) => {
                    eprintln!(
                        "warning: environment override {key} has invalid value {v:?}; ignoring it \
                         and keeping the configured value"
                    );
                    None
                }
            }
        }

        /// One numeric override: read, parse, and assign only if both succeed.
        ///
        /// The `Option`-of-`Option` nesting this replaces is what the branch
        /// count was made of — every numeric key spent two `if let`s saying the
        /// same two things.
        fn set_parsed<T: std::str::FromStr>(
            env: &dyn Fn(&str) -> Option<String>,
            key: &str,
            field: &mut T,
        ) {
            let Some(raw) = env(key) else { return };
            if let Some(parsed) = parse_env_or_warn(key, &raw) {
                *field = parsed;
            }
        }

        if let Some(v) = env("PROXY_CACHE__SERVER__HOST") {
            self.server.host = v;
        }
        set_parsed(&env, "PROXY_CACHE__SERVER__PORT", &mut self.server.port);
        if let Some(v) = env("PROXY_CACHE__SERVER__STATIC_DIR") {
            self.server.static_dir = Some(v);
        }
        if let Some(v) = env("PROXY_CACHE__DATABASE__URL") {
            self.database.url = v;
        }
        set_parsed(
            &env,
            "PROXY_CACHE__DATABASE__MAX_CONNECTIONS",
            &mut self.database.max_connections,
        );
        set_parsed(
            &env,
            "PROXY_CACHE__DATABASE__MIN_CONNECTIONS",
            &mut self.database.min_connections,
        );
        set_parsed(
            &env,
            "PROXY_CACHE__DATABASE__ACQUIRE_TIMEOUT_SECS",
            &mut self.database.acquire_timeout_secs,
        );

        apply_storage_env_overrides(&mut self.storage, &env);
        apply_otel_env_overrides(&mut self.otel, &env);
        apply_proxy_env_overrides(&mut self.proxy, &env);
    }
}

fn apply_storage_env_overrides(storage: &mut StoragesConfig, env: &dyn Fn(&str) -> Option<String>) {
    let StoragesConfig::Single(ref mut backend) = storage else {
        return;
    };
    match backend {
        StorageBackendConfig::Filesystem(fs) => apply_filesystem_env_overrides(fs, env),
        StorageBackendConfig::S3(s3) => apply_s3_env_overrides(s3, env),
    }
}

fn apply_filesystem_env_overrides(
    fs: &mut FilesystemStorageConfig,
    env: &dyn Fn(&str) -> Option<String>,
) {
    if let Some(v) = env("PROXY_CACHE__STORAGE__PATH") {
        fs.path = v;
    }
}

fn apply_s3_env_overrides(s3: &mut S3StorageConfig, env: &dyn Fn(&str) -> Option<String>) {
    if let Some(v) = env("PROXY_CACHE__STORAGE__BUCKET") {
        s3.bucket = v;
    }
    if let Some(v) = env("PROXY_CACHE__STORAGE__REGION") {
        s3.region = v;
    }
    if let Some(v) = env("PROXY_CACHE__STORAGE__ENDPOINT_URL") {
        s3.endpoint_url = Some(v);
    }
}

fn apply_otel_env_overrides(otel: &mut Option<OtelConfig>, env: &dyn Fn(&str) -> Option<String>) {
    if let Some(v) = env("PROXY_CACHE__OTEL__ENDPOINT") {
        match otel {
            Some(o) => o.endpoint = v,
            None => {
                *otel = Some(OtelConfig {
                    endpoint: v,
                    service_name: server::default_service_name(),
                })
            }
        }
    }
    if let Some(v) = env("PROXY_CACHE__OTEL__SERVICE_NAME") {
        if let Some(o) = otel {
            o.service_name = v;
        }
    }
}

fn apply_proxy_env_overrides(
    proxy: &mut Option<UpstreamProxyConfig>,
    env: &dyn Fn(&str) -> Option<String>,
) {
    if let Some(v) = env("PROXY_CACHE__PROXY__URL") {
        match proxy {
            Some(p) => p.url = v,
            None => {
                *proxy = Some(UpstreamProxyConfig {
                    url: v,
                    username: env("PROXY_CACHE__PROXY__USERNAME"),
                    password: env("PROXY_CACHE__PROXY__PASSWORD"),
                    no_proxy: env("PROXY_CACHE__PROXY__NO_PROXY"),
                })
            }
        }
    }
    if let Some(p) = proxy {
        if let Some(v) = env("PROXY_CACHE__PROXY__USERNAME") {
            p.username = Some(v);
        }
        if let Some(v) = env("PROXY_CACHE__PROXY__PASSWORD") {
            p.password = Some(v);
        }
        if let Some(v) = env("PROXY_CACHE__PROXY__NO_PROXY") {
            p.no_proxy = Some(v);
        }
    }
}

#[cfg(test)]
mod tests;

/// Whether `path` names an existing file with an execute bit — the check
/// RFC 0018 §4.3 asks for the subprocess scanners' `command`.
fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
