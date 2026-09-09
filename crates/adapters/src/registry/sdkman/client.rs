//! The `RegistryClient` for SDKMAN.
//!
//! Coordinates (RFC 0010 §4.3): the candidate is the package, the version
//! is SDKMAN's identifier (`21.0.5-tem`, `3.9.9`), and the platform — one
//! of eight closed values — is the artifact:
//!
//! ```text
//! PackageId { name: "java", version: "21.0.5-tem", artifact: Some("linuxx64") }
//! → GET {broker}/download/java/21.0.5-tem/linuxx64   (302 → CDN, followed here)
//! ```
//!
//! The listing documents are per candidate *and platform*, with no version,
//! so their platform travels inside the package string (`java/linuxx64`) —
//! see `batlehub_core::services::sdkman` for the coordinate helpers.
//!
//! `published_at` is always `None`: SDKMAN publishes no dates. An age gate
//! on this kind therefore decides everything with `deny_missing_timestamp`,
//! which config validation makes the operator state (§6.7).
//!
//! **Response headers are not forwarded.** RFC 0010 §4.4 asked for the
//! `X-Sdkman-*` family to cross back; §13.2 records why it does not yet:
//! the proxy's artifact path has no header channel from a cached artifact,
//! and the live probe found the CLI's own reader of those headers
//! (`grep '^X-Sdkman'`, case-sensitive) matching nothing over the HTTP/2
//! lower-cased names the API sends today.

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::sdkman::{candidate_of, parse_platform, split_listing_package, versions_in_csv},
};

use super::models::{broker_download_url, parse_validate_answer, relayed_path_is_allowed};
use crate::registry::http_client::{
    basic_auth_get, cache_control, fetch_text_document, new_http_client, no_redirect_client_pair,
    to_registry_error, UpstreamHttpOptions,
};
use crate::registry::ssrf;

/// Every SDKMAN document is plain text, and upstream says so.
const TEXT: &str = "text/plain; charset=utf-8";

pub struct SdkmanRegistryClient {
    /// The candidates API: small text documents, redirects followed by
    /// reqwest itself (the API never redirects off-host).
    http: reqwest::Client,
    /// The broker download, with reqwest's own redirect following disabled so
    /// every hop of the `302` chain goes through the SSRF guard.
    dl_credentialed: reqwest::Client,
    dl_plain: reqwest::Client,
    api_base: String,
    broker_base: String,
    basic_auth: Option<(String, String)>,
    /// The two configured origins credentials may be sent to. The CDN a
    /// redirect lands on is neither, and gets the credential-free client.
    trusted_origins: Vec<String>,
}

