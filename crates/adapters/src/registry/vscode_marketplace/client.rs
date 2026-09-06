use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{MetadataReadme, PackageId, PackageMetadata, ReadmeFormat},
    error::CoreError,
    ports::{FetchedArtifact, RegistryClient},
};

use super::super::http_client::{
    ensure_linked_origin, fetch_linked_text, new_http_client, no_redirect_client_pair,
    to_registry_error, UpstreamHttpOptions,
};
use super::models::{
    ExtensionQueryCriteria, ExtensionQueryFilter, ExtensionQueryRequest, ExtensionQueryResponse,
    ResolvedExtension, FILTER_EXTENSION_NAME, FILTER_VERSION, FLAG_INCLUDE_ASSET_URI,
    FLAG_INCLUDE_FILES, FLAG_INCLUDE_LATEST_ONLY, FLAG_INCLUDE_VERSIONS, GALLERY_API_ACCEPT,
    README_ASSET_TYPE, SIGNATURE_ASSET_TYPE, VSIX_ASSET_TYPE,
};

/// VS Code Marketplace registry client (marketplace.visualstudio.com or compatible).
///
/// Supported `PackageId` conventions:
/// - `name` format: `"{publisher}.{extension}"` (e.g. `"ms-python.python"`)
/// - `version = "latest"` → current latest version
/// - `version = "1.2.3"`  → specific semver version
/// - `artifact = Some("vsix")` → stream the `.vsix` extension package
pub struct VsCodeMarketplaceRegistryClient {
    http: reqwest::Client,
    /// The linked-asset pair: redirects disabled so the SSRF guard validates
    /// every hop, credentials attached only while the chain stays on `base_url`
    /// (see [`fetch_linked_text`]). The public gallery's assets live off that
    /// origin by construction, so on a public-gallery deployment the README read
    /// is credential-free — which is what it should be: the CDN is not the host
    /// the operator configured a token for.
    readme_credentialed: reqwest::Client,
    readme_plain: reqwest::Client,
    base_url: String,
}

/// The host the public gallery API answers on.
///
/// The asset exception below is granted only when this *is* the configured
/// base: a self-hosted mirror serves its own assets from its own origin, and
/// has no business being pointed at Microsoft's CDN by a response it proxies.
const PUBLIC_GALLERY_HOST: &str = "marketplace.visualstudio.com";

/// The gallery's asset CDN, one sub-domain per publisher —
/// `ms-python.gallerycdn.vsassets.io`, and so on.
///
/// `files[].source` — the `Content.Details` README included — always points
/// here, never at the API host, so checking the asset URL strictly against
/// `base_url` refused *every* linked README this kind has: none was ever stored,
/// while `readme_support()` advertised `MetadataLinked`.
const GALLERY_CDN_HOST_SUFFIX: &str = "gallerycdn.vsassets.io";

