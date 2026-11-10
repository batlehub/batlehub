use batlehub_core::{entities::PackageId, error::CoreError};

use super::http_client::{basic_auth_get, new_http_client, UpstreamHttpOptions};
use crate::registry::encode_path;

mod client;
mod models;

#[cfg(feature = "local-registry")]
pub use client::parse_gem_bytes;
pub use client::split_gem_stem;
pub use models::{GemDependency, GemMetadata};

/// RubyGems registry proxy client.
///
/// Implements the RubyGems REST API v1:
/// <https://guides.rubygems.org/rubygems-org-api/>
///
/// Default upstream: `https://rubygems.org`
///
/// `PackageId` conventions:
/// - `name`: gem name (e.g. `"rails"`) or `"_index"` for index files
/// - `version`:
///   - `"info"` → `/api/v1/gems/{name}.json`
///   - `"versions"` → `/api/v1/versions/{name}.json`
///   - `"specs"` → `/specs.4.8.gz`
///   - `"latest_specs"` → `/latest_specs.4.8.gz`
///   - `"prerelease_specs"` → `/prerelease_specs.4.8.gz`
///   - a semver string (with `artifact` set) → versioned gem resource
/// - `artifact`:
///   - `Some("gem")` → `/gems/{name}-{version}.gem`
///   - `Some("gemspec")` → `/quick/Marshal.4.8/{name}-{version}.gemspec.rz`
///   - `None` → REST endpoint determined by `version` field
pub struct RubyGemsRegistryClient {
    http: reqwest::Client,
    pub(super) base_url: String,
    basic_auth: Option<(String, String)>,
}

impl RubyGemsRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(None, opts)?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            basic_auth: opts.basic_auth.clone(),
        })
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    pub(super) fn artifact_url(&self, pkg: &PackageId) -> Result<String, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let name = &encode_path(&pkg.name);
        let version = &pkg.version;

        if name == "_index" {
            return match version.as_str() {
                "specs" => Ok(format!("{base}/specs.4.8.gz")),
                "latest_specs" => Ok(format!("{base}/latest_specs.4.8.gz")),
                "prerelease_specs" => Ok(format!("{base}/prerelease_specs.4.8.gz")),
                other => Err(CoreError::Registry(format!(
                    "rubygems: unknown index variant '{other}'"
                ))),
            };
        }

        match pkg.artifact.as_deref() {
            Some("gem") => Ok(format!("{base}/gems/{name}-{}.gem", encode_path(version))),
            Some("gemspec") => Ok(format!(
                "{base}/quick/Marshal.4.8/{name}-{}.gemspec.rz",
                encode_path(version)
            )),
            None => match version.as_str() {
                "versions" => Ok(format!("{base}/api/v1/versions/{name}.json")),
                _ => Ok(format!("{base}/api/v1/gems/{name}.json")),
            },
            Some(other) => Err(CoreError::Registry(format!(
                "rubygems: unknown artifact type '{other}'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
