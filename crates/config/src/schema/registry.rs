use std::collections::HashMap;

use serde::Deserialize;

use super::network::{RateLimitConfig, UpstreamAuthConfig, UpstreamProxyConfig, UpstreamTlsConfig};
use super::rules::{RbacConfig, RuleConfig};
pub use batlehub_core::entities::Immutable;
use batlehub_core::entities::Visibility;

/// RFC 0015 §4.7 — this node's grants are in **shadow**: they resolve, the
/// would-have-been is recorded, and nothing is refused because of them.
///
/// # Why a block rather than `grants.dry_run`
///
/// §4.9 spells the flag `grants.dry_run`, which puts it inside `[…grants]`. That
/// block is a `subject → [verb]` **map**, so it can hold neither a boolean nor a
/// date — the reserved-key reading is unambiguous (no subject can be spelled
/// `dry_run`) and still does not typecheck.
///
/// A sibling block is better than the workarounds, and not only because it
/// compiles: **`until` is a required field**, so a shadow with no expiry cannot
/// be written at all. §4.7 asks config load to reject the flag without a
/// companion date, and *"a shadow mode that cannot be forgotten is the entire
/// point"* — a rejection the type performs is stronger than one a validator
/// remembers to.
///
/// ```toml
/// [registries.namespaces.grants_shadow]
/// until = "2026-12-01"
/// ```
///
/// Presence is the flag. There is deliberately no `enabled = false` spelling: a
/// block that says it does nothing is one more thing to misread on the page
/// listing everything currently failing open.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsShadowConfig {
    /// The date the shadow stops applying. **Required**, and config load refuses
    /// a date already past.
    pub until: chrono::NaiveDate,
}

/// One `[[registries.namespaces]]` block.
///
/// A namespace is **matched, not enumerated**: `match = "@acme/billing"` covers
/// `@acme/billing/cards` and `@acme/billing/ledger`. Matching is on segment
/// boundaries using the ecosystem's own separator, so `@acme/billing` never
/// matches `@acme/billing-internal` — the bug RFC 0011-bis §4.2 records for
/// `digital` versus `digital.pipeline-tools`.
///
/// RFC 0015 §4.1 gives the namespace tier `visibility`, `versioning`, `quota`
/// and `rules` beside its `grants`, and phase 4 adds them here. `retention` is
/// [RFC 0016](/rfc/0016-retention-and-the-permanence-of-a-published-name)'s
/// block and is not declared on this struct.
///
/// `deny_unknown_fields` stays, and is now what stops a `retention` block from
/// being silently ignored — an operator who writes a policy and gets no error
/// concludes it is in force.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceConfig {
    /// The namespace prefix, in the ecosystem's own spelling.
    #[serde(rename = "match")]
    pub match_prefix: String,
    /// See [`RegistryConfig::grants`] — absent inherits, empty seals.
    #[serde(default)]
    pub grants: Option<HashMap<String, Vec<String>>>,
    /// Default visibility for a version published into this namespace
    /// (RFC 0015 §4.5), replacing "public unless someone sets it".
    ///
    /// Composes **deepest wins** — it is a single value, so there is nothing to
    /// merge — and a per-package override remains (RFC 0011-bis §4.3).
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// A different default visibility for **pre-releases** published here
    /// (RFC 0015 §4.5), replacing `[registries.beta_channel]`.
    ///
    /// `beta_channel` existed because "pre-releases are for members only" could
    /// not be said any other way. As a conditional visibility default it is one
    /// line at the tier that owns the packages, it composes with everything else
    /// by §4.1's rules, and a version-tier `visibility` overrides it for the one
    /// build you want to show someone.
    #[serde(default)]
    pub prerelease_visibility: Option<Visibility>,
    /// What a version may be called here, and whether it may change.
    ///
    /// Composes **wholesale**, not per field: the motivating case is a narrower
    /// policy on a deeper tier, and a per-field merge cannot express dropping an
    /// inherited constraint. Wholesale is also greppable — what is on the node
    /// is what runs.
    #[serde(default)]
    pub versioning: Option<VersioningPolicy>,
    /// How much may be published here. Composes wholesale, like `versioning`.
    #[serde(default)]
    pub quota: Option<QuotaConfig>,
    /// Gate overrides for this namespace only.
    ///
    /// Composes **deepest wins, per rule** — each gate is independently
    /// configured, and a wholesale override would force redeclaring `cve_gate`
    /// and `license_gate` to change `release_age`. A forgotten one would then be
    /// a gate silently switched off, which is the fail-open direction.
    ///
    /// This is the piece that answers the standing `release_age` finding:
    /// `min_age_secs = 0` on the namespace your CI publishes to states the
    /// intent directly, instead of choosing between quarantining your own builds
    /// and turning the gate off everywhere.
    #[serde(default)]
    pub rules: Option<Vec<RuleConfig>>,
    /// RFC 0015 §4.7 — put this namespace's grants in shadow.
    #[serde(default)]
    pub grants_shadow: Option<GrantsShadowConfig>,
}

// ── Registry mode ─────────────────────────────────────────────────────────────

/// Controls whether a registry acts as a caching proxy, a private authoritative
/// registry, or both.
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RegistryMode {
    /// Forward all requests to upstream registries and cache responses.
    #[default]
    Proxy,
    /// BatleHub is the authoritative source; no upstream is consulted.
    Local,
    /// Check local publications first; fall back to upstream if not found.
    Hybrid,
}

impl RegistryMode {
    /// The wire name, matching the `serde(rename_all = "lowercase")` above and
    /// the `mode` field on every registry-shaped response.
    ///
    /// The `front_office` registry listing hand-rolled this match; RFC 0004-bis
    /// A2 put a `mode` on the admin health DTO too, and two copies of a mapping
    /// that must agree is one too many.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Local => "local",
            Self::Hybrid => "hybrid",
        }
    }
}

