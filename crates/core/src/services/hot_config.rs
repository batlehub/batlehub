use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use regex::Regex;
use tokio::sync::RwLock;

use crate::entities::{RegistryGrants, RegistryPolicyTiers};
use crate::entities::{ResolutionPolicy, Role};
use crate::ports::{BetaChannelPort, RegistryClient};
use crate::rules::Rule;
use crate::services::signed_url::SignedUrlService;

/// Per-registry behaviour configuration wired in at startup (or on reload).
pub struct RegistryPolicy {
    pub metadata_ttl: Option<Duration>,
    /// Rules evaluated in order for every request to this registry.
    pub rules: Vec<Box<dyn Rule>>,
    /// When `true`, skip artifact storage entirely and stream directly from upstream.
    pub firewall_only: bool,
    /// When `true`, serve stale (expired) cached metadata if upstream returns a transient
    /// `Registry` error. Allows cached artifacts to keep being served during outages.
    pub serve_stale_metadata: bool,
    /// When set, artifacts are re-fetched from upstream after this duration even if
    /// present in storage.
    pub artifact_ttl: Option<Duration>,
}

/// Versioning policy enforced at publish time for a single registry.
#[derive(Default, Clone)]
pub struct VersioningPolicy {
    /// Reject versions that are not valid semver.
    pub enforce_semver: bool,
    /// If `enforce_semver` is true, also reject pre-release versions (e.g. `1.0.0-beta.1`).
    pub allow_prerelease: bool,
    /// Optional compiled regex; publish is rejected when the version string does not match.
    pub version_pattern: Option<Regex>,
}

/// Per-registry artifact integrity policy (mirrors config-layer `IntegrityConfig`).
///
/// Verification runs on the proxy fetch-and-cache path: once the upstream bytes
/// are buffered, they are hashed and compared against the checksum advertised in
/// the registry metadata (`PackageMetadata.checksum`). It does **not** apply to
/// `firewall_only` registries, which stream straight through without buffering.
#[derive(Debug, Clone)]
pub struct IntegrityPolicy {
    /// Master switch. When `false`, no verification is performed.
    pub enabled: bool,
    /// When a mismatch is detected, fail the download (and never cache the bytes).
    /// A mismatch is never bypassable — it means the bytes are wrong.
    pub block_on_mismatch: bool,
    /// When the upstream provides no usable checksum, block the download unless
    /// the caller holds one of `bypass_roles`. When `false` (default), a missing
    /// checksum is only warned about.
    pub require_metadata: bool,
    /// Roles allowed to bypass the `require_metadata` gate.
    pub bypass_roles: Vec<Role>,
    /// Re-verify stored bytes against a self-computed SHA-256 on every serve
    /// (cache hit / local read), not just on first fetch. Off by default.
    pub verify_on_serve: bool,
}

impl Default for IntegrityPolicy {
    fn default() -> Self {
        // Verify-and-block-on-mismatch by default: a mismatch indicates
        // corruption or tampering and should essentially never fire in normal
        // operation. Missing metadata only warns (registries like NuGet/Maven
        // advertise no checksum), so the default never blocks a healthy fetch.
        // Re-serve verification stays opt-in (it costs a read+hash per serve).
        Self {
            enabled: true,
            block_on_mismatch: true,
            require_metadata: false,
            bypass_roles: Vec::new(),
            verify_on_serve: false,
        }
    }
}

/// Signing configuration stored in the service (mirrors config-layer `SigningConfig`).
#[derive(Debug, Default, Clone)]
pub struct SigningConfig {
    pub required: bool,
    pub allowed_types: Vec<String>,
    /// Verify a stored `ed25519` detached signature against `trusted_keys` on download.
    pub verify_on_download: bool,
    /// Hex-encoded 32-byte Ed25519 public keys trusted to sign artifacts.
    pub trusted_keys: Vec<String>,
}

