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
    ResolvedExtension, FILTER_EXTENSION_NAME, FLAG_INCLUDE_ASSET_URI, FLAG_INCLUDE_FILES,
    FLAG_INCLUDE_LATEST_ONLY, FLAG_INCLUDE_VERSIONS, GALLERY_API_ACCEPT, README_ASSET_TYPE,
    SIGNATURE_ASSET_TYPE, VSIX_ASSET_TYPE,
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
        // There is no version criterion in `extensionquery`: the filter types
        // are a closed set naming *extensions* (name, id, tag, target, search
        // text), and a body carrying one the API does not know is refused with
        // `400 Value does not fall within the expected range` — not an empty
        // result. So a pinned version is asked for the same way the editor asks
        // for it: the extension's whole version list, picked from here.
        let latest_only = version == "latest";
        let mut flags = FLAG_INCLUDE_VERSIONS | FLAG_INCLUDE_FILES | FLAG_INCLUDE_ASSET_URI;
        if latest_only {
            flags |= FLAG_INCLUDE_LATEST_ONLY;
        }

        let body = ExtensionQueryRequest {
            filters: vec![ExtensionQueryFilter {
                criteria: vec![ExtensionQueryCriteria {
                    filter_type: FILTER_EXTENSION_NAME,
                    value: format!("{publisher}.{name}"),
                }],
            }],
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

        let version_info = if latest_only {
            ext.versions.into_iter().next().ok_or_else(|| {
                CoreError::NotFound(format!(
                    "no versions available for {publisher}.{name}@{version}"
                ))
            })?
        } else {
            ext.versions
                .into_iter()
                .find(|v| v.version == version)
                .ok_or_else(|| {
                    CoreError::NotFound(format!(
                        "VS Code Marketplace extension {publisher}.{name}@{version} not found"
                    ))
                })?
        };

        Ok(ResolvedExtension {
            version_info,
            display_name: ext.display_name,
            description: ext.description,
        })
    }
}