// ── Registry config ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegistryConfig {
    #[serde(rename = "type")]
    pub registry_type: String,
    pub name: String,
    /// RFC 0015 §4.3 — grants written on this registry.
    ///
    /// `subject → [verb]`, unioned with whatever `[registries.rbac]` translates
    /// to (§10: the old block "remains accepted indefinitely and is documented
    /// as the shorthand it becomes. There is no flag day.").
    ///
    /// **`Option` is load-bearing.** An absent block inherits — which at
    /// registry tier means "only the rbac translation applies" — and a block
    /// written as `grants = {}` is a *seal*: it grants nothing and stops
    /// inheritance. Those are different states, and collapsing them is the
    /// modelling rule survey finding 2 broke, where an empty accessible-registry
    /// list read as *every* registry.
    ///
    /// ```toml
    /// [registries.grants]
    /// "*"                = ["releases:read"]
    /// "group:*:engineer" = ["releases:read", "source:read"]
    /// ```
    #[serde(default)]
    pub grants: Option<HashMap<String, Vec<String>>>,
    /// RFC 0015 §4.1 — namespace nodes beneath this registry.
    #[serde(default)]
    pub namespaces: Vec<NamespaceConfig>,
    /// RFC 0015 §4.7 — put this **registry's** grants in shadow, which covers
    /// every namespace and package beneath it.
    ///
    /// The shape §10's migration needs: enable the new model in shadow, watch a
    /// week of real traffic, then enforce.
    #[serde(default)]
    pub grants_shadow: Option<GrantsShadowConfig>,
    /// Upstream URLs tried in order; if a registry returns 404 the next one is tried.
    /// When empty the adapter's built-in default (e.g. registry.npmjs.org) is used.
    #[serde(default)]
    pub upstreams: Vec<String>,
    /// Glob allowlist of upstream paths this registry may serve. Only meaningful
    /// for path-addressed registry types (`deb`/`rpm`/`pacman`/`jetbrains`/`generic`),
    /// where the request path is passed straight through to the upstream — without
    /// it, a registry pointed at a shared host (`storage.googleapis.com`, a CDN
    /// serving many vendors) would relay *every* path on that host.
    ///
    /// Patterns are matched against the upstream-relative path with
    /// [`glob::Pattern`] semantics, where `*` also crosses `/`. Required for
    /// `generic`; `["**"]` is the explicit opt-out that allows everything.
    ///
    /// ```toml
    /// path_allow = ["v*/node-v*-linux-x64.tar.*", "v*/SHASUMS256.txt*"]
    /// ```
    #[serde(default)]
    pub path_allow: Vec<String>,
    /// Extra hostnames whose **root** serves this registry, in addition to
    /// `/proxy/{name}/…`. Everything reachable under the subpath is reachable
    /// identically at the root of each of these hosts.
    ///
    /// ```toml
    /// hosts = ["npm.acme.io"]
    /// ```
    ///
    /// Independent of `[subdomain_routing]`: a registry can have vanity hosts
    /// with no wildcard configured at all, and an explicit entry wins over this
    /// registry's own wildcard host. A host claimed by two registries is a config
    /// error. DNS and TLS for these names are the operator's job.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Whether `/proxy/{name}/…` serves this registry. Defaults to `true`.
    ///
    /// `false` makes the registry reachable **only** through its `hosts` (or its
    /// wildcard host), so content handed to a team under `npm.acme.io` does not
    /// also answer on the shared main host, where it would inherit that host's
    /// CORS policy, WAF rules and cache keys. The subpath then returns `404`, not
    /// `403` — a disabled ingress should look absent, not forbidden.
    ///
    /// `path_routing = false` on a registry with no reachable host is a config
    /// error: nothing could talk to it.
    #[serde(default = "default_true")]
    pub path_routing: bool,
    /// Cargo only: URL of the sparse crate index.
    /// Defaults to `https://index.crates.io` when the upstream is crates.io.
    /// Set this for self-hosted registries (e.g. Gitea/Forgejo package feeds).
    #[serde(default)]
    pub index_url: Option<String>,
    /// SDKMAN only: URL of the download broker, the second host of the one
    /// protocol. Defaults to `https://broker.sdkman.io`; `upstreams` is the
    /// candidates API (`https://api.sdkman.io/2`). Rejected on any other type,
    /// for the same reason `index_url` would be on a non-cargo registry: a
    /// silently ignored option is a misconfiguration that looks like a proxy
    /// bug (RFC 0010 §4.1, §4.5).
    #[serde(default)]
    pub broker_url: Option<String>,
    /// rustup only: components this registry never serves, whatever the
    /// channel manifest says.
    ///
    /// Names are manifest package names (`rust-docs`, `clippy-preview`),
    /// before `[renames]` are applied, because that is the table the filter
    /// edits; `[A-Za-z0-9_-]+`, which is every component the tree publishes.
    /// A profile install proceeds without them, and an explicit `rustup
    /// component add` stops before any request on rustup's own "toolchain '…'
    /// does not contain component '…' for target '…'".
    /// Empty — the default — denies nothing. Rejected on any other type, for
    /// the reason `broker_url` is (RFC 0024 §4.1, §4.5).
    #[serde(default)]
    pub deny_components: Vec<String>,
    /// galaxy only: how much of Ansible Galaxy's **v1 role** surface this
    /// registry serves (RFC 0031 §4.4).
    ///
    /// - `proxy` (the default when absent) — the v1 read endpoints are served,
    ///   each role version's `download_url` is rewritten to this instance, and
    ///   the archive is fetched server-side from the host it names, through the
    ///   SSRF guard and only from the fixed role-download allowlist.
    /// - `index` — the same endpoints with `download_url` relayed. Role
    ///   metadata is proxied; role bytes are not, so the server makes no egress
    ///   to `github.com`.
    /// - `off` — the v1 endpoints answer `404` **and** `v1` is absent from the
    ///   discovery document, so `ansible-galaxy role install` fails on the
    ///   client's own *"requires API versions 'v1'"* rather than on a `404` an
    ///   operator reads as a proxy fault.
    ///
    /// Rejected on any other type, for the reason `broker_url` and
    /// `deny_components` are: a silently ignored option is a misconfiguration
    /// nobody sees.
    #[serde(default)]
    pub roles: Option<GalaxyRoleMode>,
    #[serde(default)]
    pub cache: CachePolicy,
    #[serde(default)]
    pub rbac: RbacConfig,
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
    /// Name of the storage backend to use for this registry's artifacts.
    /// Must match one of the backend names in `[[storage.backends]]`.
    /// When absent, the default backend is used.
    #[serde(default)]
    pub storage: Option<String>,
    /// When `true` the registry acts as a pure firewall: rules are evaluated but
    /// artifacts are never cached. Requests that pass rules are streamed directly
    /// from upstream with nothing written to storage.
    #[serde(default)]
    pub firewall_only: bool,
    /// Mint signed, expiring, single-coordinate download URLs in the protocol
    /// documents of this registry, and accept them on the artifact routes that
    /// the client fetches without an `Authorization` header (RFC 0012).
    ///
    /// Terraform is the case this exists for: it authenticates the two JSON
    /// documents of a provider install and then fetches the archive, its
    /// `SHA256SUMS` and the `.sig` with no credential — measured, not read
    /// (RFC 0012 §11). Without this, such a registry needs
    /// `anonymous = ["releases:read", "source:read"]`, which opens *every* read
    /// on it rather than the one step that needs opening.
    ///
    /// Requires `[server.signed_urls].secret`; setting it without one is a
    /// startup error, because a registry that believes it is closed and is not
    /// is the failure this feature exists to prevent.
    #[serde(default)]
    pub signed_downloads: bool,
    /// `cargo` only: force `"auth-required"` in the sparse index's
    /// `config.json` on or off, instead of deriving it.
    ///
    /// The field tells cargo that "this is a private registry that requires all
    /// operations to be authenticated including API requests, crate downloads
    /// and sparse index updates" — and without it cargo sends **no credential
    /// at all** on a read, so a registry that refuses anonymous callers is
    /// simply unusable rather than authenticated.
    ///
    /// Left unset it is derived: `true` when an anonymous caller cannot read
    /// this registry. The derivation reads the *registry* tier, so it is wrong
    /// in one direction — a registry that closes the tier and then re-opens one
    /// package to `*` through a grant would be advertised as fully closed, and
    /// cargo would demand a token for the open package too. That is the case
    /// this knob exists for.
    #[serde(default)]
    pub cargo_auth_required: Option<bool>,
    /// Credentials to send on every upstream request for this registry.
    #[serde(default)]
    pub upstream_auth: Option<UpstreamAuthConfig>,
    /// TLS settings for upstream connections (e.g. custom CA certificate).
    #[serde(default)]
    pub tls: Option<UpstreamTlsConfig>,
    /// Optional HTTP/SOCKS proxy for upstream connections.
    #[serde(default)]
    pub proxy: Option<UpstreamProxyConfig>,
    /// Controls proxy vs. local vs. hybrid behaviour for this registry.
    #[serde(default)]
    pub mode: RegistryMode,
    /// Optional publish quota enforced on local/hybrid registries.
    #[serde(default)]
    pub quota: Option<QuotaConfig>,
    /// Optional per-user request rate limit for this registry.
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    /// Optional versioning policy enforced at publish time (local/hybrid mode only).
    #[serde(default)]
    pub versioning: Option<VersioningPolicy>,
    /// RFC 0015 §4.1 — the registry-tier default visibility, inherited by every
    /// namespace and package beneath it that does not set its own.
    ///
    /// Visibility is one of the policies §4.1 notes has "no registry-level
    /// expression at all" today; the tier system regularises what is there
    /// rather than inventing it. Absent means `public`, which is the behaviour
    /// on every existing instance.
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// RFC 0015 §4.5 — the registry-tier default visibility for pre-releases.
    ///
    /// This is what `[registries.beta_channel]` translates to (§10 rule 6), and
    /// it is accepted on proxy-mode registries — where it is inert — because
    /// `beta_channel` carries no mode restriction today and refusing it would
    /// stop such an instance booting on upgrade. §4.9 warns instead.
    #[serde(default)]
    pub prerelease_visibility: Option<Visibility>,
    /// RFC 0014 §13 O6 — the registry-tier `on_confirmed`: what the upstream
    /// audit does with a disappearance confirmed on *this* registry,
    /// `"audit"` or `"block"`. Deepest wins: set, it overrides
    /// `[upstream_audit] on_confirmed` for this registry alone; absent, the
    /// estate's key applies. `"block"` needs the audit enabled and this
    /// registry audited, which §4.9-style validation checks.
    #[serde(default)]
    pub on_confirmed: Option<String>,
    /// Optional artifact signing configuration (local/hybrid mode only).
    #[serde(default)]
    pub signing: Option<SigningConfig>,
    /// Optional Ed25519 OpenPGP key for signing generated Deb/RPM repository
    /// metadata (`Release`/`InRelease`/`Release.gpg`, `repomd.xml.asc`). When
    /// absent, the hosted repository is unsigned.
    #[serde(default)]
    pub repo_signing: Option<RepoSigningConfig>,
    /// Optional Ed25519 key a `vscode-marketplace`/`openvsx` registry signs
    /// every VSIX it publishes with (RFC 0020). The signature is served as the
    /// gallery's `VsixSignature` asset, which is what a current editor's
    /// Extensions view requires before it enables Install.
    #[serde(default)]
    pub vsx_signing: Option<VsxSigningConfig>,
    /// Optional Ed25519 key a `nix` registry signs the narinfos it hosts with
    /// (RFC 0028 §4.1). `local`/`hybrid` only: a proxied narinfo keeps the
    /// upstream's `Sig:` lines byte-exact and is never re-signed.
    ///
    /// Absent in `local`/`hybrid` mode is allowed and warned about — every
    /// stock client runs with `require-sigs = true` and refuses an unsigned
    /// path, but a fleet that has turned it off is a legitimate lab.
    #[serde(default)]
    pub nix_signing: Option<NixSigningConfig>,
    /// Refuse to relay a `nix` narinfo that carries no `Sig:` line at all.
    ///
    /// Off by default for two reasons: a content-addressed path (`CA:`)
    /// legitimately has none — Nix's own `isContentAddressed` short-circuits
    /// `checkSignatures` — and the client's `require-sigs` is the check that
    /// actually protects the store. On, it closes the one case the client
    /// cannot see: an upstream mirror that silently dropped signatures
    /// (RFC 0028 §4.1).
    #[serde(default)]
    pub require_upstream_sigs: bool,
    /// Optional RSA key an `apk` registry signs its generated `APKINDEX.tar.gz`
    /// with (RFC 0026 §4.1). `local`/`hybrid` only: in proxy mode the upstream
    /// index is relayed byte-exact and there is nothing to sign.
    #[serde(default)]
    pub apk_signing: Option<ApkSigningConfig>,
    /// Explicitly ship an `apk` repository whose index is unsigned.
    ///
    /// An unsigned index is uninstallable by every apk that ships unless the
    /// client passes `--allow-untrusted`, which also switches off the package
    /// identity check — so this is a choice put on the record rather than a
    /// default anything inherits. Kind-prefixed like the signing fields beside
    /// it: a bare `unsigned` on a struct shared by every registry type would
    /// read as a promise the other kinds do not keep.
    #[serde(default)]
    pub apk_unsigned: bool,
    /// Optional beta-channel configuration (local/hybrid mode only).
    /// When enabled, pre-release versions are only visible to registered beta-channel members.
    #[serde(default)]
    pub beta_channel: Option<BetaChannelConfig>,
    /// Base URL of the upstream search API used by the Package Explorer.
    ///
    /// When absent, each registry type falls back to its built-in default:
    /// - `maven`    → `https://search.maven.org`
    /// - `composer` → `https://packagist.org` (for packagist.org-based repos)
    ///
    /// Set to `""` (empty string) to disable upstream search for this registry.
    /// Has no effect on registry types that do not support upstream search.
    #[serde(default)]
    pub search_url: Option<String>,
    /// Optional SBOM generation configuration. When absent, SBOM is disabled.
    #[serde(default)]
    pub sbom: Option<SbomConfig>,
    /// Optional README capture configuration.
    ///
    /// Unlike [`Self::sbom`], **absent means enabled**: for the metadata-borne
    /// registry kinds the text is a field of a document the proxy already
    /// fetches and parses, so the default costs one deserialised field
    /// (RFC 0007 §4.1).
    #[serde(default)]
    pub readme: Option<ReadmeConfig>,
    /// `[registries.security]` — the quarantine profile (RFC 0018 §4.1).
    /// Absent means no quarantine: the gates run as rules, exactly as before.
    #[serde(default)]
    pub security: Option<super::security::SecurityConfig>,
    /// `[registries.refs]` — how long a forge ref → commit resolution is
    /// trusted (RFC 0019 §4.1). Forge kinds only; validation refuses it
    /// elsewhere. Absent means the defaults: branches every minute, tags every
    /// hour.
    #[serde(default)]
    pub refs: Option<super::forge::RefsConfig>,
    /// `[registries.raw]` — raw file serving, off unless written (RFC 0019
    /// §4.1, phase 3).
    #[serde(default)]
    pub raw: Option<super::forge::RawConfig>,
    /// `[registries.api_reads]` — the typed read-only JSON routes to add
    /// beside the release listing.
    #[serde(default)]
    pub api_reads: Option<super::forge::ApiReadsConfig>,
    /// Optional configuration for the console's discovery read — whether this
    /// instance may ask upstream about a package it holds nothing of.
    ///
    /// A separate block from [`Self::readme`] because it is not a README
    /// setting: it governs the version list too, and an operator may want one
    /// without the other. **Absent means enabled** (RFC 0007 §4.1).
    #[serde(default)]
    pub upstream_detail: Option<UpstreamDetailConfig>,
    /// Whether the console may ask this instance to fetch a version from
    /// upstream — the **Fetch this version** button (RFC 0007-bis §4.1, §4.4).
    ///
    /// **On by default**, and it admits nothing: the fetch runs the same
    /// download the caller could already run with `curl`, through every gate
    /// that download would pass, attributed to them in the audit log. The switch
    /// exists for the operator who wants the console strictly read-only, which
    /// is a legitimate posture and not one the software should have to guess at.
    ///
    /// Inert on a `local`-mode registry: there is no upstream to fetch from, and
    /// every version the page lists is already held.
    #[serde(default = "default_true")]
    pub console_fetch: bool,
    /// Optional per-registry feature flags (opt-in/out toggles for cross-cutting
    /// UI/integration features). When absent, every flag takes its default.
    #[serde(default)]
    pub feature_flags: Option<FeatureFlagsConfig>,
    /// Optional artifact integrity (checksum) verification on proxied downloads.
    /// When absent, the defaults apply: verify against any advertised checksum
    /// and block on a mismatch; warn (do not block) when none is advertised.
    #[serde(default)]
    pub integrity: Option<IntegrityConfig>,
    /// Base URL for the Go Vulnerability Database (`govulndb`) proxy endpoints.
    /// Only applies to `goproxy` registries.
    /// Absent → default to `https://vuln.go.dev`.
    /// Set to `""` to disable the `/v1/index.json`, `/v1/ID/{id}.json`, and
    /// `/v1/query` passthrough endpoints for this registry.
    #[serde(default)]
    pub vuln_db_url: Option<String>,
    /// Base URL for the Go checksum database (`GOSUMDB`) proxy endpoint.
    /// Only applies to `goproxy` registries.
    /// Absent → default to `https://sum.golang.org`.
    /// Set to `""` to disable `/sumdb/{path}` for this registry.
    ///
    /// Proxying the checksum database is the other half of `GOPROXY`
    /// (RFC 0009 §7.4). Without it `go mod download` still opens a direct
    /// connection to `sum.golang.org` for every module it has not seen, which
    /// fails closed in an air-gapped estate — so the proxy has moved the egress
    /// rather than removed it. A registry serving only private modules has no
    /// sumdb and should set `""`, because a lookup there would leak private
    /// module paths upstream.
    #[serde(default)]
    pub sumdb_url: Option<String>,
    /// Optional retention policy for what this registry holds *locally*
    /// (RFC 0016). Absent means keep everything, forever, which is what every
    /// instance does today.
    ///
    /// Not to be confused with [`CachePolicy`]'s `idle_days`/`keep_latest_n`,
    /// which govern the **proxy cache** and are a different problem: an evicted
    /// cache entry is re-fetchable from upstream, a reclaimed local version is
    /// frequently the only copy in existence (RFC 0016 §5.1). The two are
    /// deliberately separate blocks with opposite defaults, and this one must not
    /// be implemented by widening that one.
    #[serde(default)]
    pub retention: Option<RetentionConfig>,
}