/// Local retention policy stored in the service (mirrors config-layer
/// `RetentionConfig`) — RFC 0016.
///
/// Registry tier. The namespace and package tiers RFC 0016 §4.1 describes need
/// RFC 0015's namespace blocks and its `policy` table; the version-tier `keep`
/// pin does not, and lives on the version row as `retention_keep`.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Keep the newest N versions of each package.
    pub keep_versions: Option<u32>,
    /// Keep anything published within this window.
    pub keep_for: Option<Duration>,
    /// Keep anything downloaded within this window — the veto that makes
    /// retention safe to switch on (RFC 0016 §4.3).
    pub keep_if_pulled: Option<Duration>,
    /// Keep yanked versions. Defaults to `true`.
    pub keep_yanked: bool,
    /// Before this instant, an absent download record proves nothing.
    pub download_signal_floor: DateTime<Utc>,
    /// Pause between reclamations, bounding the blast radius of a first live run
    /// without leaving the estate in a state nothing else models.
    pub reclaim_delay: Duration,
    /// How long a tombstone keeps its detail before compaction strips it to the
    /// coordinate. `None` — the default — keeps it forever.
    pub tombstone_detail_for: Option<Duration>,
    /// Report and write nothing. Defaults to `true`, so a configured policy does
    /// nothing until an operator has read a report and turned it off.
    pub dry_run: bool,
}

impl RetentionPolicy {
    /// The plain-data policy the retention run takes. The two are separate types
    /// on purpose: this one mirrors a config block and lives behind a lock, that
    /// one is a snapshot a run is judged against for its whole duration.
    pub fn for_run(&self) -> crate::services::retention::RetentionPolicy {
        crate::services::retention::RetentionPolicy {
            keep_versions: self.keep_versions,
            keep_for: self.keep_for,
            keep_if_pulled: self.keep_if_pulled,
            keep_yanked: self.keep_yanked,
            download_signal_floor: self.download_signal_floor,
            reclaim_delay: self.reclaim_delay,
            dry_run: self.dry_run,
        }
    }
}

/// `dry_run: true` and `keep_yanked: true`, matching the config layer. A derived
/// `Default` would say `false` for both — the destructive direction — and the
/// two definitions of "default" must not disagree about a policy that destroys
/// the only copy of something.
impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            keep_versions: None,
            keep_for: None,
            keep_if_pulled: None,
            keep_yanked: true,
            download_signal_floor: default_download_signal_floor(),
            reclaim_delay: Duration::ZERO,
            tombstone_detail_for: None,
            dry_run: true,
        }
    }
}

/// [`crate::services::retention::DEFAULT_DOWNLOAD_SIGNAL_FLOOR`], parsed.
///
/// Infallible in practice — the constant is a literal this crate owns — and a
/// parse failure falls back to [`DateTime::UNIX_EPOCH`], which disables the
/// floor rather than enabling it for everything. Failing open here would keep
/// every version forever; failing closed would reclaim on evidence the RFC says
/// is not evidence.
pub fn default_download_signal_floor() -> DateTime<Utc> {
    crate::services::retention::DEFAULT_DOWNLOAD_SIGNAL_FLOOR
        .parse()
        .unwrap_or(DateTime::UNIX_EPOCH)
}

/// Resolve `download_signal_floor_days` — days before *now* — into the instant a
/// run compares against, or [`default_download_signal_floor`] when unset.
///
/// Resolved once, at config load, and not per run: a floor recomputed from the
/// clock would creep forward, so a version protected by it today would be
/// reclaimable tomorrow. That is the opposite of a floor.
///
/// Here rather than in the server crate because it is chrono arithmetic and the
/// server does not depend on chrono — and because a second implementation of
/// "what does this number mean" is how the two would come to disagree.
pub fn resolve_download_signal_floor(days_before_now: Option<u32>) -> DateTime<Utc> {
    match days_before_now {
        Some(d) => Utc::now() - chrono::Duration::days(i64::from(d)),
        None => default_download_signal_floor(),
    }
}

