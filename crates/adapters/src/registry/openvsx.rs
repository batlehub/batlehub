use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;
use serde::Deserialize;
use std::collections::HashMap;

use batlehub_core::{
    entities::{MetadataLinks, MetadataReadme, PackageId, PackageMetadata, ReadmeFormat},
    error::CoreError,
    ports::{FetchedArtifact, RegistryClient, UpstreamPackage},
};

use super::http_client::{
    basic_auth_get, cache_control, ensure_same_origin, fetch_linked_text, new_http_client,
    no_redirect_client_pair, percent_encode, to_registry_error, UpstreamHttpOptions,
};

/// OpenVSX registry client (open-vsx.org or compatible).
///
/// Supported `PackageId` conventions:
/// - `name` format: `"{publisher}.{extension}"` (e.g. `"ms-python.python"`)
/// - `version = "latest"` → current latest version
/// - `version = "1.2.3"`  → specific semver version
/// - `artifact = Some("vsix")` → stream the `.vsix` extension package
pub struct OpenVsxRegistryClient {
    http: reqwest::Client,
    /// The linked-README pair: redirects disabled so the SSRF guard validates
    /// every hop, credentials attached only while the chain stays on `base_url`
    /// (see [`fetch_linked_text`]).
    readme_credentialed: reqwest::Client,
    readme_plain: reqwest::Client,
    base_url: String,
    basic_auth: Option<(String, String)>,
}