impl SdkmanRegistryClient {
    pub fn new(
        api_base: impl Into<String>,
        broker_base: impl Into<String>,
        opts: &UpstreamHttpOptions,
    ) -> Result<Self, CoreError> {
        let http = new_http_client(Some(5), opts)?;
        let (dl_credentialed, dl_plain) = no_redirect_client_pair(opts)?;
        let api_base = api_base.into().trim_end_matches('/').to_owned();
        let broker_base = broker_base.into().trim_end_matches('/').to_owned();
        let trusted_origins = vec![api_base.clone(), broker_base.clone()];
        Ok(Self {
            http,
            dl_credentialed,
            dl_plain,
            api_base,
            broker_base,
            basic_auth: opts.basic_auth.clone(),
            trusted_origins,
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    fn api(&self, path: &str) -> String {
        format!("{}/{path}", self.api_base)
    }

    /// The platform an artifact coordinate names, refused before any request
    /// is made when it is not one of SDKMAN's eight.
    fn platform_of(pkg: &PackageId) -> Result<&'static str, CoreError> {
        let Some(raw) = pkg.artifact.as_deref() else {
            return Err(CoreError::NotFound(format!(
                "an SDKMAN artifact is addressed by platform; name one for {} {}",
                pkg.name, pkg.version
            )));
        };
        parse_platform(raw).ok_or_else(|| {
            CoreError::InvalidInput(format!(
                "'{raw}' is not an SDKMAN platform (linuxx64, linuxarm64, darwinx64, \
                 darwinarm64, windowsx64, linuxx32, linuxarm32hf, exotic)"
            ))
        })
    }
}

#[async_trait]
impl RegistryClient for SdkmanRegistryClient {
    fn registry_type(&self) -> &str {
        "sdkman"
    }

    /// `candidates/validate/{c}/{v}/{plat}`: the cheapest existence check the
    /// protocol offers, and the same question the rule engine needs answered.
    /// `invalid` is [`CoreError::NotFound`].
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let platform = Self::platform_of(pkg)?;
        let url = self.api(&format!(
            "candidates/validate/{}/{}/{platform}",
            pkg.name, pkg.version
        ));
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CoreError::Registry(format!(
                "SDKMAN validate for {} {} on {platform} returned {status} from upstream",
                pkg.name, pkg.version
            )));
        }
        let body = resp.text().await.map_err(to_registry_error)?;
        match parse_validate_answer(&body) {
            Some(true) => {}
            Some(false) => {
                return Err(CoreError::NotFound(format!(
                    "{} {} is not a valid SDKMAN version on {platform}",
                    pkg.name, pkg.version
                )))
            }
            None => {
                return Err(CoreError::Registry(format!(
                    "SDKMAN validate for {} {} answered neither 'valid' nor 'invalid' \
                     ({} bytes); is '{}' the candidates API?",
                    pkg.name,
                    pkg.version,
                    body.len(),
                    self.api_base
                )))
            }
        }
        Ok(PackageMetadata {
            id: pkg.clone(),
            // SDKMAN publishes no dates (RFC 0010 §6.7).
            published_at: None,
            download_url: Some(broker_download_url(
                &self.broker_base,
                &pkg.name,
                &pkg.version,
                platform,
            )),
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({ "platform": platform }),
            cache_control: None,
        })
    }

    /// `{broker}/download/{c}/{v}/{plat}`, following the broker's `302` to
    /// the vendor's CDN server-side: every hop is checked against the SSRF
    /// guard and the operator's credentials stop at the configured origins.
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let platform = Self::platform_of(pkg)?;
        let url = broker_download_url(&self.broker_base, &pkg.name, &pkg.version, platform);
        tracing::debug!(url = %url, "fetching SDKMAN artifact through the broker");
        let start = reqwest::Url::parse(&url)
            .map_err(|e| CoreError::Registry(format!("invalid broker URL '{url}': {e}")))?;

        let response = ssrf::fetch_following_redirects_trusting(
            &self.dl_credentialed,
            &self.dl_plain,
            &self.basic_auth,
            &self.trusted_origins,
            start,
        )
        .await?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "SDKMAN has no {} {} for {platform}",
                pkg.name, pkg.version
            )));
        }
        let response = response.error_for_status().map_err(to_registry_error)?;
        let cache_control = cache_control(&response);
        let stream = response.bytes_stream().map_err(to_registry_error);
        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    /// The text documents, each addressed by what `package` carries:
    ///
    /// - `Versions` — `{candidate}/{platform}` → `versions/all`;
    /// - `SDKMAN_DEFAULT` — `{candidate}` → `candidates/default/{candidate}`;
    /// - `SDKMAN_VERSIONS_LIST` — `{candidate}/{platform}?current=…&installed=…`
    ///   → the rendered list, the query forwarded as the client sent it;
    /// - `RELAYED` — an API path from the allow-list, byte-exact.
    ///
    /// A bare candidate with no platform reads the default platform's list,
    /// which is what the console's discovery read and `list_versions` ask.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let coord = split_listing_package(package);
        match kind {
            DocumentKind::Versions => {
                let platform = platform_or_reject(coord.platform)?;
                let url = self.api(&format!(
                    "candidates/{}/{platform}/versions/all",
                    coord.candidate
                ));
                fetch_text_document(
                    self.get(&url),
                    &format!("SDKMAN versions for '{}' on {platform}", coord.candidate),
                    TEXT,
                )
                .await
            }
            DocumentKind::SDKMAN_DEFAULT => {
                let candidate = candidate_of(package);
                let url = self.api(&format!("candidates/default/{candidate}"));
                fetch_text_document(
                    self.get(&url),
                    &format!("SDKMAN default version for '{candidate}'"),
                    TEXT,
                )
                .await
            }
            DocumentKind::SDKMAN_VERSIONS_LIST => {
                let platform = platform_or_reject(coord.platform)?;
                // The API answers 400 without the two parameters, so an
                // absent query is sent as the empty pair the client sends.
                let query = coord.query.unwrap_or("current=&installed=");
                let url = self.api(&format!(
                    "candidates/{}/{platform}/versions/list?{query}",
                    coord.candidate
                ));
                fetch_text_document(
                    self.get(&url),
                    &format!(
                        "SDKMAN rendered version list for '{}' on {platform}",
                        coord.candidate
                    ),
                    TEXT,
                )
                .await
            }
            DocumentKind::RELAYED => {
                if !relayed_path_is_allowed(package) {
                    return Err(CoreError::InvalidInput(format!(
                        "'{package}' is not an SDKMAN API path this proxy relays"
                    )));
                }
                let url = self.api(package);
                fetch_text_document(self.get(&url), &format!("SDKMAN '{package}'"), TEXT).await
            }
            other => Err(CoreError::NotSupported(format!(
                "SDKMAN has no '{other}' listing document"
            ))),
        }
    }

    /// The identifiers of `versions/all`, in the order upstream lists them.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let doc = self
            .fetch_version_document(package, DocumentKind::Versions)
            .await?;
        Ok(doc.body.as_text().map(versions_in_csv).unwrap_or_default())
    }
}

/// A platform read out of a package string, refused before it becomes a
/// path segment when it is not one of the eight.
fn platform_or_reject(raw: &str) -> Result<&'static str, CoreError> {
    parse_platform(raw)
        .ok_or_else(|| CoreError::InvalidInput(format!("'{raw}' is not an SDKMAN platform")))
}