/// SBOM configuration stored in the service (mirrors config-layer `SbomConfig`).
#[derive(Debug, Default, Clone)]
pub struct SbomConfig {
    pub enabled: bool,
    pub formats: Vec<String>,
    pub required: bool,
    pub fetch_upstream: bool,
    /// The registry adapter type (e.g. "cargo", "npm") — used for archive extraction.
    pub registry_type: String,
}

/// What the renderer does with an `<img>` pointing at a third-party host.
///
/// There is deliberately no `Allow`: the SPA's CSP is baked into the document at
/// build time (`img-src 'self' data:`), so a setting that only worked in a custom
/// UI build would be a trap — the operator would set it and see broken images
/// with no error anywhere (RFC 0007 §4.1).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RemoteImagePolicy {
    /// Replace the image with an inline chip carrying its `alt` text and its
    /// host, so the reader can see that an image was there and where it pointed.
    #[default]
    Strip,
    /// Rewrite it to fetch through this server. Every page view would otherwise
    /// beacon to a host the package author chose, from inside the network.
    Proxy,
}

impl RemoteImagePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Strip => "strip",
            Self::Proxy => "proxy",
        }
    }

    /// `None` for anything else — an unrecognised value must not silently become
    /// the default, because the two behaviours differ in what leaves the network.
    /// [`crate::entities::RegistryKind`]-style parse: the config validator turns
    /// the `None` into a refusal to start.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "strip" => Some(Self::Strip),
            "proxy" => Some(Self::Proxy),
            _ => None,
        }
    }
}

/// README capture configuration stored in the service (mirrors config-layer
/// `ReadmeConfig`).
///
/// Unlike [`SbomConfig`], the absence of the config block means **enabled**: for
/// the metadata-borne registry kinds the text is a field of a document the proxy
/// already fetches and parses, so the default costs one deserialised field
/// (RFC 0007 §4.1).
#[derive(Debug, Clone)]
pub struct ReadmeConfig {
    pub enabled: bool,
    /// Extract from the cached artifact when the metadata carries none. Rides
    /// the artifact read SBOM already performs when SBOM is on, and adds one
    /// storage read per newly-cached version when it is not.
    pub from_archive: bool,
    /// Cap on the **stored source**, applied after decompression at the point of
    /// extraction. Truncation is recorded and surfaced, never silent.
    pub max_bytes: usize,
    pub remote_images: RemoteImagePolicy,
    /// Hosts an image may be proxied from. Empty means every host — see
    /// `ReadmeConfig::remote_image_hosts` in the config crate for why the
    /// permissive reading is the compatible one.
    pub remote_image_hosts: Vec<String>,
    /// Cap on **one proxied image**, separate from `max_bytes` above, which caps
    /// the stored text. Inert under [`RemoteImagePolicy::Strip`], where nothing
    /// is fetched (RFC 0007-bis §4.1).
    pub image_max_bytes: usize,
    /// The registry adapter type (e.g. "cargo", "npm") — used to pick the
    /// extraction family, exactly as [`SbomConfig::registry_type`] is.
    pub registry_type: String,
}

impl Default for ReadmeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            from_archive: true,
            max_bytes: DEFAULT_README_MAX_BYTES,
            remote_images: RemoteImagePolicy::Strip,
            remote_image_hosts: Vec::new(),
            image_max_bytes: DEFAULT_README_IMAGE_MAX_BYTES,
            registry_type: String::new(),
        }
    }
}

/// 256 KiB. Large enough for essentially every real README, small enough that
/// the row stays cheap to read on a page load.
pub const DEFAULT_README_MAX_BYTES: usize = 262_144;

/// 2 MiB per proxied image.
///
/// Generous rather than restrictive, and deliberately so: the largest image in a
/// survey of 150 real README image URLs was 1.6 MB, with a median of 4 kB
/// (RFC 0007-bis §13.2). A cap that refused the real maximum would present as
/// "this proxy breaks images" rather than as a limit somebody chose.
pub const DEFAULT_README_IMAGE_MAX_BYTES: usize = 2_097_152;