// ── Local retention ────────────────────────────────────────────────────────────

/// Retention policy for locally published versions and for the tombstones they
/// leave behind (RFC 0016).
///
/// Valid at **registry tier**. The namespace and package tiers RFC 0016 §4.1
/// describes need RFC 0015's namespace config blocks and its `policy` table,
/// neither of which exists; the version-tier `keep` pin does *not* need them and
/// is a column on the version row, set through the admin API beside
/// `yanked`/`unlisted`.
///
/// # Keep conditions are a union of vetoes
///
/// **A version survives if *any* configured condition matches.** There is no
/// expression to write and no ordering to get wrong: the only way to reclaim a
/// version is for every configured condition to decline to keep it. Wrong
/// configuration therefore fails toward keeping, which is the direction that is
/// recoverable (RFC 0016 §4.2).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    /// Keep the newest N versions of every package, by publish date.
    ///
    /// Alone, this throws away the version half the estate is pinned to because
    /// it happens to be N+1th by date. Pair it with `keep_if_pulled_days`.
    #[serde(default)]
    pub keep_versions: Option<u32>,
    /// Keep anything **published** within this many days.
    #[serde(default)]
    pub keep_for_days: Option<u32>,
    /// Keep anything **downloaded** within this many days.
    ///
    /// The rule that makes retention safe to switch on: whatever anyone is
    /// actually using stays, regardless of age or count (RFC 0016 §4.3).
    ///
    /// Reads the download signal, so it also reads
    /// [`Self::download_signal_floor_days`] — absence of a pull record from
    /// before the floor is not evidence of disuse.
    #[serde(default)]
    pub keep_if_pulled_days: Option<u32>,
    /// Keep yanked versions. **On by default**: a yank says "do not install
    /// this", which is a reason to stop resolving it and not a reason to destroy
    /// the only copy — a yanked version is frequently the one an incident is
    /// still being investigated against.
    #[serde(default = "default_true")]
    pub keep_yanked: bool,
    /// The date before which "no recorded download" proves nothing (RFC 0016 §4.3).
    ///
    /// Expressed as days before now. The Maven and NuGet local artifact paths
    /// recorded no download event at all until the 2026-08-26 survey
    /// remediation, so for those ecosystems the audit trail is silent for that
    /// period, and a retention run that read the silence as disuse would reclaim
    /// versions the estate was using every day.
    ///
    /// A version whose only evidence is older than the floor is **kept**. Unset
    /// uses the built-in floor, which is the remediation date itself; set it
    /// explicitly if this instance's audit history begins later — after a
    /// restore from backup, say, or an `audit_purge`.
    #[serde(default)]
    pub download_signal_floor_days: Option<u32>,
    /// Milliseconds to wait between reclamations in one run.
    ///
    /// A first live run on an estate that has never reclaimed anything may drop
    /// a large fraction of its storage in one pass. A rate limit rather than a
    /// per-run cap, per RFC 0016 §11: a cap leaves the estate mid-reclamation in
    /// a state nothing else models, where every intermediate state of a paced
    /// run is a valid one.
    ///
    /// `0` — the default — paces nothing.
    #[serde(default)]
    pub reclaim_delay_ms: u64,
    /// Days after deletion at which a tombstone's *detail* — index metadata,
    /// checksum, publisher, signature — is stripped, keeping the coordinate
    /// claim forever (RFC 0016 §4.5).
    ///
    /// **Unset by default.** Disk is recoverable; a question an auditor can no
    /// longer answer is not. Nothing is stripped until an operator asks for it.
    ///
    /// There is deliberately no setting that *removes* a tombstone. Not "off by
    /// default" — absent from the schema, so it cannot be reached by an operator
    /// in a hurry, because collecting a tombstone reopens the hole tombstones
    /// exist to close.
    #[serde(default)]
    pub tombstone_detail_for_days: Option<u32>,
    /// Report what a run would reclaim or strip, and change nothing. **On by
    /// default**, so a configured policy does nothing until the operator has
    /// read a report and turned it off.
    ///
    /// Retention is the one policy whose dry-run direction is unambiguously safe
    /// — the system does less — which is why it is the only one that defaults to
    /// on (RFC 0016 §4.2).
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