/// Decode a `Content-Encoding: gzip` body into the bytes it encodes.
///
/// The gallery's `vspackage` endpoint answers `Content-Encoding: gzip`
/// *unsolicited* — this client asks for no encoding and is sent one anyway —
/// so what arrives on the wire is a gzip stream wrapping the VSIX, not the
/// VSIX. A content encoding is a property of the transfer, not of the
/// artifact: relaying it as it came served every editor a gzip file under a
/// `.vsix` name (`Could not find EOCD`, because a zip's directory lives at the
/// end and there is no zip here), stored those bytes under the artifact's key,
/// and failed every asset route with it — each of those reads a file *inside*
/// the archive.
///
/// Streaming rather than buffering: `ProxyService` bounds the artifact size,
/// and a decoder that holds one chunk keeps that bound meaningful.
fn gunzip_stream<S>(
    stream: S,
) -> impl futures::Stream<Item = Result<bytes::Bytes, CoreError>> + Send
where
    S: futures::Stream<Item = Result<bytes::Bytes, CoreError>> + Send + Unpin + 'static,
{
    use futures::StreamExt;
    use std::io::Write;

    let decoder = flate2::write::GzDecoder::new(Vec::new());
    futures::stream::unfold(
        (stream, Some(decoder)),
        |(mut stream, mut decoder)| async move {
            loop {
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        // `None` only once the body has ended or failed, and both
                        // of those arms return rather than come back here.
                        let dec = decoder.as_mut()?;
                        if let Err(e) = dec.write_all(&chunk) {
                            let err = CoreError::Registry(format!(
                                "upstream sent a malformed gzip body: {e}"
                            ));
                            return Some((Err(err), (stream, None)));
                        }
                        let decoded = std::mem::take(dec.get_mut());
                        if decoded.is_empty() {
                            // A chunk that completed no window yet: ask for the
                            // next one rather than yield an empty frame.
                            continue;
                        }
                        return Some((Ok(bytes::Bytes::from(decoded)), (stream, decoder)));
                    }
                    Some(Err(e)) => return Some((Err(e), (stream, None))),
                    None => {
                        let dec = decoder.take()?;
                        return match dec.finish() {
                            Ok(tail) if !tail.is_empty() => {
                                Some((Ok(bytes::Bytes::from(tail)), (stream, None)))
                            }
                            Ok(_) => None,
                            Err(e) => {
                                let err = CoreError::Registry(format!(
                                    "upstream gzip body ended mid-stream: {e}"
                                ));
                                Some((Err(err), (stream, None)))
                            }
                        };
                    }
                }
            }
        },
    )
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

        let mut upstream_supplied = false;
        let url = if pkg.artifact.as_deref()
            == Some(batlehub_core::services::vsx_signature::SIGNATURE_ARTIFACT)
        {
            upstream_supplied = true;
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

        let response = if upstream_supplied {
            // The signature archive's URL came out of the gallery *document*, so
            // only its first hop was origin-checked above. `self.http` follows
            // up to ten redirects itself, with the operator's configured auth
            // headers attached and no check on any hop — the same defect this
            // MR fixed in the GitHub client and in the sigstore scanner. So the
            // redirects are followed here instead, one at a time: the first
            // hop's own origin (the one `ensure_linked_origin` just approved)
            // and the configured base stay credentialed, and any hop that leaves
            // them is checked against the private, reserved and link-local
            // ranges and re-issued without the credential.
            let parsed = reqwest::Url::parse(&url)
                .map_err(|e| CoreError::Registry(format!("invalid upstream URL '{url}': {e}")))?;
            let trusted = vec![self.base_url.clone(), parsed.origin().ascii_serialization()];
            crate::registry::ssrf::fetch_following_redirects_trusting(
                &self.readme_credentialed,
                &self.readme_plain,
                &None,
                &trusted,
                parsed,
            )
            .await?
        } else {
            // The package URL is built from the configured base, but the gallery
            // answers it with a `302` to its own CDN, and `self.http` would
            // follow that — and any further hop — unchecked and still
            // credentialed. Same walk as the branch above, with only the base
            // trusted: the first hop *is* the base here, so nothing else needs
            // to be.
            let parsed = reqwest::Url::parse(&url)
                .map_err(|e| CoreError::Registry(format!("invalid upstream URL '{url}': {e}")))?;
            crate::registry::ssrf::fetch_following_redirects(
                &self.readme_credentialed,
                &self.readme_plain,
                &None,
                &self.base_url,
                parsed,
            )
            .await?
        };

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

        // Unsolicited, so it is checked for rather than assumed absent: see
        // [`gunzip_stream`].
        let gzipped = response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|enc| {
                enc.split(',')
                    .any(|e| matches!(e.trim().to_ascii_lowercase().as_str(), "gzip" | "x-gzip"))
            });

        let stream: batlehub_core::ports::ArtifactStream =
            Box::pin(response.bytes_stream().map_err(to_registry_error));
        let stream: batlehub_core::ports::ArtifactStream = if gzipped {
            Box::pin(gunzip_stream(stream))
        } else {
            stream
        };

        Ok(FetchedArtifact {
            stream,
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

    /// Several versions, newest first, as the gallery returns them.
    fn ext_body_versions(versions: &[&str]) -> String {
        let versions: Vec<String> = versions
            .iter()
            .map(|v| {
                format!(
                    r#"{{"version":"{v}","lastUpdated":"2024-01-01T00:00:00Z","files":[{{"assetType":"Microsoft.VisualStudio.Services.VSIXPackage","source":"http://example.com/python-{v}.vsix"}}]}}"#
                )
            })
            .collect();
        format!(
            r#"{{"results":[{{"extensions":[{{"displayName":"Python","shortDescription":"Python language support","publisher":{{"publisherName":"ms-python"}},"versions":[{}]}}]}}]}}"#,
            versions.join(",")
        )
    }

    /// The gallery's answer to a criterion it does not know — which is what a
    /// version filter is. It is a `400`, not an empty result, so every pinned
    /// resolve failed: `code --install-extension` asks for the version the
    /// gallery query named, and got `502 Bad Gateway` from this proxy.
    const UNKNOWN_CRITERION: &str = r#"{"message":"Value does not fall within the expected range.","typeKey":"ArgumentException"}"#;

    /// A pinned version is asked for by extension name alone.
    ///
    /// `extensionquery` has no version criterion: the filter types name
    /// extensions, and a body carrying an unknown one is refused outright. The
    /// mock is the gallery on that point — one criterion, `filterType` 7, or a
    /// `400` — so the query shape is what this asserts, not just the answer.
    #[tokio::test]
    async fn a_pinned_version_is_picked_from_the_version_list() {
        let mut server = Server::new_async().await;
        let _refusal = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(UNKNOWN_CRITERION)
            .expect_at_most(0)
            .create_async()
            .await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .match_request(|req| {
                let body: serde_json::Value =
                    serde_json::from_slice(req.body().unwrap()).expect("a JSON query body");
                let criteria = body["filters"][0]["criteria"].as_array().unwrap();
                criteria.len() == 1 && criteria[0]["filterType"] == 7
            })
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body_versions(&["2024.3.0", "2024.2.1", "2024.1.0"]))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let meta = client
            .resolve_metadata(&pkg("ms-python.python", "2024.2.1").with_artifact("vsix"))
            .await
            .unwrap();

        assert_eq!(meta.id.version, "2024.2.1");
        // The asset of the version asked for, not the newest one's.
        assert_eq!(
            meta.download_url.as_deref(),
            Some("http://example.com/python-2024.2.1.vsix")
        );
    }

    /// A version the extension does not have is `NotFound`, not the newest one:
    /// the list is filtered here now, so nothing may fall through to `[0]`.
    #[tokio::test]
    async fn a_version_absent_from_the_list_is_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("POST", "/_apis/public/gallery/extensionquery")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ext_body_versions(&["2024.3.0", "2024.2.1"]))
            .create_async()
            .await;

        let client =
            VsCodeMarketplaceRegistryClient::new(server.url(), &Default::default()).unwrap();
        let err = client
            .resolve_metadata(&pkg("ms-python.python", "1.0.0"))
            .await
            .unwrap_err();

        assert!(matches!(err, CoreError::NotFound(_)), "got {err:?}");
    }

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

    /// The gallery answers `vspackage` with `Content-Encoding: gzip` whether or
    /// not it was asked to, so what the wire carries is a gzip stream around the
    /// VSIX. Relayed undecoded, every editor got a gzip file called `.vsix` and
    /// refused it ("Could not find EOCD"), and the cache kept those bytes.
    #[tokio::test]
    async fn a_gzip_encoded_artifact_is_decoded() {
        use std::io::Write;

        let vsix = b"PK\x03\x04 pretend this is a zip, and a long enough one to compress";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(vsix).unwrap();
        let gzipped = encoder.finish().unwrap();
        assert_ne!(gzipped.as_slice(), vsix.as_slice());

        let mut server = Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                "/_apis/public/gallery/publishers/ms-python/vsextensions/python/2024.2.1/vspackage",
            )
            .with_status(200)
            .with_header("content-encoding", "gzip")
            .with_body(gzipped)
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

        assert_eq!(content, vsix.as_slice());
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
