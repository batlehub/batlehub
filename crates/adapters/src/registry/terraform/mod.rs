use crate::registry::encode_path;
use serde::Deserialize;
use tracing as log;

use batlehub_core::{entities::PackageId, error::CoreError, ports::UpstreamPackage};

use super::http_client::{basic_auth_get, new_http_client, percent_encode, UpstreamHttpOptions};

mod client;
mod models;
mod modules;
mod providers;

/// Terraform provider and module registry proxy client.
///
/// Implements the Terraform Registry Protocol v1:
/// - Providers: <https://developer.hashicorp.com/terraform/internals/provider-registry-protocol>
/// - Modules:   <https://developer.hashicorp.com/terraform/internals/module-registry-protocol>
///
/// Default upstream: `https://registry.terraform.io`
///
/// `PackageId.name`: `"providers/{ns}/{type}"` or `"modules/{ns}/{name}/{provider}"`.
/// `PackageId.version`: `"versions"` for listing or a semver string.
/// `PackageId.artifact`: `None` (listing), `"{os}/{arch}"` (provider download), `"download"` (module).
pub struct TerraformRegistryClient {
    pub(super) http: reqwest::Client,
    pub(super) base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
    /// Resolved search base URL. `None` = disabled; `Some(url)` = use this.
    pub(super) search_base: Option<String>,
}

impl TerraformRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(10), opts)?;
        let base_url = base_url.into();

        // Terraform search API endpoints use /v1/modules/search and /v1/providers/{ns}.
        // Strip any trailing /v1 component from the base URL so we don't double it up
        // (some configs set upstreams = ["https://registry.terraform.io/v1"]).
        let search_root = base_url
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .trim_end_matches('/');
        let search_base = match opts.search_url.as_deref() {
            Some("") => None,
            Some(url) => Some(url.trim_end_matches('/').to_owned()),
            None => Some(search_root.to_owned()),
        };

        Ok(Self {
            http,
            base_url,
            basic_auth: opts.basic_auth.clone(),
            search_base,
        })
    }

    /// Fetch `url` and decode the JSON body as `T`. Returns `None` on network
    /// error, non-2xx status, or deserialization failure, logging a warning.
    pub(super) async fn fetch_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        label: &str,
    ) -> Option<T> {
        let res = match self.get(url).send().await {
            Err(e) => {
                log::warn!(%url, error = %e, "{label}: send failed");
                return None;
            }
            Ok(r) => r,
        };
        let status = res.status();
        if !status.is_success() {
            log::warn!(%url, %status, "{label}: bad status");
            return None;
        }
        match res.json::<T>().await {
            Ok(body) => Some(body),
            Err(e) => {
                log::warn!(error = %e, "{label}: json parse failed");
                None
            }
        }
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// Build the upstream URL for the given `PackageId`.
    pub(super) fn artifact_url(&self, pkg: &PackageId) -> Result<String, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let is_provider = pkg.name.starts_with("providers/");
        let is_module = pkg.name.starts_with("modules/");

        if !is_provider && !is_module {
            return Err(CoreError::Registry(format!(
                "terraform: invalid package name '{}': must start with 'providers/' or 'modules/'",
                pkg.name
            )));
        }

        if pkg.version == "versions" {
            return Ok(format!("{base}/v1/{}/versions", encode_path(&pkg.name)));
        }

        if is_provider {
            match pkg.artifact.as_deref() {
                None => {
                    // Provider version metadata (not a standard endpoint; fall back to versions)
                    Ok(format!("{base}/v1/{}/versions", encode_path(&pkg.name)))
                }
                // The checksum manifest and its signature are named *inside*
                // the download document rather than addressed by a path, so the
                // URL here is that document and `fetch_artifact` follows the
                // field (RFC 0009 §12.8). Terraform verifies the archive
                // against these, so an air-gapped install needs them proxied
                // too — a gated archive whose checksums come from the internet
                // is not an offline install.
                Some("shasums") | Some("shasums.sig") => Ok(format!(
                    "{base}/v1/{}/{}/download/linux/amd64",
                    encode_path(&pkg.name),
                    encode_path(&pkg.version)
                )),
                Some(platform) => {
                    // platform = "linux/amd64"
                    let (os, arch) = platform.split_once('/').ok_or_else(|| {
                        CoreError::Registry(format!(
                            "terraform: invalid provider platform '{platform}': expected 'os/arch'"
                        ))
                    })?;
                    Ok(format!(
                        "{base}/v1/{}/{}/download/{}/{}",
                        encode_path(&pkg.name),
                        encode_path(&pkg.version),
                        encode_path(os),
                        encode_path(arch)
                    ))
                }
            }
        } else {
            // module
            match pkg.artifact.as_deref() {
                Some("download") | None => Ok(format!(
                    "{base}/v1/{}/{}/download",
                    encode_path(&pkg.name),
                    encode_path(&pkg.version)
                )),
                Some(other) => Err(CoreError::Registry(format!(
                    "terraform: unknown module artifact '{other}'"
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests;