impl RetentionConfig {
    /// Whether any *reclamation* keep condition is configured — i.e. whether a
    /// retention run would do anything at all.
    ///
    /// `keep_yanked` is excluded on purpose: it defaults to `true` and only ever
    /// vetoes, so a block containing nothing else describes a policy that
    /// reclaims every unyanked version. That is the configuration validation
    /// refuses, and it must not look configured here.
    pub fn reclaims_anything(&self) -> bool {
        self.keep_versions.is_some()
            || self.keep_for_days.is_some()
            || self.keep_if_pulled_days.is_some()
    }
}

/// Hand-written rather than derived: `#[derive(Default)]` would make `dry_run`
/// and `keep_yanked` **false**, which is the opposite of what the `serde`
/// defaults above say and of what RFC 0016 §4.2 requires. A struct whose two
/// defaults disagree is the kind of divergence that only shows up when something
/// destructive runs.
impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            keep_versions: None,
            keep_for_days: None,
            keep_if_pulled_days: None,
            keep_yanked: true,
            download_signal_floor_days: None,
            reclaim_delay_ms: 0,
            tombstone_detail_for_days: None,
            dry_run: true,
        }
    }
}

// ── Artifact integrity ──────────────────────────────────────────────────────────

/// Per-registry artifact integrity verification, applied on the proxy
/// fetch-and-cache path. Once upstream bytes are buffered they are hashed and
/// compared against the checksum advertised in the registry metadata
/// (Cargo SHA-256, npm SRI/`shasum`, PyPI SHA-256). Registries that advertise no
/// checksum (NuGet, Maven, GitHub, Go, …) fall through to the "missing" path.
///
/// Does **not** apply to `firewall_only` registries, which stream straight
/// through without buffering.
///
/// ```toml
/// [registries.integrity]
/// enabled = true            # verify when a checksum is advertised
/// block_on_mismatch = true  # fail the download on a hash mismatch (never bypassable)
/// require_metadata = false  # block downloads with no advertised checksum
/// bypass_roles = ["admin"]  # roles exempt from the require_metadata gate
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct IntegrityConfig {
    /// Master switch. When `false`, no verification is performed.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Fail the download (and skip caching) when the computed digest does not
    /// match the advertised one. A mismatch is never bypassable.
    #[serde(default = "default_true")]
    pub block_on_mismatch: bool,
    /// Block downloads for which the upstream advertises no usable checksum,
    /// unless the caller holds one of `bypass_roles`. Defaults to `false`
    /// (missing checksums are only warned about).
    #[serde(default)]
    pub require_metadata: bool,
    /// Roles allowed to bypass the `require_metadata` gate.
    #[serde(default)]
    pub bypass_roles: Vec<String>,
    /// Re-verify cached/stored bytes against a self-computed SHA-256 on **every**
    /// serve (cache hit on the proxy path, and local-registry reads), not just on
    /// the first fetch. Catches storage corruption or tampering of already-cached
    /// artifacts. Off by default: it reads and hashes the bytes on each serve (the
    /// proxy path streams them through the hash, so memory stays bounded, then
    /// re-opens the entry to serve it). A mismatch fails the download (`502`) and
    /// evicts the bad entry.
    #[serde(default)]
    pub verify_on_serve: bool,
}

