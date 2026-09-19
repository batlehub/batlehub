//! The upstream response shapes this client reads (RFC 0031 §5.1).
//!
//! Only the fields that decide something are typed. Everything else travels as
//! the `serde_json::Value` the handler relays, because the invariant of §5.2 is
//! that the **only** fields this instance writes are the ones no checksum
//! covers: a typed round-trip of the whole version document would quietly drop
//! any field a future galaxy_ng adds.

use serde::Deserialize;

/// `GET {api_root}` — what `g_connect` reads, and refuses to act without.
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub available_versions: std::collections::BTreeMap<String, String>,
}

impl Discovery {
    /// The path segment this upstream serves an API version under, relative to
    /// the API root — `v3/` on galaxy_ng, a longer path on a standalone
    /// pulp_ansible.
    ///
    /// Read from the document rather than hardcoded, exactly as `g_connect`
    /// does, because the two upstreams do not agree.
    pub fn path_for(&self, version: &str) -> Option<&str> {
        self.available_versions.get(version).map(String::as_str)
    }
}

/// The `artifact` object of a version document: what the client checksums the
/// bytes against.
#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactInfo {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// `GET {v3}/collections/{ns}/{name}/versions/{version}/`.
#[derive(Debug, Clone, Deserialize)]
pub struct VersionDetail {
    pub version: String,
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub artifact: Option<ArtifactInfo>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub requires_ansible: Option<String>,
}

/// One entry of a v1 role search — `lookup_role_by_name` takes `results[0]`.
#[derive(Debug, Clone, Deserialize)]
pub struct RoleSummary {
    pub id: serde_json::Value,
    #[serde(default)]
    pub github_user: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `GET {v1}/roles/?owner__username=&name=` and `GET {v1}/roles/{id}/`.
#[derive(Debug, Clone, Deserialize)]
pub struct RoleSearch {
    #[serde(default)]
    pub results: Vec<RoleSummary>,
}
