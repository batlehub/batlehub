use batlehub_core::error::CoreError;
use serde::Deserialize;

use super::http_client::{
    apply_upstream_options, apply_upstream_tls, basic_auth_get, new_http_client,
    UpstreamHttpOptions,
};
use super::ssrf;

mod client;
mod models;

pub use client::{fetch_simple_page, normalize_name, rewrite_simple_page, SIMPLE_JSON_ACCEPT};

/// PyPI registry proxy client.
///
/// Implements the PyPI JSON API and Simple Repository API (PEP 503/691).
///
/// Default upstream: `https://pypi.org`
///
/// `PackageId` conventions:
/// - `name`: PEP 503-normalised package name (lower-case, `[-_.]` → `-`)
/// - `version`:
///   - a version string (e.g. `"2.28.0"`) → `GET /pypi/{name}/{version}/json`
///   - `"__all__"` → `GET /pypi/{name}/json` (all versions, for `list_versions`)
/// - `artifact`: filename of the specific distribution file
///   When `None`, `resolve_metadata` returns metadata without a specific artifact URL.
pub struct PypiRegistryClient {
    pub(super) http: reqwest::Client,
    /// Credentialed client for artifact downloads, with redirects DISABLED so the
    /// SSRF guard re-validates every hop. Carries the same auth default headers as
    /// `http`; the guard only attaches them while the URL stays on `base_url`.
    pub(super) dl_credentialed: reqwest::Client,
    /// Credential-free download client (no auth headers), redirects disabled.
    /// Used for any hop that leaves the index origin so configured credentials
    /// never leak off-origin.
    pub(super) dl_plain: reqwest::Client,
    pub(super) base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
}

impl PypiRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(None, opts)?;
        // Download clients follow redirects MANUALLY (via the SSRF guard), so their
        // own redirect policy must be disabled — otherwise reqwest would follow a
        // 30x itself, unchecked, before our per-hop validation runs.
        let no_redirect = || {
            reqwest::Client::builder()
                .user_agent("batlehub/0.1")
                .redirect(reqwest::redirect::Policy::none())
        };
        let dl_credentialed =
            apply_upstream_options(no_redirect(), opts).map_err(CoreError::Other)?;
        let dl_plain = apply_upstream_tls(super::ssrf::public_only_dns(no_redirect(), opts), opts)
            .map_err(CoreError::Other)?
            .build()
            .map_err(|e| CoreError::Other(e.into()))?;
        Ok(Self {
            http,
            dl_credentialed,
            dl_plain,
            base_url: base_url.into(),
            basic_auth: opts.basic_auth.clone(),
        })
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// Fetch an artifact download URL taken from the (untrusted) index JSON.
    ///
    /// PyPI legitimately serves files from a different origin than the index
    /// (pypi.org → files.pythonhosted.org), so same-origin cannot be required.
    /// But the URL — and any redirect it issues — is index-controlled, so we:
    /// - reject targets that resolve to private/reserved/loopback/link-local
    ///   addresses (SSRF, incl. `169.254.169.254`), on the initial URL and every
    ///   redirect hop, and
    /// - attach configured credentials ONLY while the URL stays on the index
    ///   origin, so they can never be exfiltrated to an off-origin (redirect) host.
    pub(super) async fn download(&self, url: &str) -> Result<reqwest::Response, CoreError> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| CoreError::Registry(format!("invalid pypi download URL '{url}': {e}")))?;
        ssrf::fetch_following_redirects(
            &self.dl_credentialed,
            &self.dl_plain,
            &self.basic_auth,
            &self.base_url,
            parsed,
        )
        .await
    }
}

#[cfg(test)]
mod tests;