impl Default for IntegrityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            block_on_mismatch: true,
            require_metadata: false,
            bypass_roles: Vec::new(),
            verify_on_serve: false,
        }
    }
}

// ── Feature flags ─────────────────────────────────────────────────────────────

/// Per-registry "feature flag" category: a set of named boolean toggles for
/// optional, cross-cutting features that can be turned on or off for a whole
/// registry. New opt-in features add a new field here.
///
/// ```toml
/// [registries.feature_flags]
/// socket_badge = false   # hide the socket.dev badge for this registry
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct FeatureFlagsConfig {
    /// Show a [socket.dev](https://socket.dev) supply-chain badge/link for each
    /// package version in the UI (for registry types socket.dev supports, e.g.
    /// `cargo`, `npm`, `pypi`). Enabled by default; set to `false` to disable
    /// the badge for the whole registry.
    #[serde(default = "default_true")]
    pub socket_badge: bool,
}

impl Default for FeatureFlagsConfig {
    fn default() -> Self {
        Self {
            socket_badge: default_true(),
        }
    }
}

// ── Versioning policy ─────────────────────────────────────────────────────────

/// Versioning policy: what a version may be called, and whether it may change.
///
/// Registry-level today and namespace-level as of RFC 0015 phase 4 — which is
/// most of the value on its own, since a policy that is right for
/// `com.acme.internal` is rarely right for the vendored third-party namespace
/// beside it, and one setting per registry forces the looser of the two.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VersioningPolicy {
    /// Reject publish if the version string is not a valid semver (e.g. `1.2.3`, `1.0.0-beta.1`).
    #[serde(default)]
    pub enforce_semver: bool,
    /// Reject publish if the semver pre-release component is non-empty (e.g. `-alpha`, `-beta.1`).
    /// Only effective when `enforce_semver` is also `true`.
    #[serde(default = "default_true")]
    pub allow_prerelease: bool,
    /// Reject publish if the version string does not match this regex.
    #[serde(default)]
    pub version_pattern: Option<String>,
    /// Whether these bytes may be replaced (RFC 0015 §4.5).
    ///
    /// The one field of this block honoured at **version** tier, where freezing
    /// a single golden build inside a namespace that otherwise permits
    /// replacement is the motivating case (§4.1).
    #[serde(default)]
    pub immutable: Immutable,
    /// Refuse a publish whose version does not sort strictly above the newest
    /// existing one for that package (RFC 0015 §4.5).
    ///
    /// Catches what `immutable` cannot: republishing an *older* number after a
    /// bad release, which leaves a resolver picking a version that was never
    /// meant to come back. Ordered by
    /// [`version_order::newest_first`](batlehub_core::services::version_order::newest_first),
    /// already the single ordering function in the tree.
    ///
    /// Three consequences, each stated in §4.5 rather than left to be
    /// discovered:
    ///
    /// - **A yanked or deleted version still counts** as the newest, which RFC
    ///   0016's soft delete is what makes possible. Otherwise deleting `2.0.0`
    ///   would let `1.9.9` be re-taken.
    /// - **Pre-releases fall out correctly** with no special case: `1.3.0-rc1`
    ///   sorts above `1.2.0` and is accepted, while `1.2.0-rc1` after `1.2.0`
    ///   sorts below and is refused.
    /// - **Bulk import is incompatible with it** by construction, since a
    ///   package's history publishes oldest-first. Import with `monotonic =
    ///   false` and turn it on afterwards; there is deliberately no bypass verb.
    #[serde(default)]
    pub monotonic: bool,
    /// Evaluate fully, record what would have been refused, refuse nothing
    /// (RFC 0015 §4.7).
    ///
    /// **Direction: mixed.** A badly-named or duplicate version is accepted, so
    /// bad data lands — but nothing leaks. That is why this one has no expiry
    /// requirement while `grants.dry_run` does: forgetting it costs a messy
    /// registry, where forgetting the other one is an authorization bypass.
    ///
    /// Defaults to `false`, as §4.7 requires. Only `retention.dry_run` defaults
    /// to `true`, and RFC 0016 argues that from the fact that it is the only one
    /// of the three whose dry-run direction is unambiguously safe.
    #[serde(default)]
    pub dry_run: bool,
}

