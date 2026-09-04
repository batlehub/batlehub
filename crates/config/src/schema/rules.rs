use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::registry::default_true;

// ── RBAC ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RbacConfig {
    #[serde(default)]
    pub anonymous: Vec<String>,
    #[serde(default)]
    pub user: Vec<String>,
    #[serde(default)]
    pub admin: Vec<String>,
    /// Dynamic groups from external identity providers (e.g. Authentik).
    /// Maps group name → list of permitted resource types for this registry.
    #[serde(default)]
    pub groups: HashMap<String, Vec<String>>,
    /// Controls which roles can search/browse this registry in the package explorer.
    /// When absent, defaults to allowing explore for any role that has proxy access.
    #[serde(default)]
    pub explore: ExploreRbacConfig,
}

/// Per-registry explore/search permissions.
///
/// Example TOML:
/// ```toml
/// [registries.rbac.explore]
/// anonymous = false   # anonymous users cannot search
/// user = false        # regular users cannot search (proxy-only)
/// admin = true        # admins can browse
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct ExploreRbacConfig {
    #[serde(default = "default_true")]
    pub anonymous: bool,
    #[serde(default = "default_true")]
    pub user: bool,
    #[serde(default = "default_true")]
    pub admin: bool,
}

impl Default for ExploreRbacConfig {
    fn default() -> Self {
        Self {
            anonymous: true,
            user: true,
            admin: true,
        }
    }
}

// ── Rules ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleConfig {
    ReleaseAgeGate(ReleaseAgeGateConfig),
    RequireSignedRelease(RequireSignedReleaseConfig),
    DenyLatest(DenyLatestConfig),
    CveGate(CveGateConfig),
    LicenseGate(LicenseGateConfig),
    VersionGate(VersionGateConfig),
    TrustedPublisher(TrustedPublisherConfig),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReleaseAgeGateConfig {
    /// Minimum age in seconds before a release is downloadable.
    #[serde(default = "default_min_age")]
    pub min_age_secs: u64,
    /// Roles that may bypass the age gate (e.g. `["admin"]`).
    #[serde(default)]
    pub bypass_roles: Vec<String>,
    /// When `true`, deny requests for packages whose upstream does not provide
    /// a publish timestamp (instead of the default behaviour of skipping the
    /// check and allowing the download).
    ///
    /// Useful for registries — such as conda — where the timestamp field is
    /// optional: setting this to `true` forces every package to carry a
    /// verifiable age before it can be downloaded.
    ///
    /// An `Option` so that *absence* is observable: on the toolchain kinds
    /// (`nodedist`; `sdkman` when it lands) this one field is most of the rule,
    /// and config validation refuses to let it default there (RFC 0010 §4.5,
    /// §6.7). Everywhere else an unset field means `false`, exactly as before —
    /// read it through [`Self::deny_missing_timestamp`].
    #[serde(default)]
    pub deny_missing_timestamp: Option<bool>,
}

impl ReleaseAgeGateConfig {
    /// The effective setting: `false` unless the operator wrote `true`.
    pub fn deny_missing_timestamp(&self) -> bool {
        self.deny_missing_timestamp.unwrap_or(false)
    }
}

fn default_min_age() -> u64 {
    3600
}

/// Gate downloads on the upstream's best-effort signature signal
/// (`PackageMetadata::is_signed`) — e.g. a `.asc`/`.sig` release asset on
/// GitHub/Forgejo, or a signature blob in an OpenVSX/VS Code extension.
/// This is *not* cryptographic verification: registries with no such signal
/// for their ecosystem (npm, PyPI, crates.io, Maven, …) report `is_signed =
/// None`, which this rule allows through by default (see
/// `deny_missing_signature`).
#[derive(Debug, Serialize, Deserialize)]
pub struct RequireSignedReleaseConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Roles that may bypass the signature requirement (e.g. `["admin"]`).
    #[serde(default)]
    pub bypass_roles: Vec<String>,
    /// When `true`, deny releases from registries that report no signature
    /// signal at all (`is_signed == None`), instead of skipping the check.
    #[serde(default)]
    pub deny_missing_signature: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DenyLatestConfig {
    /// Roles that may bypass the restriction (e.g. `["admin"]`).
    #[serde(default)]
    pub bypass_roles: Vec<String>,
}

