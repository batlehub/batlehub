use serde::Deserialize;

use batlehub_core::{entities::PackageId, error::CoreError};

use super::http_client::{basic_auth_get, new_http_client, UpstreamHttpOptions};

mod client;
mod models;

#[cfg(feature = "local-registry")]
pub use client::parse_conda_metadata;
pub use models::CondaPackageInfo;

/// Conda channel proxy client.
///
/// Proxies a single conda channel (e.g. `conda-forge`) across all platforms.
///
/// Default upstream: `https://conda.anaconda.org`
///
/// `PackageId` conventions (repodata):
/// - `name`: `"repodata"` for the channel index, or the package filename stem
/// - `version`: platform string (e.g. `"linux-64"`, `"noarch"`)
/// - `artifact`: `None` for repodata, `Some("<filename>")` for a specific package
///
/// `list_versions` is implemented by fetching `repodata.json` for each of the
/// `list_platforms` (default: `noarch` + the four major binary platforms) and
/// collecting every distinct version string for the named package.
pub struct CondaRegistryClient {
    pub(super) http: reqwest::Client,
    pub(super) base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
    /// Platforms queried when `list_versions` is called.
    /// Defaults to the five most common platforms.
    pub(super) list_platforms: Vec<String>,
}

impl CondaRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(None, opts)?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            basic_auth: opts.basic_auth.clone(),
            list_platforms: default_list_platforms(),
        })
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// `HEAD` of the same URL, with the same credential.
    pub(super) fn head(&self, url: &str) -> reqwest::RequestBuilder {
        let req = self.http.head(url);
        match &self.basic_auth {
            Some((user, pass)) => req.basic_auth(user, Some(pass)),
            None => req,
        }
    }

    /// The URL of one of the channel's index documents, and its filename.
    ///
    /// `None` for a kind this channel has no index file for — the parsed path
    /// answers those. Shared by the byte path and the probe so the two cannot
    /// come to disagree about where an index lives.
    pub(super) fn index_url(
        &self,
        package: &str,
        kind: batlehub_core::ports::DocumentKind,
    ) -> Option<(String, &'static str)> {
        use batlehub_core::ports::DocumentKind;
        let base = self.base_url.trim_end_matches('/');
        match kind {
            DocumentKind::Versions => {
                Some((format!("{base}/{package}/repodata.json"), "repodata.json"))
            }
            DocumentKind::REPODATA_SHARDS => Some((
                format!("{base}/{package}/repodata_shards.msgpack"),
                "repodata_shards.msgpack",
            )),
            DocumentKind::CURRENT_REPODATA => Some((
                format!("{base}/{package}/current_repodata.json"),
                "current_repodata.json",
            )),
            DocumentKind::CHANNELDATA => {
                Some((format!("{base}/channeldata.json"), "channeldata.json"))
            }
            _ => None,
        }
    }

    pub(super) fn artifact_url(&self, pkg: &PackageId) -> String {
        let base = self.base_url.trim_end_matches('/');
        let (platform, artifact) = platform_and_file(pkg);

        match artifact {
            None | Some("repodata.json") => {
                format!("{base}/{platform}/repodata.json")
            }
            Some("current_repodata.json") => {
                format!("{base}/{platform}/current_repodata.json")
            }
            Some(filename) => {
                format!("{base}/{platform}/{filename}")
            }
        }
    }
}

/// Where a coordinate says which subdir it is in.
///
/// The proxy route files a package under its *name and version* — the
/// coordinate every rule reads, and the one a block is placed on — and
/// carries the subdir in the artifact selector as `{platform}/{filename}`.
/// A coordinate whose selector has no `/` is the older shape, `version` =
/// platform, which the listing routes still use (`repodata` at
/// `{platform}`); it is read as before. Found by RFC 0008-bis's suite: a
/// proxied conda download resolved its repodata under the *version* as the
/// platform and never worked.
pub(super) fn platform_and_file(pkg: &PackageId) -> (&str, Option<&str>) {
    match pkg.artifact.as_deref().and_then(|a| a.split_once('/')) {
        Some((platform, filename)) => (platform, Some(filename)),
        None => (pkg.version.as_str(), pkg.artifact.as_deref()),
    }
}

/// The five platforms queried by `list_versions` to synthesise a version list
/// from `repodata.json`.  `noarch` covers pure-Python and architecture-neutral
/// packages and is tried first because it is the smallest repodata file.
fn default_list_platforms() -> Vec<String> {
    ["noarch", "linux-64", "osx-64", "osx-arm64", "win-64"]
        .map(String::from)
        .to_vec()
}

#[cfg(test)]
mod tests;