/// The console's discovery read, per registry (mirrors config-layer
/// `UpstreamDetailConfig`).
///
/// A separate block from [`ReadmeConfig`] because it is not a README setting:
/// it governs the *version list* too, and an operator may want one without the
/// other (RFC 0007 §4.1).
///
/// **There is no TTL of its own.** The document lands in the existing metadata
/// cache under the key `cached_version_document` already builds, so it obeys the
/// registry's `metadata_ttl_secs` and its `serve_stale_metadata`. A second,
/// independently clocked expiry for the same bytes is how two caches come to
/// disagree about one document.
#[derive(Debug, Clone)]
pub struct UpstreamDetailConfig {
    pub enabled: bool,
    /// Cap on upstream-only versions returned for one package.
    ///
    /// Bounds the *response*, not the fetch: the document is one document
    /// whatever its size. Applied newest-first, and the response says it was
    /// applied — a silently shortened list is a lie about the registry.
    pub max_versions: usize,
    /// How long an upstream "no such package" is remembered, so a bad URL, a
    /// typo or a crawler cannot turn every reload into an upstream request.
    pub negative_ttl: Duration,
}

impl Default for UpstreamDetailConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_versions: DEFAULT_UPSTREAM_MAX_VERSIONS,
            negative_ttl: Duration::from_secs(DEFAULT_UPSTREAM_NEGATIVE_TTL_SECS),
        }
    }
}

/// **On.** The button admits nothing a caller could not already do, and an
/// instance that upgrades and changes nothing gets one new capability that
/// changes no permission (RFC 0007-bis §9).
pub const DEFAULT_CONSOLE_FETCH: bool = true;

/// 300 versions. Long enough for essentially every real package's table, short
/// enough that one page's JSON stays a page's worth.
pub const DEFAULT_UPSTREAM_MAX_VERSIONS: usize = 300;

/// Five minutes. Long enough to absorb a reload loop, short enough that a
/// package published a moment ago appears without an operator waiting.
pub const DEFAULT_UPSTREAM_NEGATIVE_TTL_SECS: u64 = 300;

/// Per-registry feature flags (mirrors config-layer `FeatureFlagsConfig`).
/// A "feature flag" category of optional, cross-cutting UI/integration toggles.
#[derive(Debug, Clone)]
pub struct FeatureFlags {
    /// Show the socket.dev supply-chain badge for each package version in the UI.
    pub socket_badge: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        // Flags default to "on" so a registry without a `[registries.feature_flags]`
        // block still gets the badge; it is disabled explicitly per registry.
        Self { socket_badge: true }
    }
}