/// Gate downloads of package versions with known vulnerabilities, as discovered
/// by the periodic SBOM re-scan (`[vulnerability_scan]`).
///
/// ```toml
/// [[registries.rules]]
/// kind = "cve_gate"
/// min_severity = "high"        # unknown | low | medium | high | critical
/// block = true                 # false (default) = warn-only, surfaced in UI but never blocked
/// bypass_roles = ["admin"]
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct CveGateConfig {
    /// Lowest severity that triggers the gate. One of
    /// `unknown | low | medium | high | critical`. Defaults to `high`.
    #[serde(default = "default_cve_min_severity")]
    pub min_severity: String,
    /// When `true`, deny downloads of affected versions; when `false` (the
    /// default) the finding is only surfaced in the UI and never blocks.
    #[serde(default)]
    pub block: bool,
    /// Roles that may bypass the gate even when `block` is `true` (e.g. `["admin"]`).
    #[serde(default)]
    pub bypass_roles: Vec<String>,
}

fn default_cve_min_severity() -> String {
    "high".to_owned()
}

/// Gate downloads by the licence the package's own manifest declares, as read
/// by the SBOM extractor at cache/publish time (RFC 0004-bis §13.1).
///
/// ```toml
/// [[registries.rules]]
/// kind = "license_gate"
/// allow = ["MIT", "Apache-2.0", "BSD-3-Clause"]  # optional allowlist
/// deny  = ["AGPL-3.0", "SSPL-1.0"]               # always refused
/// allow_unknown = true                           # default
/// block = false                                  # default = warn-only
/// bypass_roles = ["admin"]
/// ```
///
/// **The licence is read from the archive, so it is not known until the
/// artifact has been fetched once.** The first request for an uncached package
/// is therefore governed by `allow_unknown`, not by `allow`/`deny`. This is the
/// same trade `integrity.require_metadata` makes for checksums; see §13.1 for
/// why it cannot be designed away.
///
/// Comparison is case-insensitive and ignores surrounding whitespace, because
/// manifests are written by hand. It is otherwise literal: `allow = ["MIT"]`
/// does not match `MIT OR Apache-2.0`, since a compound expression is a
/// different declaration and silently accepting it would let a package opt out
/// of the gate by adding an alternative.
#[derive(Debug, Serialize, Deserialize)]
pub struct LicenseGateConfig {
    /// Approved licences. When non-empty, a declared licence matching none of
    /// these is refused.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Refused licences, checked before `allow` so a deny entry always wins.
    #[serde(default)]
    pub deny: Vec<String>,
    /// How to treat a package whose licence is unknown — no manifest parser for
    /// the registry type, nothing declared, or not fetched yet. `true` (the
    /// default) lets it through; `false` refuses it.
    #[serde(default = "default_true")]
    pub allow_unknown: bool,
    /// When `true`, deny; when `false` (the default) the rule never blocks and
    /// the licence is only surfaced in the UI.
    #[serde(default)]
    pub block: bool,
    /// Roles that may bypass the gate even when `block` is `true`.
    #[serde(default)]
    pub bypass_roles: Vec<String>,
}

/// Gate downloads by version: an optional approved-version allowlist plus a
/// blocklist of specific versions with known issues. Each entry is an exact
/// version string or a semver range (e.g. `">=1.2.0, <2.0.0"`).
///
/// ```toml
/// [[registries.rules]]
/// kind = "version_gate"
/// allow = [">=1.2.0, <2.0.0"]   # optional: when set, only matching versions are served
/// block = ["1.4.7", "1.5.0"]    # specific versions with known issues
/// bypass_roles = ["admin"]
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct VersionGateConfig {
    /// Approved-version allowlist. When non-empty, a version that matches none of
    /// these entries is rejected.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Blocklist of specific versions (or ranges) with known issues.
    #[serde(default)]
    pub block: Vec<String>,
    /// Roles that may bypass the gate (e.g. `["admin"]`).
    #[serde(default)]
    pub bypass_roles: Vec<String>,
}

/// Restrict downloads to packages published by an allowed org/user/scope.
///
/// The publisher is derived from already-resolved metadata, with no extra
/// upstream calls — supported for `github`/`gitlab`/`forgejo` (top-level
/// owner/group), `npm` (scope, or the publishing user when unscoped), and
/// `openvsx`/`vscode-marketplace` (the extension's publisher segment).
/// **Not yet supported for `cargo`** (crates.io ownership isn't in the sparse
/// index and would need a separate API call) — configuring this rule on an
/// unsupported registry denies every request. Matching is case-insensitive.
///
/// ```toml
/// [[registries.rules]]
/// kind = "trusted_publisher"
/// allow = ["my-org", "trusted-user"]
/// bypass_roles = ["admin"]
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct TrustedPublisherConfig {
    /// Allowed publisher identifiers (org/user/scope). When non-empty, a
    /// package whose derived publisher matches none of these is rejected.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Roles that may bypass the gate (e.g. `["admin"]`).
    #[serde(default)]
    pub bypass_roles: Vec<String>,
}
