use super::http_client::{
    apply_upstream_options, basic_auth_get, to_registry_error, UpstreamHttpOptions,
};
use super::models::PackagistV2Response;
use crate::registry::encode_path;
use batlehub_core::error::CoreError;

/// Composer / Packagist registry proxy client.
///
/// Implements the Packagist v2 API:
/// <https://packagist.org/apidoc>
///
/// Default upstream: `https://repo.packagist.org`
///
/// `PackageId` conventions:
/// - `name`: `vendor/package` (e.g. `symfony/console`)
/// - `version`: semver string (e.g. `v7.2.0`) or `dev-*` branch alias
/// - `artifact`:
///   - `Some("dist")`     → stream the dist ZIP for that version
///   - `Some("p2")`       → fetch `p2/{vendor}/{package}.json` bytes (all versions)
///   - `Some("p2~dev")`   → fetch `p2/{vendor}/{package}~dev.json` bytes (dev variants)
///   - `None`             → version metadata only
pub struct ComposerRegistryClient {
    pub(super) http: reqwest::Client,
    pub(super) base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
    /// Resolved search base URL. `None` = disabled; `Some(url)` = use this.
    pub(super) search_base: Option<String>,
}

impl ComposerRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let builder = reqwest::Client::builder().user_agent("Composer/2.0 batlehub/0.1");
        let http = apply_upstream_options(builder, opts)?;
        let base_url = base_url.into().trim_end_matches('/').to_owned();

        // Resolve the search base URL:
        //   - explicit empty string → disabled
        //   - explicit non-empty URL → use as-is
        //   - absent → derive from base_url: any *.packagist.org URL maps to packagist.org
        let search_base = match opts.search_url.as_deref() {
            Some("") => None,
            Some(url) => Some(url.trim_end_matches('/').to_owned()),
            None => {
                if base_url.contains("packagist.org") {
                    Some("https://packagist.org".to_owned())
                } else {
                    // Unknown private Composer repository — attempt the same host
                    Some(base_url.clone())
                }
            }
        };

        Ok(Self {
            http,
            base_url,
            basic_auth: opts.basic_auth.clone(),
            search_base,
        })
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    /// Send a GET to the given p2 URL and check the status code.
    /// Returns the raw response on success; maps 404 to `CoreError::NotFound`.
    pub(super) async fn send_p2_request(
        &self,
        url: &str,
        package: &str,
    ) -> Result<reqwest::Response, CoreError> {
        let resp = self
            .get(url)
            .send()
            .await
            .map_err(|e| CoreError::Registry(format!("composer p2 request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "composer package '{package}' not found"
            )));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "composer upstream returned {} for '{package}'",
                resp.status()
            )));
        }
        Ok(resp)
    }

    pub(super) async fn fetch_p2_response(
        &self,
        package: &str,
    ) -> Result<PackagistV2Response, CoreError> {
        let url = format!("{}/p2/{}.json", self.base_url, encode_path(package));
        let resp = self.send_p2_request(&url, package).await?;
        resp.json::<PackagistV2Response>()
            .await
            .map_err(|e| CoreError::Registry(format!("composer p2 parse error: {e}")))
    }

    /// Return the raw p2 JSON bytes for `package`, using `url_suffix` to select
    /// the exact endpoint (`""` for `.json`, `"~dev"` for `~dev.json`).
    pub(super) async fn fetch_p2_bytes(
        &self,
        package: &str,
        url_suffix: &str,
    ) -> Result<(bytes::Bytes, Option<String>), CoreError> {
        let url = format!(
            "{}/p2/{}{url_suffix}.json",
            self.base_url,
            encode_path(package)
        );
        let resp = self.send_p2_request(&url, package).await?;

        let cache_control = resp
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let body = resp.bytes().await.map_err(to_registry_error)?;

        Ok((body, cache_control))
    }
}
