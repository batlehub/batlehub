//! `[[release_imports]]` — a forge release into the registry that serves it
//! (RFC 0021 §4.1).
//!
//! ```toml
//! [[release_imports]]
//! into          = "vsx-local"              # a local or hybrid registry
//! from          = "gh"                     # a configured forge registry
//! repo          = "batleforc/batlehub-vsx"
//! assets        = ["*.vsix"]               # globs; never empty
//! releases      = "latest"                 # "latest" | "all" | a tag
//! interval_secs = 3600                     # absent: import only when asked
//!
//! [release_imports.as]
//! user_id = "svc-release-import"
//! groups  = ["config:extension-publishers"]
//! ```
//!
//! Top-level rather than under `[[registries]]` because an import is a
//! *relationship* between two registries: hanging it off either one hides the
//! other, and the pair is the thing an operator reads.

use serde::Deserialize;

/// The most results one release listing is asked for. See
/// [`ReleaseImportConfig::releases`].
pub const LATEST: &str = "latest";
pub const ALL: &str = "all";

/// The floor under an import's polling interval, per **source registry**.
///
/// RFC 0014 §4.4 set the same five minutes under `[upstream_audit]` because
/// below it a sweep is an unintentional denial of service against a third
/// party's registry, from a config typo. The party protected here is the forge,
/// and a forge's rate limit is spent by the credential — which belongs to the
/// source registry, not to any one import. So the floor is checked against the
/// combined rate of every import sharing a `from` (RFC 0021 §11 q6).
pub const MIN_IMPORT_INTERVAL_SECS: u64 = 300;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ReleaseImportConfig {
    /// The registry published into. Local or hybrid mode: a publish into a
    /// proxy-mode registry is a `404` at request time, and failing at load is
    /// the same answer a day earlier.
    pub into: String,
    /// The registry fetched from — `github`, `gitlab` or `forgejo`. A
    /// configured registry and never a URL, so the fetch inherits its
    /// credential, its allowlist and its SSRF guard (RFC 0021 §7).
    pub from: String,
    /// `owner/repo` on the source forge.
    pub repo: String,
    /// Asset name globs. Never empty: a release routinely carries checksums,
    /// signatures and source tarballs beside the artifact, and "everything" is
    /// never what an operator means.
    pub assets: Vec<String>,
    /// `"latest"` (the newest release that is neither a draft nor a
    /// pre-release), `"all"`, or one tag.
    #[serde(default = "default_releases")]
    pub releases: String,
    /// How often this import runs on its own. Absent means it runs only when
    /// an operator asks.
    #[serde(default)]
    pub interval_secs: Option<u64>,
    /// Who the publish is, and answers as.
    #[serde(rename = "as")]
    pub principal: ImportPrincipalConfig,
}

/// The identity an import publishes under (RFC 0021 §4.3).
///
/// Deliberately smaller than `Identity`: there is no `role`, because the answer
/// is always `user` and an admin would skip the namespace check outright; and
/// no provider, because nothing authenticated this principal — the server
/// constructs it for its own action.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ImportPrincipalConfig {
    /// What grants name: `user:<user_id>`. Also what quota is charged to and
    /// what the audit row records.
    pub user_id: String,
    /// Groups, each written `config:<name>`. The prefix is reserved so a config
    /// file cannot mint a group string an identity provider owns.
    #[serde(default)]
    pub groups: Vec<String>,
}

fn default_releases() -> String {
    LATEST.to_owned()
}

impl ReleaseImportConfig {
    /// How many times an hour this import polls, or `0` when it does not.
    pub fn polls_per_hour(&self) -> f64 {
        match self.interval_secs {
            Some(secs) if secs > 0 => 3600.0 / secs as f64,
            _ => 0.0,
        }
    }
}