impl VsCodeMarketplaceRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(10), opts)?;
        let (readme_credentialed, readme_plain) = no_redirect_client_pair(opts)?;
        Ok(Self {
            http,
            readme_credentialed,
            readme_plain,
            base_url: base_url.into(),
        })
    }

    /// Hosts a linked asset may be read from besides the base URL's own origin.
    ///
    /// Empty for anything but the public gallery — see [`PUBLIC_GALLERY_HOST`].
    fn asset_host_suffixes(&self) -> &'static [&'static str] {
        let is_public_gallery = reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .is_some_and(|host| host == PUBLIC_GALLERY_HOST);
        if is_public_gallery {
            &[GALLERY_CDN_HOST_SUFFIX]
        } else {
            &[]
        }
    }

    fn parse_id(name: &str) -> Result<(&str, &str), CoreError> {
        name.split_once('.').ok_or_else(|| {
            CoreError::Registry(format!(
                "invalid VS Code Marketplace extension id '{name}': expected '{{publisher}}.{{name}}'"
            ))
        })
    }

    async fn query_extension(
        &self,
        publisher: &str,
        name: &str,
        version: &str,
    ) -> Result<ResolvedExtension, CoreError> {
        let (flags, criteria) = if version == "latest" {
            (
                FLAG_INCLUDE_VERSIONS
                    | FLAG_INCLUDE_FILES
                    | FLAG_INCLUDE_ASSET_URI
                    | FLAG_INCLUDE_LATEST_ONLY,
                vec![ExtensionQueryCriteria {
                    filter_type: FILTER_EXTENSION_NAME,
                    value: format!("{publisher}.{name}"),
                }],
            )
        } else {
            (
                FLAG_INCLUDE_VERSIONS | FLAG_INCLUDE_FILES | FLAG_INCLUDE_ASSET_URI,
                vec![
                    ExtensionQueryCriteria {
                        filter_type: FILTER_EXTENSION_NAME,
                        value: format!("{publisher}.{name}"),
                    },
                    ExtensionQueryCriteria {
                        filter_type: FILTER_VERSION,
                        value: version.to_owned(),
                    },
                ],
            )
        };

        let body = ExtensionQueryRequest {
            filters: vec![ExtensionQueryFilter { criteria }],
            flags,
        };

        let url = format!("{}/_apis/public/gallery/extensionquery", self.base_url);

        let resp = self
            .http
            .post(&url)
            .header("Accept", GALLERY_API_ACCEPT)
            .json(&body)
            .send()
            .await
            .map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "VS Code Marketplace extension {publisher}.{name}@{version} not found"
            )));
        }

        let query_resp = resp
            .error_for_status()
            .map_err(to_registry_error)?
            .json::<ExtensionQueryResponse>()
            .await
            .map_err(to_registry_error)?;

        // The API returns 200 with empty results for missing extensions
        let ext = query_resp
            .results
            .into_iter()
            .next()
            .and_then(|r| r.extensions.into_iter().next())
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "VS Code Marketplace extension {publisher}.{name}@{version} not found"
                ))
            })?;

        let version_info = ext.versions.into_iter().next().ok_or_else(|| {
            CoreError::NotFound(format!(
                "no versions available for {publisher}.{name}@{version}"
            ))
        })?;

        Ok(ResolvedExtension {
            version_info,
            display_name: ext.display_name,
            description: ext.description,
        })
    }
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for VsCodeMarketplaceRegistryClient {
    fn registry_type(&self) -> &str {
        "vscode-marketplace"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let (publisher, ext_name) = Self::parse_id(&pkg.name)?;
        let resolved = self
            .query_extension(publisher, ext_name, &pkg.version)
            .await?;

        let published_at = resolved
            .version_info
            .last_updated
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let vsix_url = resolved
            .version_info
            .files
            .iter()
            .find(|f| f.asset_type == VSIX_ASSET_TYPE)
            .map(|f| f.source.clone());

        let download_url = if pkg.artifact.as_deref() == Some("vsix") {
            vsix_url
        } else {
            None
        };

        let readme = resolved
            .version_info
            .files
            .iter()
            .find(|f| f.asset_type == README_ASSET_TYPE)
            // Markdown by protocol, whatever `Content-Type` the asset URL
            // answers with when it is read.
            .map(|f| MetadataReadme::linked(&f.source, ReadmeFormat::Markdown));

        // The marketplace signs every extension it publishes and names the
        // archive as one more file (RFC 0020 §4.2); the editor verifies it
        // itself, so the proxy relays it and this flag advertises it.
        let signature_url = resolved
            .version_info
            .files
            .iter()
            .find(|f| f.asset_type == SIGNATURE_ASSET_TYPE)
            .map(|f| f.source.clone());

        let extra = serde_json::json!({
            "resolved_version": resolved.version_info.version,
            "display_name": resolved.display_name,
            "description": resolved.description,
            "readme": readme,
            "signature_url": signature_url,
        });

        Ok(PackageMetadata {
            id: PackageId {
                version: resolved.version_info.version,
                ..pkg.clone()
            },
            published_at,
            download_url,
            checksum: None,
            is_signed: Some(signature_url.is_some()),
            extra,
            cache_control: None,
        })
    }

    /// Read the `Content.Details` asset the gallery response linked to.
    ///
    /// Never called on the resolve path — the link travels on `extra` and this
    /// runs in the detached introspection task (RFC 0007 §5.1).
    async fn fetch_linked_readme(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Option<String>, CoreError> {
        fetch_linked_text(
            &self.readme_credentialed,
            &self.readme_plain,
            &None,
            url,
            &self.base_url,
            self.asset_host_suffixes(),
            max_bytes,
        )
        .await
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let (publisher, ext_name) = Self::parse_id(&pkg.name)?;

        let url = if pkg.artifact.as_deref()
            == Some(batlehub_core::services::vsx_signature::SIGNATURE_ARTIFACT)
        {
            // RFC 0020 §4.2: the signature archive is the file the gallery
            // document names, on the gallery's own CDN — the same hosts the
            // README link is allowed to point at, and no other.
            let resolved = self
                .query_extension(publisher, ext_name, &pkg.version)
                .await?;
            let source = resolved
                .version_info
                .files
                .iter()
                .find(|f| f.asset_type == SIGNATURE_ASSET_TYPE)
                .map(|f| f.source.clone())
                .ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "{publisher}.{ext_name}@{} is not signed upstream",
                        pkg.version
                    ))
                })?;
            ensure_linked_origin(&source, &self.base_url, self.asset_host_suffixes())?;
            source
        } else {
            format!(
                "{base}/_apis/public/gallery/publishers/{publisher}/vsextensions/{name}/{version}/vspackage",
                base = self.base_url,
                name = ext_name,
                version = pkg.version,
            )
        };

        tracing::debug!(url = %url, "fetching VS Code Marketplace artifact");

        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(to_registry_error)?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "VS Code Marketplace extension {publisher}.{ext_name}@{} not found",
                pkg.version,
            )));
        }

        let response = response.error_for_status().map_err(to_registry_error)?;

        let cache_control = response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let stream = response.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    /// List all available versions for a VS Code extension (`publisher.name`).
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let (publisher, ext_name) = Self::parse_id(package)?;

        let body = ExtensionQueryRequest {
            filters: vec![ExtensionQueryFilter {
                criteria: vec![ExtensionQueryCriteria {
                    filter_type: FILTER_EXTENSION_NAME,
                    value: format!("{publisher}.{ext_name}"),
                }],
            }],
            flags: FLAG_INCLUDE_VERSIONS,
        };

        let url = format!("{}/_apis/public/gallery/extensionquery", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("Accept", GALLERY_API_ACCEPT)
            .json(&body)
            .send()
            .await
            .map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "vscode-marketplace: extensionquery returned {}",
                resp.status()
            )));
        }

        let query_resp: ExtensionQueryResponse = resp.json().await.map_err(to_registry_error)?;

        let versions = query_resp
            .results
            .into_iter()
            .next()
            .and_then(|r| r.extensions.into_iter().next())
            .map(|ext| ext.versions.into_iter().map(|v| v.version).collect())
            .unwrap_or_default();

        Ok(versions)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use mockito::Server;

    fn pkg(name: &str, version: &str) -> PackageId {
        PackageId::new("vscode-marketplace", name, version)
    }

    fn ext_body(version: &str) -> String {
        format!(
            r#"{{"results":[{{"extensions":[{{"displayName":"Python","shortDescription":"Python language support","publisher":{{"publisherName":"ms-python"}},"versions":[{{"version":"{version}","lastUpdated":"2024-01-01T00:00:00Z","files":[{{"assetType":"Microsoft.VisualStudio.Services.VSIXPackage","source":"http://example.com/python.vsix"}}]}}]}}]}}]}}"#
        )
    }

    const EMPTY_RESULTS: &str = r#"{"results":[{"extensions":[]}]}"#;

    /// The gallery names its README on a CDN the API host does not share, so a
    /// strict same-origin check refused every one of them — no VS Code
    /// Marketplace README was ever stored, while the support table advertised
    /// `MetadataLinked`. The exception is granted only to the public gallery: a
    /// mirror serves its own assets from its own origin.
    #[test]
    fn the_asset_cdn_is_trusted_only_when_the_base_is_the_public_gallery() {
        let public = VsCodeMarketplaceRegistryClient::new(
            "https://marketplace.visualstudio.com",
            &UpstreamHttpOptions::default(),
        )
        .unwrap();
        assert_eq!(public.asset_host_suffixes(), &[GALLERY_CDN_HOST_SUFFIX]);

        let readme_url =
            "https://ms-python.gallerycdn.vsassets.io/extensions/ms-python/python/1.0.0/Details";
        assert!(crate::registry::http_client::ensure_linked_origin(
            readme_url,
            "https://marketplace.visualstudio.com",
            public.asset_host_suffixes(),
        )
        .is_ok());

        let mirror = VsCodeMarketplaceRegistryClient::new(
            "https://gallery.corp.example",
            &UpstreamHttpOptions::default(),
        )
        .unwrap();
        assert!(mirror.asset_host_suffixes().is_empty());
        assert!(crate::registry::http_client::ensure_linked_origin(
            readme_url,
            "https://gallery.corp.example",
            mirror.asset_host_suffixes(),
        )
        .is_err());
    }

    #[test]
    fn parse_id_valid() {
        let (publisher, name) =
            VsCodeMarketplaceRegistryClient::parse_id("ms-python.python").unwrap();
        assert_eq!(publisher, "ms-python");
        assert_eq!(name, "python");
    }

    #[test]
    fn parse_id_multiple_dots() {
        let (publisher, name) = VsCodeMarketplaceRegistryClient::parse_id("pub.ext.extra").unwrap();
        assert_eq!(publisher, "pub");
        assert_eq!(name, "ext.extra");
    }

    #[test]
    fn parse_id_no_dot() {
        let result = VsCodeMarketplaceRegistryClient::parse_id("nopublisher");
        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    #[tokio::test]
    async fn resolve_metadata_latest() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body("2024.2.1"))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "latest"))
            .await
            .unwrap();

        assert_eq!(meta.id.version, "2024.2.1");
    }

    #[tokio::test]
    async fn resolve_metadata_specific_version() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body("2024.2.1"))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2024.2.1"))
            .await
            .unwrap();

        assert_eq!(meta.id.version, "2024.2.1");
    }

    #[tokio::test]
    async fn resolve_metadata_timestamp_parsed() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body("2024.2.1"))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "latest"))
            .await
            .unwrap();

        assert!(meta.published_at.is_some());
    }

    #[tokio::test]
    async fn resolve_metadata_no_artifact() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body("2024.2.1"))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2024.2.1"))
            .await
            .unwrap();

        assert!(meta.download_url.is_none());
    }

    #[tokio::test]
    async fn resolve_metadata_vsix_artifact() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body("2024.2.1"))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let p = pkg("ms-python.python", "2024.2.1").with_artifact("vsix");
        let meta = client.resolve_metadata(&p).await.unwrap();

        assert_eq!(
            meta.download_url.as_deref(),
            Some("http://example.com/python.vsix")
        );
    }

    #[tokio::test]
    async fn resolve_metadata_is_signed_false() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body("2024.2.1"))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2024.2.1"))
            .await
            .unwrap();

        assert_eq!(meta.is_signed, Some(false));
    }

    #[tokio::test]
    async fn resolve_metadata_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(EMPTY_RESULTS)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .resolve_metadata(&pkg("ms-python.python", "9.9.9"))
            .await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn resolve_metadata_server_error() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(500)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .resolve_metadata(&pkg("ms-python.python", "2024.2.1"))
            .await;

        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_streams_bytes() {
        let mut server = Server::new_async().await;
        let dl_path =
            "/_apis/public/gallery/publishers/ms-python/vsextensions/python/2024.2.1/vspackage";
        let _mock = server
            .mock("GET", dl_path)
            .with_status(200)
            .with_body("fake vsix content")
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let fetched = client
            .fetch_artifact(&pkg("ms-python.python", "2024.2.1"))
            .await
            .unwrap();
        let chunks: Vec<bytes::Bytes> = fetched.stream.try_collect().await.unwrap();
        let content: Vec<u8> = chunks.into_iter().flat_map(|b| b.to_vec()).collect();
        assert_eq!(content, b"fake vsix content");
    }

    #[tokio::test]
    async fn fetch_artifact_not_found() {
        let mut server = Server::new_async().await;
        let dl_path =
            "/_apis/public/gallery/publishers/ms-python/vsextensions/python/9.9.9/vspackage";
        let _mock = server
            .mock("GET", dl_path)
            .with_status(404)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&pkg("ms-python.python", "9.9.9"))
            .await;

        assert!(matches!(result, Err(CoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn fetch_artifact_server_error() {
        let mut server = Server::new_async().await;
        let dl_path =
            "/_apis/public/gallery/publishers/ms-python/vsextensions/python/2024.2.1/vspackage";
        let _mock = server
            .mock("GET", dl_path)
            .with_status(500)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let result = client
            .fetch_artifact(&pkg("ms-python.python", "2024.2.1"))
            .await;

        assert!(matches!(result, Err(CoreError::Registry(_))));
    }

    #[tokio::test]
    async fn list_versions_returns_all_versions() {
        let mut server = Server::new_async().await;
        let body = r#"{"results":[{"extensions":[{"displayName":"Python","shortDescription":"desc","publisher":{"publisherName":"ms-python"},"versions":[{"version":"2024.3.0","lastUpdated":"2024-03-01T00:00:00Z","files":[]},{"version":"2024.2.1","lastUpdated":"2024-02-15T00:00:00Z","files":[]},{"version":"2024.1.0","lastUpdated":"2024-01-10T00:00:00Z","files":[]}]}]}]}"#;

        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let versions = client.list_versions("ms-python.python").await.unwrap();
        assert_eq!(versions, vec!["2024.3.0", "2024.2.1", "2024.1.0"]);
    }

    #[tokio::test]
    async fn list_versions_empty_for_missing_extension() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_body(EMPTY_RESULTS)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let versions = client.list_versions("unknown.extension").await.unwrap();
        assert!(versions.is_empty());
    }
    // ── README capture (RFC 0007 §2.1, §7.4) ─────────────────────────────────

    /// The `Content.Details` asset is a URL, so it travels as a link and is
    /// read in the detached introspection task — not on the resolve path.
    #[tokio::test]
    async fn the_content_details_asset_travels_as_a_link() {
        let mut server = Server::new_async().await;
        let body = format!(
            r#"{{"results":[{{"extensions":[{{"displayName":"Python",
                "shortDescription":"Python language support",
                "publisher":{{"publisherName":"ms-python"}},
                "versions":[{{"version":"2024.1.0","lastUpdated":"2024-01-01T00:00:00Z",
                "files":[
                  {{"assetType":"Microsoft.VisualStudio.Services.VSIXPackage","source":"{url}/python.vsix"}},
                  {{"assetType":"Microsoft.VisualStudio.Services.Content.Details","source":"{url}/README.md"}}
                ]}}]}}]}}]}}"#,
            url = server.url()
        );
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2024.1.0"))
            .await
            .unwrap();

        let found = MetadataReadme::from_extra(&meta.extra).expect("readme link captured");
        assert_eq!(
            found.url.as_deref(),
            Some(format!("{}/README.md", server.url()).as_str())
        );
        assert_eq!(found.content, None);
        assert_eq!(found.format, ReadmeFormat::Markdown);
    }

    /// An extension whose gallery entry lists no `Content.Details` asset claims
    /// nothing rather than falling back to the VSIX URL.
    #[tokio::test]
    async fn an_extension_without_a_readme_asset_captures_nothing() {
        let mut server = Server::new_async().await;
        let body = format!(
            r#"{{"results":[{{"extensions":[{{"displayName":"Python",
                "shortDescription":"desc","publisher":{{"publisherName":"ms-python"}},
                "versions":[{{"version":"2024.1.0","lastUpdated":"2024-01-01T00:00:00Z",
                "files":[{{"assetType":"Microsoft.VisualStudio.Services.VSIXPackage","source":"{url}/python.vsix"}}]}}]}}]}}]}}"#,
            url = server.url()
        );
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2024.1.0"))
            .await
            .unwrap();
        assert_eq!(MetadataReadme::from_extra(&meta.extra), None);
    }

    /// A compromised or misconfigured gallery must not be able to use the
    /// README asset URL to point BatleHub at an internal host.
    #[tokio::test]
    async fn a_cross_origin_readme_asset_is_refused() {
        let server = Server::new_async().await;
        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        assert!(matches!(
            client
                .fetch_linked_readme("http://169.254.169.254/latest/meta-data/", 4096)
                .await,
            Err(CoreError::Registry(_))
        ));
    }
}