pub fn default_true() -> bool {
    true
}

// ── Artifact signing ──────────────────────────────────────────────────────────

/// Per-registry artifact signing configuration.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SigningConfig {
    /// When `true`, reject publish requests that do not include an `X-Artifact-Signature` header.
    #[serde(default)]
    pub required: bool,
    /// Accepted signature types (e.g. `["pgp", "ed25519"]`).
    /// When empty, any type (or no type) is accepted.
    #[serde(default)]
    pub allowed_types: Vec<String>,
    /// When `true`, verify a stored `ed25519` detached signature against
    /// `trusted_keys` on every download. A stored signature that fails to verify
    /// (or was signed by an untrusted key) fails the download with `502`.
    /// A stored signature of an *unsupported* type (anything other than
    /// `ed25519`, which cannot be verified here) is likewise **rejected** with
    /// `502` — the download fails closed rather than serving unverified bytes.
    /// Only artifacts with *no* stored signature are exempt (their presence is
    /// governed by `required` at publish time).
    #[serde(default)]
    pub verify_on_download: bool,
    /// Hex-encoded 32-byte Ed25519 public keys trusted to sign artifacts in this
    /// registry. A download verifies against each in turn; any match passes.
    #[serde(default)]
    pub trusted_keys: Vec<String>,
}

/// Ed25519 repository-metadata signing key for `deb`/`rpm` registries.
///
/// ```toml
/// [registries.repo_signing]
/// seed_hex = "9d61b19d..."   # 32-byte Ed25519 seed, hex-encoded
/// user_id  = "BatleHub Repo <repo@example.com>"
/// created  = 1700000000      # key creation unix time (stable across restarts)
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoSigningConfig {
    /// Hex-encoded 32-byte Ed25519 seed.
    pub seed_hex: String,
    /// OpenPGP User ID string. Defaults to `"BatleHub"`.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Key creation time (unix seconds). Part of the fingerprint, so it must stay
    /// stable. Defaults to 0.
    #[serde(default)]
    pub created: Option<u32>,
}

/// RSA signing key for a locally hosted `apk` repository's index
/// (RFC 0026 §4.1).
///
/// ```toml
/// [registries.apk_signing]
/// key_name        = "internal-apk@example.com-5f3a1c2e.rsa.pub"
/// private_key_pem = "${APK_SIGNING_KEY_PEM}"
/// ```
///
/// `key_name` is the exact file name the client will hold under
/// `/etc/apk/keys/`, because apk opens the key *by the name the signature entry
/// carries* (`package.c:589`). A mismatch is not an error the client reports as
/// one — it is an untrusted index — so the name is validated here and served
/// back verbatim on the key route.
///
/// RSA rather than Ed25519 because no shipping apk accepts Ed25519 for a v2
/// index, and without the banned `rsa` crate: the signing goes through
/// `aws-lc-rs`, already this tree's TLS provider (RFC 0026 §2.4, decision 2).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApkSigningConfig {
    /// The `.rsa.pub` file name the public half lands under in
    /// `/etc/apk/keys/`. Alpine's own convention is `<email>-<8 hex>.rsa.pub`.
    pub key_name: String,
    /// RSA private key, PEM (PKCS#8 or PKCS#1), 2048 bits or more. A secret of
    /// the same class as `repo_signing.seed_hex`: keep it out of the file with
    /// `${VAR}`. Never written back by the config editor.
    pub private_key_pem: String,
    /// Keys this one replaced, newest first — the rotation window.
    ///
    /// The index is signed with `key_name` **and then** with each of these, in
    /// order, in one signature member. apk installs from the first `.SIGN.*`
    /// entry whose key file it holds, so a fleet where some machines have the
    /// new key and some do not keeps installing from both. Drop an entry once
    /// every client has the new file; an empty list is a completed rotation
    /// (RFC 0026 §11 decision 9).
    #[serde(default)]
    pub previous_keys: Vec<ApkSigningConfig>,
}

/// Ed25519 VSIX signing key for `vscode-marketplace`/`openvsx` registries
/// (RFC 0020 §4.1).
///
/// ```toml
/// [registries.vsx_signing]
/// seed_hex = "${VSX_SIGNING_SEED}"   # 32-byte Ed25519 seed, hex-encoded
/// key_id   = "2026-09"               # optional; default: 16 hex chars of SHA-256(public key)
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VsxSigningConfig {
    /// Hex-encoded 32-byte Ed25519 seed. A secret of the same class as
    /// `repo_signing.seed_hex`: keep it out of the file with `${VAR}`.
    pub seed_hex: String,
    /// The id the public key is served under
    /// (`/proxy/{registry}/api/-/public-key/{key_id}`). Must change when the
    /// key does; the default derives it from the key, so it does.
    #[serde(default)]
    pub key_id: Option<String>,
}