impl OpenVsxRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(10), opts)?;
        let (readme_credentialed, readme_plain) = no_redirect_client_pair(opts)?;
        Ok(Self {
            http,
            readme_credentialed,
            readme_plain,
            base_url: base_url.into(),
            basic_auth: opts.basic_auth.clone(),
        })
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    fn parse_id(name: &str) -> Result<(&str, &str), CoreError> {
        name.split_once('.').ok_or_else(|| {
            CoreError::Registry(format!(
                "invalid OpenVSX extension id '{name}': expected '{{publisher}}.{{name}}'"
            ))
        })
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OpenVsxExtension {
    namespace: String,
    #[allow(dead_code)]
    name: String,
    version: String,
    timestamp: Option<String>,
    #[serde(default)]
    files: OpenVsxFiles,
    #[serde(rename = "allVersions", default)]
    all_versions: HashMap<String, String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    description: Option<String>,
    #[serde(rename = "downloadCount", default)]
    download_count: u64,
    verified: Option<bool>,
    /// The extension's own repository, as its `package.json` declared it — so
    /// usually a clone URL keeping its `.git`, which `MetadataLinks` turns back
    /// into the page it names.
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenVsxFiles {
    download: Option<String>,
    signature: Option<String>,
    /// The key the signature verifies under, one URL per signing key
    /// (`/api/-/public-key/{id}`); present only when the registry signs.
    #[serde(rename = "publicKey", default)]
    public_key: Option<String>,
    manifest: Option<String>,
    icon: Option<String>,
    /// A URL to the extension's README, not the text.
    ///
    /// Following it is an outbound request in its own right, so it is carried
    /// as a link and read in the detached introspection task rather than on the
    /// resolve path — same-origin checked against this registry's own base URL,
    /// so a compromised or misconfigured upstream cannot use it to point
    /// BatleHub at an internal host (RFC 0007 §7.4).
    readme: Option<String>,
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for OpenVsxRegistryClient {
    fn registry_type(&self) -> &str {
        "openvsx"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let (publisher, ext_name) = Self::parse_id(&pkg.name)?;
        let ext = self
            .fetch_extension(publisher, ext_name, &pkg.version)
            .await?;

        let published_at = ext
            .timestamp
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let download_url = if pkg.artifact.as_deref() == Some("vsix") {
            ext.files.download.clone()
        } else {
            None
        };

        let is_signed = Some(ext.files.signature.is_some());

        let extra = serde_json::json!({
            "resolved_version": ext.version,
            "namespace": ext.namespace,
            "display_name": ext.display_name,
            "description": ext.description,
            "download_count": ext.download_count,
            "verified": ext.verified,
            "manifest_url": ext.files.manifest,
            "icon_url": ext.files.icon,
            // RFC 0020 §4.2: relayed as assets of this registry when present.
            "signature_url": ext.files.signature,
            "public_key_url": ext.files.public_key,
            "readme": ext.files.readme.as_deref().map(|url| {
                // Markdown by protocol, whatever the upstream's `Content-Type`
                // says when the link is followed: an upstream must not be able
                // to choose which renderer path runs.
                MetadataReadme::linked(url, ReadmeFormat::Markdown)
            }),
            "all_versions_count": ext.all_versions.len(),
            "links": MetadataLinks::new(ext.repository.as_deref(), ext.homepage.as_deref()),
        });

        Ok(PackageMetadata {
            id: PackageId {
                version: ext.version,
                ..pkg.clone()
            },
            published_at,
            download_url,
            checksum: None,
            is_signed,
            extra,
            cache_control: None,
        })
    }

    /// Read the `files.readme` URL the packument linked to.
    ///
    /// Never called on the resolve path — the link travels on `extra` and this
    /// runs in the detached introspection task (RFC 0007 §5.1).
    async fn fetch_linked_readme(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Option<String>, CoreError> {
        // No widening: Open VSX serves its `files.readme` from the same origin
        // as the API, unlike the VS Code Marketplace's asset CDN — so a
        // configured credential does travel with this read (it is a request to
        // the registry it was configured for, and the anonymous read is the one
        // that gets rate-limited), and stops at the first redirect that leaves
        // that origin.
        fetch_linked_text(
            &self.readme_credentialed,
            &self.readme_plain,
            &self.basic_auth,
            url,
            &self.base_url,
            &[],
            max_bytes,
        )
        .await
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let (publisher, ext_name) = Self::parse_id(package)?;
        let ext = self.fetch_extension(publisher, ext_name, "latest").await?;
        let mut versions: Vec<String> = ext.all_versions.into_keys().collect();
        versions.sort();
        Ok(versions)
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let (publisher, ext_name) = Self::parse_id(&pkg.name)?;
        let ext = self
            .fetch_extension(publisher, ext_name, &pkg.version)
            .await?;

        // RFC 0020 §4.2: the signature archive and the public key are
        // artifacts of the same version, addressed by selector, fetched from
        // the URLs the extension document names and cached beside the VSIX.
        let (download_url, what) = match pkg.artifact.as_deref() {
            Some(batlehub_core::services::vsx_signature::SIGNATURE_ARTIFACT) => (
                ext.files.signature.ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "{}.{} v{} is not signed upstream",
                        publisher, ext_name, pkg.version
                    ))
                })?,
                "signature archive",
            ),
            Some(batlehub_core::services::vsx_signature::PUBLIC_KEY_ARTIFACT) => (
                ext.files.public_key.ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "{}.{} v{} names no public key upstream",
                        publisher, ext_name, pkg.version
                    ))
                })?,
                "public key",
            ),
            _ => (
                ext.files.download.ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "no VSIX download available for {}.{} v{}",
                        publisher, ext_name, pkg.version
                    ))
                })?,
                "VSIX",
            ),
        };

        ensure_same_origin(&download_url, &self.base_url)?;
        tracing::debug!(url = %download_url, what, "fetching OpenVSX artifact");

        let response = self
            .get(&download_url)
            .send()
            .await
            .map_err(to_registry_error)?
            .error_for_status()
            .map_err(to_registry_error)?;

        let cache_control = cache_control(&response);

        let stream = response.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        #[derive(Deserialize)]
        struct SearchResponse {
            extensions: Vec<ExtHit>,
        }
        #[derive(Deserialize)]
        struct ExtHit {
            namespace: String,
            name: String,
            version: String,
            description: Option<String>,
        }

        // Strip any /api suffix that some configs include in the base URL,
        // so we don't produce .../api/api/-/search.
        let base = self
            .base_url
            .trim_end_matches('/')
            .trim_end_matches("/api")
            .trim_end_matches('/');

        let url = format!(
            "{}/api/-/search?query={}&size={}",
            base,
            percent_encode(query),
            limit.min(50),
        );

        let res = self.get(&url).send().await.map_err(to_registry_error)?;

        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let body: SearchResponse = res.json().await.map_err(to_registry_error)?;

        Ok(body
            .extensions
            .into_iter()
            .map(|e| UpstreamPackage {
                // OpenVSX package IDs use "publisher.name" (dot separator)
                name: format!("{}.{}", e.namespace, e.name),
                latest_version: e.version,
                description: e.description,
            })
            .collect())
    }
}