/// All registry state that can be hot-reloaded without restarting the process.
///
/// Stored behind `Arc<RwLock<>>` inside `ProxyService` and `LocalRegistryService`.
/// When config is reloaded, the write lock is acquired, the struct is replaced in-place,
/// and in-flight requests finish with the previous data before seeing the update.
pub struct HotConfig {
    /// Per-registry proxy clients. `Arc` allows cheap cloning before releasing the read lock.
    pub registries: HashMap<String, Arc<dyn RegistryClient>>,
    /// Per-registry access policies. `Arc` allows cheap cloning (rules are not Clone).
    pub policies: HashMap<String, Arc<RegistryPolicy>>,
    /// Per-namespace rule chains, for the namespaces that override a gate
    /// (RFC 0015 §4.1).
    ///
    /// `(match_prefix, chain)` in config order, and **only for namespaces that
    /// declared `rules`** — one with none runs the registry's chain from
    /// `policies` above, and holding an identical copy would double the trait
    /// objects for no behaviour change.
    ///
    /// Composition is deepest-wins, so the **last** matching prefix supplies the
    /// chain: an operator who writes `@acme` and then `@acme/billing` gets the
    /// more specific answer from the block they wrote second.
    pub namespace_policies: HashMap<String, Vec<(String, Arc<RegistryPolicy>)>>,
    /// Per-registry grant hierarchies (RFC 0015 §4.1).
    ///
    /// The registry and namespace tiers, built from the config file. Package and
    /// version tiers come from the `policy` table and are not here.
    ///
    /// **A registry with no entry grants nothing**, which is the fail-closed
    /// reading and the one §4.3 requires: absence is not "everything". Every
    /// registry the server configures gets an entry, so a missing key only
    /// happens in a test that did not build one — and such a test gets a closed
    /// registry rather than an open one.
    pub grants: HashMap<String, Arc<RegistryGrants>>,
    /// RFC 0015 §4.1's tier above `registry` — the whole server.
    ///
    /// Where the control-surface verbs live, because about a dozen admin
    /// endpoints name no registry and so had no node to attach a grant to. It is
    /// prepended to every resolution path, so it composes by §4.3's union like
    /// any other tier rather than by a rule of its own.
    ///
    /// `None` is **not** a seal: a deployment that has never written an instance
    /// grant must behave as it did before this tier existed, and a union with
    /// nothing is what that means. The empty-versus-absent distinction §4.3 draws
    /// for a node's `grants` is carried by the `Node` inside, not by this
    /// `Option`.
    pub instance: Option<Arc<crate::entities::Node>>,
    /// RFC 0015 §4.2 — the GPG keys a Terraform namespace signs its providers
    /// with, read on the provider download path.
    ///
    /// `None` serves the empty list this server hardcoded before the store
    /// existed, which is what an estate that has registered no key still gets.
    pub signing_keys: Option<Arc<dyn crate::ports::SigningKeyPort>>,
    /// Per-registry policy tiers (RFC 0015 §4.1) — the other five policies.
    ///
    /// `grants` above carries the one that composes by union; this carries
    /// `visibility`, `prerelease_visibility`, `versioning`, `quota` and `rules`,
    /// which compose deepest-wins.
    ///
    /// **A registry with no entry constrains nothing**, which is the opposite of
    /// the reading `grants` takes and is not an inconsistency: grants only widen,
    /// so a union of nothing is nothing and absence must fail closed; these are
    /// constraints, so absence must leave behaviour exactly as it is today.
    pub policy_tiers: HashMap<String, Arc<RegistryPolicyTiers>>,
    /// Storage for the package and version tiers (RFC 0015 §6.3).
    ///
    /// `None` means those two tiers contribute nothing — which is the correct
    /// reading for a deployment with no database and for a test fixture that
    /// wired none, and is *not* the same as "they deny": a tier with no rows
    /// inherits.
    pub grant_repo: Option<Arc<dyn crate::ports::GrantRepository>>,
    /// Where forge ref resolutions are remembered (RFC 0019 §5.2).
    ///
    /// `None` means refs still resolve — resolution is what builds the cache
    /// key — but nothing is remembered, so a moved tag cannot be detected.
    /// Correct for a fixture that wired none; the server always wires one.
    pub ref_resolutions: Option<Arc<dyn crate::ports::RefResolutionRepository>>,
    /// The rate-limit budget forge clients share (RFC 0019 §5.2). Read by the
    /// clients, not by the proxy; carried here so a reload replaces it with
    /// the rest of the registry state.
    pub rate_limit_budget: Option<Arc<dyn crate::ports::RateLimitBudget>>,
    /// Per-registry ref TTLs (`[registries.refs]`). A registry with no entry
    /// takes the defaults, which is what every non-forge registry has.
    pub forge_refs: HashMap<String, crate::entities::ForgeRefsPolicy>,
    /// Per-registry `[registries.raw]` (RFC 0019 §4.1, phase 3). A forge
    /// registry with no entry takes [`crate::entities::RawPolicy::default`],
    /// which is **off**: raw was implicitly on before the section existed,
    /// and turning it off is the behaviour change §9 states.
    pub forge_raw: HashMap<String, crate::entities::RawPolicy>,
    /// Per-registry `[registries.api_reads]` (RFC 0019 §4.1, phase 3): the
    /// typed read-only JSON families to serve beside the release routes. A
    /// registry with no entry serves none — the routes answer `404`, which
    /// is what an un-opted-in registry has always done.
    pub forge_api_reads: HashMap<String, Vec<crate::entities::ApiReadFamily>>,
    /// RFC 0008 §4.1 — whether this instance will dial out at all, and what
    /// it does with a miss. The default is today's behaviour.
    pub air_gap: crate::entities::AirGapPolicy,
    /// Where a miss is written. `None` on a deployment with no database and
    /// on every connected instance that never records one.
    pub miss_recorder: Option<Arc<dyn crate::ports::MissRecorder>>,
    /// Per-registry `[registries.security]` profiles (RFC 0018 §4.1). A
    /// registry with no entry has no quarantine: its gates run as rules,
    /// exactly as before the section existed.
    pub security: HashMap<String, crate::entities::SecurityPolicy>,
    /// The gates a `[security]` registry runs as scanners instead of rules
    /// (RFC 0018 §6.1): `block_list`, `cve_gate` and the three wrapped
    /// metadata gates, built per registry because each holds that registry's
    /// thresholds. Read by the worker.
    pub internal_scanners: HashMap<String, Vec<Arc<dyn crate::ports::ArtifactScanner>>>,
    /// Where verdicts live (RFC 0018 §6.1). `None` on a deployment with no
    /// database — and then no registry may opt into `[security]`, which the
    /// server refuses at startup rather than serving unscanned.
    pub verdicts: Option<Arc<dyn crate::ports::VerdictRepository>>,
    /// The scan queue the proxy enqueues on and the worker leases from.
    pub scan_queue: Option<Arc<dyn crate::ports::ScanQueue>>,
    /// Storage for the package and version *policy* tiers (RFC 0015 §6.3).
    ///
    /// `None` means those two tiers contribute nothing, which is the correct
    /// reading for a deployment with no database and for a fixture that wired
    /// none — and is not the same as "they deny": a tier with no row inherits.
    pub policy_repo: Option<Arc<dyn crate::ports::PolicyRepository>>,
    /// What shadow mode would have refused (RFC 0015 §4.7).
    ///
    /// `None` disables the recording, not the shadow — a node in `dry_run` still
    /// serves what it would refuse. That asymmetry is deliberate: the fail-open
    /// behaviour is what the operator configured, and dropping it because a
    /// buffer is missing would make the setting mean different things in
    /// different builds. What a missing log costs is the *visibility*, which is
    /// why every wiring path installs one.
    pub shadow_log: Option<Arc<crate::services::shadow::ShadowLog>>,
    /// Whole-registry documents, cached per grant set (RFC 0015 §11.7 arm 3).
    ///
    /// `None` disables the cache entirely, which is what every fixture that does
    /// not build one gets — correct, because a missing cache is a slow answer
    /// rather than a wrong one.
    pub document_cache: Option<Arc<crate::services::document_cache::DocumentCache>>,
    /// Per-registry versioning policies (Clone, cheap).
    pub versioning: HashMap<String, VersioningPolicy>,
    /// Per-registry artifact signing configs (Clone, cheap).
    pub signing: HashMap<String, SigningConfig>,
    /// Per-registry VSIX signing keys (`[registries.vsx_signing]`, RFC 0020):
    /// a `vscode-marketplace`/`openvsx` registry that holds one signs what it
    /// publishes and serves the signature as a gallery asset.
    pub vsx_signing: HashMap<String, Arc<crate::services::signature::VsxSigningKey>>,
    /// Per-registry SBOM generation configs (Clone, cheap).
    pub sbom: HashMap<String, SbomConfig>,
    /// Per-registry README capture configs (Clone, cheap).
    ///
    /// A registry with no entry is *not* one with the feature off: the absence
    /// of a `[registries.readme]` block means enabled, so the builder writes an
    /// entry for every registry and a missing key only happens in a test that
    /// did not care. Readers use `unwrap_or_default()`, which is the enabled
    /// shape (RFC 0007 §4.1).
    pub readme: HashMap<String, ReadmeConfig>,
    /// Per-registry discovery-read configs (Clone, cheap).
    ///
    /// Populated for every registry for the same reason `readme` is: the absence
    /// of a `[registries.upstream_detail]` block means **on**.
    pub upstream_detail: HashMap<String, UpstreamDetailConfig>,
    /// Whether the console's **Fetch this version** button is offered, per
    /// registry (RFC 0007-bis §4.4).
    ///
    /// A bare `bool` rather than a config struct because there is one question
    /// to answer. An absent entry means [`DEFAULT_CONSOLE_FETCH`], which is what
    /// a registry that never wrote the setting down gets.
    pub console_fetch: HashMap<String, bool>,
    /// Per-registry feature flags (Clone, cheap).
    pub feature_flags: HashMap<String, FeatureFlags>,
    /// Per-registry artifact integrity policies (Clone, cheap).
    pub integrity: HashMap<String, IntegrityPolicy>,
    /// Per-registry beta-channel gate ports.
    pub beta_channel: HashMap<String, Arc<dyn BetaChannelPort>>,
    /// Per-registry local retention policies (Clone, cheap), from
    /// `[registries.retention]` (RFC 0016).
    ///
    /// Keyed only by the registries that wrote the block down, like
    /// `sbom` and unlike `readme`: absence here means keep everything forever,
    /// which is both the default and what every instance did before this
    /// existed. Nothing has to be populated for the absent case to be right.
    pub retention: HashMap<String, RetentionPolicy>,
    /// Per-registry inputs for naming a package's resolution state, as plain
    /// data (Clone, cheap).
    ///
    /// The numbers here are already expressed twice over in `policies` —
    /// `artifact_ttl_secs` in the cache config, `min_age`/`bypass_roles` inside
    /// a `ReleaseAgeGateRule`. Neither is readable back out: the rule is a
    /// `Box<dyn Rule>` with no accessors, by design. The catalog has to answer
    /// "is this quarantined, is this past its TTL" without running the download
    /// path, so the builder records the same values in a shape it can read.
    /// They are built from the identical config fields in one place
    /// (`server/src/builders.rs`) so the two cannot say different things.
    pub resolution: HashMap<String, ResolutionPolicy>,
    /// Whether this registry mints and accepts signed download URLs, per
    /// registry (RFC 0012 §4.1). A bare `bool` for the same reason
    /// `console_fetch` is one: there is a single question to answer. An absent
    /// entry means **off**, which is the safe direction — a registry that never
    /// wrote the setting down keeps authenticating by header only.
    pub signed_downloads: HashMap<String, bool>,
    /// The instance signer for those URLs, or `None` when
    /// `[server.signed_urls]` is absent.
    ///
    /// In the hot config rather than in app data so a secret can be rotated by
    /// a config reload rather than a restart. It lives beside
    /// `signed_downloads` so one read-lock snapshot answers both halves of the
    /// question "may this registry mint, and with what" — a handler that had to
    /// take two locks could observe a registry switched on with the signer from
    /// before the reload.
    pub signed_url: Option<Arc<SignedUrlService>>,
    /// Maximum artifact size when buffering from upstream; None = 500 MiB default.
    pub max_artifact_size_bytes: Option<u64>,
    /// How many versions one package-detail answer carries — the default for a
    /// caller that asks for no `per_page`, and the ceiling on what one may ask
    /// for. From `[limits].versions_per_page`; see the config crate for why the
    /// two readings are one key.
    pub versions_per_page: u64,
    /// How many packages one catalog answer carries — the same two readings as
    /// `versions_per_page`, for the other list. From `[limits].packages_per_page`.
    ///
    /// A separate key rather than one shared number because the two answer
    /// different questions: a catalog row is a name and a handful of counts and
    /// 20 of them is a screenful, while a version row costs a vulnerability read
    /// and a licence read and 100 is about what one request should build. One
    /// key would force an operator sizing a screen to also size a query.
    pub packages_per_page: u64,
}