/// Ed25519 narinfo signing key for a `nix` registry (RFC 0028 §4.1).
///
/// ```toml
/// [registries.nix_signing]
/// seed_hex = "${NIX_SIGNING_SEED}"   # 32-byte Ed25519 seed, hex-encoded
/// key_name = "batlehub-nix-1"        # optional; default: batlehub-{registry}-1
/// ```
///
/// `key_name` is the half of a `trusted-public-keys` entry before the colon,
/// and it is how Nix picks which key verifies a `Sig:`. It has to be stable
/// across restarts and unique among the caches a client trusts — hence a
/// default carrying the registry name, and a `-1` suffix an operator bumps on
/// rotation exactly as `cache.nixos.org-1` does.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NixSigningConfig {
    /// Hex-encoded 32-byte Ed25519 seed. A secret of the same class as
    /// `vsx_signing.seed_hex`: keep it out of the file with `${VAR}`.
    pub seed_hex: String,
    /// The name before the colon in `Sig:` and in `trusted-public-keys`.
    #[serde(default)]
    pub key_name: Option<String>,
}

impl NixSigningConfig {
    /// The key name this registry signs under — the configured one, or the
    /// default derived from the registry's name.
    pub fn resolved_key_name(&self, registry: &str) -> String {
        self.key_name
            .clone()
            .unwrap_or_else(|| format!("batlehub-{registry}-1"))
    }
}

// ── SBOM generation ───────────────────────────────────────────────────────────

fn default_sbom_formats() -> Vec<String> {
    vec!["spdx".to_owned(), "cyclonedx".to_owned()]
}

/// Per-registry SBOM generation configuration.
///
/// ```toml
/// [registries.sbom]
/// enabled        = true
/// formats        = ["spdx", "cyclonedx"]
/// required       = false   # deny publish when no manifest found
/// fetch_upstream = true    # try GitHub/npm upstream SBOM APIs first
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SbomConfig {
    /// Enable SBOM generation for this registry.
    #[serde(default)]
    pub enabled: bool,
    /// Formats to generate. Defaults to both when enabled.
    #[serde(default = "default_sbom_formats")]
    pub formats: Vec<String>,
    /// When `true`, deny publish if no dependency manifest can be found in the archive.
    #[serde(default)]
    pub required: bool,
    /// When `true`, attempt to fetch a pre-built SBOM from the upstream before
    /// falling back to extraction / minimal generation.
    #[serde(default = "default_true")]
    pub fetch_upstream: bool,
}

// ── README capture ────────────────────────────────────────────────────────────

fn default_readme_max_bytes() -> usize {
    batlehub_core::services::DEFAULT_README_MAX_BYTES
}

fn default_remote_images() -> String {
    "strip".to_owned()
}

fn default_image_max_bytes() -> usize {
    batlehub_core::services::DEFAULT_README_IMAGE_MAX_BYTES
}

/// Per-registry README capture configuration.
///
/// ```toml
/// [registries.readme]
/// enabled       = true      # store and serve READMEs for this registry
/// from_archive  = true      # extract from the cached artifact when the metadata carries none
/// max_bytes       = 262144    # cap on stored source (256 KiB); larger is truncated and flagged
/// remote_images   = "strip"   # "strip" | "proxy"
/// image_max_bytes = 2097152   # cap on one proxied image (2 MiB); larger is not served
/// ```
///
/// The whole block is optional and its absence means **on**, which is why every
/// field defaults to the enabled shape rather than to `false`/zero. `from_archive`
/// is the one part of that default that is not free: it rides the artifact read
/// SBOM already performs when SBOM is on, and adds one storage read per
/// newly-cached version when it is not.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadmeConfig {
    /// Store and serve READMEs for this registry.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Read the README out of the artifact when the metadata carries none. Inert
    /// on kinds whose README is metadata-borne only, and on `firewall_only`
    /// registries, which never cache an artifact to extract from — both warned
    /// about rather than rejected.
    #[serde(default = "default_true")]
    pub from_archive: bool,
    /// Cap on the stored source, in bytes, applied after decompression.
    /// Truncation is recorded and surfaced, never silent.
    #[serde(default = "default_readme_max_bytes")]
    pub max_bytes: usize,
    /// Which hosts an image may be proxied *from*, when `remote_images =
    /// "proxy"`.
    ///
    /// ```toml
    /// [registries.readme]
    /// remote_images = "proxy"
    /// remote_image_hosts = ["img.shields.io", "badgen.net", "codecov.io"]
    /// ```
    ///
    /// An entry matches the host exactly or any subdomain of it, so
    /// `"shields.io"` covers `img.shields.io`. An image from anywhere else is
    /// chipped exactly as `strip` chips it — the reader still sees that an image
    /// was there and where it pointed.
    ///
    /// **Absent means every host**, which is what `proxy` did before this
    /// existed: adding a key must not silently change what a running instance
    /// serves. An operator who wants the narrow behaviour asks for it, and the
    /// asking is one line.
    ///
    /// Inert under `strip`, where nothing is fetched at all.
    #[serde(default)]
    pub remote_image_hosts: Vec<String>,
    /// `"strip"` (default) or `"proxy"`. There is no `"allow"`: the SPA's CSP is
    /// baked into the document at build time, so it would silently do nothing.
    #[serde(default = "default_remote_images")]
    pub remote_images: String,
    /// Cap on **one proxied image**, in bytes. Separate from `max_bytes`, which
    /// caps the stored *text*: a 256 KiB text cap and a 2 MiB image cap are not
    /// the same number for the same reason, and sharing one would make raising
    /// either a decision about the other (RFC 0007-bis §4.1).
    ///
    /// Inert under `remote_images = "strip"`, where nothing is fetched.
    #[serde(default = "default_image_max_bytes")]
    pub image_max_bytes: usize,
}

impl Default for ReadmeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            from_archive: true,
            max_bytes: default_readme_max_bytes(),
            remote_images: default_remote_images(),
            remote_image_hosts: Vec::new(),
            image_max_bytes: default_image_max_bytes(),
        }
    }
}

// ── The console's discovery read ──────────────────────────────────────────────

fn default_upstream_max_versions() -> usize {
    batlehub_core::services::DEFAULT_UPSTREAM_MAX_VERSIONS
}

fn default_upstream_negative_ttl_secs() -> u64 {
    batlehub_core::services::DEFAULT_UPSTREAM_NEGATIVE_TTL_SECS
}