impl OpenVsxRegistryClient {
    async fn fetch_extension(
        &self,
        publisher: &str,
        name: &str,
        version: &str,
    ) -> Result<OpenVsxExtension, CoreError> {
        let url = if version == "latest" {
            format!("{}/api/{}/{}", self.base_url, publisher, name)
        } else {
            format!("{}/api/{}/{}/{}", self.base_url, publisher, name, version)
        };

        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "OpenVSX extension {publisher}.{name}@{version} not found"
            )));
        }

        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<OpenVsxExtension>()
            .await
            .map_err(to_registry_error)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use mockito::Server;

    fn pkg(name: &str, version: &str) -> PackageId {
        PackageId::new("openvsx", name, version)
    }

    const EXT_BODY: &str = r#"{"namespace":"ms-python","name":"python","version":"2023.20.0"}"#;

    // ── parse_id ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_id_valid() {
        let (publisher, name) = OpenVsxRegistryClient::parse_id("ms-python.python").unwrap();
        assert_eq!(publisher, "ms-python");
        assert_eq!(name, "python");
    }

    #[test]
    fn parse_id_multiple_dots() {
        // split_once stops at the first dot, so extra dots go into the name segment
        let (publisher, name) = OpenVsxRegistryClient::parse_id("pub.ext.extra").unwrap();
        assert_eq!(publisher, "pub");
        assert_eq!(name, "ext.extra");
    }

    #[test]
    fn parse_id_no_dot() {
        let result = OpenVsxRegistryClient::parse_id("nopublisher");
        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    // ── resolve_metadata ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_metadata_latest() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EXT_BODY)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "latest"))
            .await
            .unwrap();

        assert_eq!(meta.id.version, "2023.20.0");
    }

    #[tokio::test]
    async fn resolve_metadata_specific_version() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EXT_BODY)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();

        assert_eq!(meta.id.version, "2023.20.0");
    }

    #[tokio::test]
    async fn resolve_metadata_no_artifact() {
        let mut server = Server::new_async().await;
        let body = r#"{"namespace":"ms-python","name":"python","version":"2023.20.0","files":{"download":"http://example.com/ext.vsix"}}"#;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();

        assert!(meta.download_url.is_none());
    }

    #[tokio::test]
    async fn resolve_metadata_vsix_artifact() {
        let mut server = Server::new_async().await;
        let body = r#"{"namespace":"ms-python","name":"python","version":"2023.20.0","files":{"download":"http://example.com/ext.vsix"}}"#;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let p = pkg("ms-python.python", "2023.20.0").with_artifact("vsix");
        let meta = client.resolve_metadata(&p).await.unwrap();

        assert_eq!(
            meta.download_url.as_deref(),
            Some("http://example.com/ext.vsix")
        );
    }

    #[tokio::test]
    async fn resolve_metadata_is_signed_true() {
        let mut server = Server::new_async().await;
        let body = r#"{"namespace":"ms-python","name":"python","version":"2023.20.0","files":{"signature":"http://example.com/ext.sigzip"}}"#;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();

        assert_eq!(meta.is_signed, Some(true));
    }

    #[tokio::test]
    async fn resolve_metadata_is_signed_false() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EXT_BODY)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();

        assert_eq!(meta.is_signed, Some(false));
    }

    #[tokio::test]
    async fn resolve_metadata_timestamp_parsed() {
        let mut server = Server::new_async().await;
        let body = r#"{"namespace":"ms-python","name":"python","version":"2023.20.0","timestamp":"2023-11-09T18:25:45Z"}"#;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();

        assert!(meta.published_at.is_some());
    }

    #[tokio::test]
    async fn resolve_metadata_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python/9.9.9")
            .with_status(404)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .resolve_metadata(&pkg("ms-python.python", "9.9.9"))
            .await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    // ── fetch_artifact ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_artifact_streams_bytes() {
        let mut server = Server::new_async().await;
        let dl_path = "/files/ms-python/python/2023.20.0/python.vsix";
        let dl_url = format!("{}{}", server.url(), dl_path);
        let body = format!(
            r#"{{"namespace":"ms-python","name":"python","version":"2023.20.0","files":{{"download":"{}"}}}}"#,
            dl_url
        );

        let _mock_meta = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let _mock_dl = server
            .mock("GET", dl_path)
            .with_status(200)
            .with_body("fake vsix content")
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let fetched = client
            .fetch_artifact(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();
        let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        let content: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
        assert_eq!(content, b"fake vsix content");
    }

    #[tokio::test]
    async fn fetch_artifact_missing_download_url() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EXT_BODY)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&pkg("ms-python.python", "2023.20.0"))
            .await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_extension_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python/9.9.9")
            .with_status(404)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&pkg("ms-python.python", "9.9.9"))
            .await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_extension_server_error() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(500)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&pkg("ms-python.python", "2023.20.0"))
            .await;

        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    // ── list_versions ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_versions_returns_sorted_versions() {
        let mut server = Server::new_async().await;
        let body = r#"{
            "namespace":"ms-python","name":"python","version":"2024.1.0",
            "allVersions":{
                "2024.1.0":"http://example.com/2024.1.0",
                "2023.20.0":"http://example.com/2023.20.0",
                "2023.5.0":"http://example.com/2023.5.0"
            }
        }"#;
        let _mock = server
            .mock("GET", "/api/ms-python/python")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let versions = client.list_versions("ms-python.python").await.unwrap();

        assert_eq!(versions, vec!["2023.20.0", "2023.5.0", "2024.1.0"]);
    }

    #[tokio::test]
    async fn list_versions_empty_when_extension_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/unknown")
            .with_status(404)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client.list_versions("ms-python.unknown").await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn list_versions_invalid_id_returns_error() {
        let client = OpenVsxRegistryClient::new("http://unused", &Default::default()).unwrap();
        let result = client.list_versions("nodot").await;
        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_download_server_error() {
        let mut server = Server::new_async().await;
        let dl_path = "/files/ms-python/python/2023.20.0/python.vsix";
        let dl_url = format!("{}{}", server.url(), dl_path);
        let body = format!(
            r#"{{"namespace":"ms-python","name":"python","version":"2023.20.0","files":{{"download":"{}"}}}}"#,
            dl_url
        );

        let _mock_meta = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let _mock_dl = server
            .mock("GET", dl_path)
            .with_status(500)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&pkg("ms-python.python", "2023.20.0"))
            .await;

        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_rejects_cross_origin_download_url() {
        let mut server = Server::new_async().await;
        let body = r#"{"namespace":"ms-python","name":"python","version":"2023.20.0","files":{"download":"http://evil.example.com/ext.vsix"}}"#;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&pkg("ms-python.python", "2023.20.0"))
            .await;

        assert!(matches!(result, Err(CoreError::Registry(_))));
    }
    // ── README capture (RFC 0007 §2.1, §7.4) ─────────────────────────────────

    /// `files.readme` is a URL, so it travels as a link: following it is an
    /// outbound request in its own right and must not happen on the resolve
    /// path a package manager is waiting on.
    #[tokio::test]
    async fn the_readme_url_travels_as_a_link_not_as_text() {
        let mut server = Server::new_async().await;
        let body = format!(
            r#"{{"namespace":"ms-python","name":"python","version":"2023.20.0",
                 "files":{{"download":"{url}/ext.vsix","readme":"{url}/README.md"}}}}"#,
            url = server.url()
        );
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();

        let found = MetadataReadme::from_extra(&meta.extra).expect("readme link captured");
        assert_eq!(
            found.url.as_deref(),
            Some(format!("{}/README.md", server.url()).as_str())
        );
        assert_eq!(found.content, None);
        // Markdown by protocol, decided before anything is fetched.
        assert_eq!(found.format, ReadmeFormat::Markdown);
    }

    /// An extension's `repository` reaches the console as a page, not as a
    /// clone URL.
    ///
    /// OpenVSX republishes what the extension's `package.json` declared, which
    /// is `…/vscode-yarn.git` for essentially every extension there — the exact
    /// string a reader cannot open.
    #[tokio::test]
    async fn the_repository_and_homepage_become_openable_links() {
        let mut server = Server::new_async().await;
        let body = r#"{"namespace":"ms-python","name":"python","version":"2023.20.0",
            "repository":"https://github.com/microsoft/vscode-python.git",
            "homepage":"https://code.visualstudio.com/docs/python/python-tutorial",
            "files":{}}"#;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();

        let links = MetadataLinks::from_extra(&meta.extra).expect("links captured");
        assert_eq!(
            links.repository.as_deref(),
            Some("https://github.com/microsoft/vscode-python"),
            "the `.git` is the clone URL, not the page"
        );
        assert_eq!(
            links.homepage.as_deref(),
            Some("https://code.visualstudio.com/docs/python/python-tutorial")
        );
    }

    /// An extension that declared neither gets no links at all, rather than an
    /// object of nulls the console would have to special-case.
    #[tokio::test]
    async fn an_extension_declaring_neither_carries_no_links() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/api/ms-python/python/2023.20.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"namespace":"ms-python","name":"python","version":"2023.20.0","files":{}}"#,
            )
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2023.20.0"))
            .await
            .unwrap();
        assert_eq!(MetadataLinks::from_extra(&meta.extra), None);
    }

    /// The read is bounded: an upstream must not be able to make this buffer a
    /// gigabyte to keep 256 KiB of it, so the body is truncated as it streams
    /// rather than after.
    #[tokio::test]
    async fn a_linked_readme_is_read_and_capped() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/README.md")
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body("0123456789abcdef")
            .expect(2)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let url = format!("{}/README.md", server.url());

        assert_eq!(
            client.fetch_linked_readme(&url, 4096).await.unwrap(),
            Some("0123456789abcdef".to_owned())
        );
        assert_eq!(
            client.fetch_linked_readme(&url, 8).await.unwrap(),
            Some("01234567".to_owned())
        );
    }

    /// An upstream that has been compromised or misconfigured must not be able
    /// to use the README link to point BatleHub at an internal host — the same
    /// guard tarball URLs already get.
    #[tokio::test]
    async fn a_cross_origin_readme_url_is_refused() {
        let server = Server::new_async().await;
        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_linked_readme("http://169.254.169.254/latest/meta-data/", 4096)
            .await;
        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    /// A README asset that has gone is a fact about that extension, not a
    /// failure worth logging as one.
    #[tokio::test]
    async fn a_missing_readme_asset_is_none_not_an_error() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/README.md")
            .with_status(404)
            .create_async()
            .await;

        let client = OpenVsxRegistryClient::new(server.url(), &Default::default()).unwrap();
        let url = format!("{}/README.md", server.url());
        assert_eq!(client.fetch_linked_readme(&url, 4096).await.unwrap(), None);
    }
}