/// What a server with no `[limits].versions_per_page` serves, and what a
/// `HotConfig` built by hand — a test, an embedder — gets.
///
/// It lives here rather than in the config crate because `HotConfig` must be
/// able to answer without one: a `..Default::default()` that fell back to zero
/// would build a version table that can never return a row, and the config
/// crate's own default re-exports this constant so the two cannot drift.
pub const DEFAULT_VERSIONS_PER_PAGE: u64 = 100;

/// The same for `[limits].packages_per_page`, and 20 because that is what the
/// catalog has always drawn.
pub const DEFAULT_PACKAGES_PER_PAGE: u64 = 20;

impl Default for HotConfig {
    /// All maps empty, no size limit. Useful as a base for `..Default::default()`
    /// when only `registries`/`policies` (and occasionally one or two other fields)
    /// need to be set.
    fn default() -> Self {
        Self {
            registries: HashMap::new(),
            policies: HashMap::new(),
            namespace_policies: HashMap::new(),
            grants: HashMap::new(),
            instance: None,
            signing_keys: None,
            policy_tiers: HashMap::new(),
            grant_repo: None,
            ref_resolutions: None,
            rate_limit_budget: None,
            forge_refs: HashMap::new(),
            security: HashMap::new(),
            forge_raw: HashMap::new(),
            forge_api_reads: HashMap::new(),
            air_gap: crate::entities::AirGapPolicy::default(),
            miss_recorder: None,
            internal_scanners: HashMap::new(),
            verdicts: None,
            scan_queue: None,
            policy_repo: None,
            shadow_log: None,
            document_cache: None,
            versioning: HashMap::new(),
            signing: HashMap::new(),
            vsx_signing: HashMap::new(),
            sbom: HashMap::new(),
            readme: HashMap::new(),
            upstream_detail: HashMap::new(),
            console_fetch: HashMap::new(),
            feature_flags: HashMap::new(),
            integrity: HashMap::new(),
            beta_channel: HashMap::new(),
            retention: HashMap::new(),
            resolution: HashMap::new(),
            signed_downloads: HashMap::new(),
            signed_url: None,
            max_artifact_size_bytes: None,
            versions_per_page: DEFAULT_VERSIONS_PER_PAGE,
            packages_per_page: DEFAULT_PACKAGES_PER_PAGE,
        }
    }
}

/// Convenience alias: the shared hot-config lock used across services.
pub type HotConfigLock = Arc<RwLock<HotConfig>>;

/// Create a new `HotConfigLock` wrapping the given `HotConfig`.
pub fn new_hot_lock(cfg: HotConfig) -> HotConfigLock {
    Arc::new(RwLock::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::{new_hot_lock, HotConfig};

    fn empty_config() -> HotConfig {
        HotConfig::default()
    }

    #[test]
    fn new_hot_lock_is_readable() {
        let lock = new_hot_lock(empty_config());
        let guard = lock.blocking_read();
        assert!(guard.registries.is_empty());
        assert_eq!(guard.max_artifact_size_bytes, None);
    }

    #[test]
    fn new_hot_lock_is_writable() {
        let lock = new_hot_lock(empty_config());
        {
            let mut guard = lock.blocking_write();
            guard.max_artifact_size_bytes = Some(100);
        }
        let guard = lock.blocking_read();
        assert_eq!(guard.max_artifact_size_bytes, Some(100));
    }
}