/// Whether the console may ask upstream about a package this instance holds
/// nothing of, and how much of the answer it may show.
///
/// ```toml
/// [registries.upstream_detail]
/// enabled           = true    # the console may ask upstream about a package we hold nothing of
/// max_versions      = 300     # cap on upstream-only versions returned for one package
/// negative_ttl_secs = 300     # how long an upstream "no such package" is remembered
/// ```
///
/// **There is no TTL of its own.** The document lands in the existing metadata
/// cache, so it obeys the registry's `metadata_ttl_secs` and its `serve_stale`.
/// A second, independently clocked expiry for the same bytes is how two caches
/// come to disagree about one document.
///
/// Inert on a `local`-mode registry — there is no upstream to ask — and on the
/// path-addressed kinds, which have no package identity to ask about. Both are
/// warned about rather than rejected.
#[derive(Debug, Clone, Deserialize)]
pub struct UpstreamDetailConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Cap on the upstream-only versions one package's page is handed. Applied
    /// newest-first, and the response says it was applied: a silently shortened
    /// list is a lie about the registry.
    #[serde(default = "default_upstream_max_versions")]
    pub max_versions: usize,
    /// How long an upstream `404` is remembered, so a bad URL, a typo or a
    /// crawler cannot turn every reload into an upstream request. A connection
    /// failure is not a fact about the package and is never remembered.
    #[serde(default = "default_upstream_negative_ttl_secs")]
    pub negative_ttl_secs: u64,
}

impl Default for UpstreamDetailConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_versions: default_upstream_max_versions(),
            negative_ttl_secs: default_upstream_negative_ttl_secs(),
        }
    }
}

// ── Quota management ──────────────────────────────────────────────────────────

/// How to enforce quota violations.
#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum QuotaEnforcement {
    /// Reject the publish request with HTTP 429 when the quota is exceeded.
    #[default]
    Block,
    /// Allow the publish but include a warning header in the response.
    Warn,
}

/// Per-registry publish quotas for local/hybrid mode.
///
/// Example TOML:
/// ```toml
/// [registries.quota]
/// max_storage_bytes_per_user = 1_073_741_824   # 1 GiB
/// max_packages_per_user      = 100
/// warn_threshold_pct         = 80
/// enforcement                = "block"
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct QuotaConfig {
    /// Maximum cumulative bytes a single user may publish to this registry.
    pub max_storage_bytes_per_user: Option<u64>,
    /// Maximum number of distinct package versions a single user may publish.
    pub max_packages_per_user: Option<u32>,
    /// Emit a quota-warning response header when usage exceeds this percentage
    /// of the limit. Defaults to 80.
    #[serde(default = "default_warn_pct")]
    pub warn_threshold_pct: u8,
    /// Whether to hard-block or just warn on quota overrun.
    #[serde(default)]
    pub enforcement: QuotaEnforcement,
}

fn default_warn_pct() -> u8 {
    80
}

// ── Beta channel ──────────────────────────────────────────────────────────────

/// Per-registry beta-channel configuration (local/hybrid mode only).
///
/// When `enabled` is `true`, pre-release versions (semver versions with a
/// non-empty pre-release component, e.g. `1.0.0-beta.1`) are hidden from users
/// who are not registered as beta-channel members. Non-members receive 404 on
/// both index listings and artifact downloads for pre-release versions.
///
/// ```toml
/// [registries.beta_channel]
/// enabled = true
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BetaChannelConfig {
    /// Enable pre-release gating for this registry.
    #[serde(default)]
    pub enabled: bool,
}

// ── Cache policy ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CachePolicy {
    /// TTL for metadata (version lists, release info) in seconds.
    #[serde(default = "default_metadata_ttl")]
    pub metadata_ttl_secs: u64,
    /// When true (the default), serve stale metadata when upstream returns a transient
    /// error instead of propagating a 502. Allows cached artifacts to keep being served
    /// during upstream outages.
    #[serde(default = "default_serve_stale")]
    pub serve_stale: bool,
    /// Evict artifacts older than this many seconds. `null` means never expire by age.
    #[serde(default)]
    pub artifact_ttl_secs: Option<u64>,
    /// Evict artifacts not accessed for this many days. `null` means never expire by idle time.
    #[serde(default)]
    pub idle_days: Option<u64>,
    /// Storage size cap in bytes. When exceeded, the least-recently-used artifacts are evicted
    /// until usage falls below this threshold. `null` means no size cap.
    #[serde(default)]
    pub max_size_bytes: Option<u64>,
    /// Keep only the N most-recently-cached versions per (registry, package). Older versions
    /// are evicted when a new one is stored. `null` means keep all versions.
    #[serde(default)]
    pub keep_latest_n: Option<usize>,
    /// Packages to pre-fetch on startup and via the `/warm` admin endpoint.
    /// Each entry is either a bare package name (`"lodash"`) or a pinned version
    /// (`"lodash@4.17.21"`). Bare names warm the latest `warm_latest_n` versions.
    #[serde(default)]
    pub warm_packages: Vec<String>,
    /// Upstream artifact paths to pre-fetch, for path-addressed registries
    /// (`deb`/`rpm`/`jetbrains`) that have no per-package version model. Each entry
    /// is the upstream-relative path, e.g. `"idea/idea-2026.1.3.tar.gz"` for a
    /// JetBrains registry or `"dists/stable/Release"` for a Deb registry. Warmed on
    /// startup and via the `/warm` admin endpoint (`paths`).
    #[serde(default)]
    pub warm_paths: Vec<String>,
    /// The platforms `warm_packages` warms, for the two kinds whose artifact
    /// is one file per platform: `sdkman` (`linuxx64`, `darwinarm64`, …) and
    /// `nodedist` (`linux-x64`, `darwin-arm64`, …). Empty — the default —
    /// means the platform this server runs on; guessing every platform would
    /// fetch 1.6 GB of JDK to satisfy a one-line `.sdkmanrc` (RFC 0010 §6.9).
    /// Rejected on any other kind.
    #[serde(default)]
    pub warm_platforms: Vec<String>,
    /// Number of most-recent versions to pre-warm per package (default: 1 = latest only).
    #[serde(default = "default_warm_latest_n")]
    pub warm_latest_n: usize,
    /// Maximum number of concurrent artifact downloads during a warming run (default: 2).
    #[serde(default = "default_warm_concurrency")]
    pub warm_concurrency: usize,
}

fn default_metadata_ttl() -> u64 {
    300
}

fn default_serve_stale() -> bool {
    true
}

fn default_warm_latest_n() -> usize {
    1
}

fn default_warm_concurrency() -> usize {
    2
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            metadata_ttl_secs: default_metadata_ttl(),
            serve_stale: true,
            artifact_ttl_secs: None,
            idle_days: None,
            max_size_bytes: None,
            keep_latest_n: None,
            warm_packages: vec![],
            warm_paths: vec![],
            warm_platforms: vec![],
            warm_latest_n: default_warm_latest_n(),
            warm_concurrency: default_warm_concurrency(),
        }
    }
}

/// Re-exported from `batlehub_core`: the value lives on `HotConfig`, which is
/// core's, so the type has to be too (RFC 0031 §6.3).
pub use batlehub_core::services::galaxy::GalaxyRoleMode;
