use crate::registry::encode_path;
use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, UpstreamPackage, VersionDocument},
};

use super::super::http_client::{
    basic_auth_get, cache_control, new_http_client, parse_http_date, percent_encode,
    to_registry_error, UpstreamHttpOptions,
};
use super::models::{decode_name, extract_all_xml_tags, extract_xml_tag, parse_last_updated};

/// Maven Central-compatible registry client.
///
/// Proxies any Maven repository that follows the standard Maven 2 layout:
/// `{base_url}/{group/path}/{artifactId}/{version}/{filename}`
///
/// Default upstream: `https://repo1.maven.org/maven2`
///
/// Supported `PackageId` conventions:
/// - `name`    = `"{groupId}:{artifactId}"` (e.g. `"com.google.guava:guava"`)
/// - `version` = version string (e.g. `"32.0.1-jre"`) **or** `"maven-metadata.xml"` to
///   fetch the artifact-level metadata document
/// - `artifact`:
///   - `None` → `.pom` file for that version (or the metadata XML when
///     `version == "maven-metadata.xml"`)
///   - `Some(filename)` → the exact filename to fetch from the version directory
///     (e.g. `"guava-32.0.1-jre.jar"`, `"guava-32.0.1-jre.jar.sha1"`)
pub struct MavenRegistryClient {
    pub(super) http: reqwest::Client,
    pub(super) base_url: String,
    pub(super) basic_auth: Option<(String, String)>,
    /// Resolved search URL base. `None` = disabled; `Some(url)` = use this.
    pub(super) search_base: Option<String>,
}

impl MavenRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(10), opts)?;

        // Resolve the search base URL:
        //   - explicit empty string → disabled
        //   - explicit non-empty URL → use as-is
        //   - absent → default to Maven Central's search service
        let search_base = match opts.search_url.as_deref() {
            Some("") => None,
            Some(url) => Some(url.trim_end_matches('/').to_owned()),
            None => Some("https://search.maven.org".to_owned()),
        };

        Ok(Self {
            http,
            base_url: base_url.into(),
            basic_auth: opts.basic_auth.clone(),
            search_base,
        })
    }

    pub(super) fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }

    pub(super) fn head(&self, url: &str) -> reqwest::RequestBuilder {
        let rb = self.http.head(url);
        match &self.basic_auth {
            Some((u, p)) => rb.basic_auth(u, Some(p)),
            None => rb,
        }
    }

    /// Build the upstream URL for the given `PackageId`.
    pub(super) fn artifact_url(&self, pkg: &PackageId) -> Result<String, CoreError> {
        let (group_id, artifact_id) = decode_name(&pkg.name)?;
        let group_path = encode_path(&group_id.replace('.', "/"));
        let artifact_id = encode_path(artifact_id);
        let base = self.base_url.trim_end_matches('/');

        if pkg.version == "maven-metadata.xml" {
            Ok(format!(
                "{base}/{group_path}/{artifact_id}/maven-metadata.xml"
            ))
        } else {
            let version = encode_path(&pkg.version);
            let filename = match pkg.artifact.as_deref() {
                Some(f) => encode_path(f),
                None => format!("{artifact_id}-{version}.pom"),
            };
            Ok(format!(
                "{base}/{group_path}/{artifact_id}/{version}/{filename}"
            ))
        }
    }

    /// HEAD-request the `.pom` file for a specific version and return its `Last-Modified` timestamp.
    pub(super) async fn head_pom_last_modified(
        &self,
        pkg: &PackageId,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let (group_id, artifact_id) = decode_name(&pkg.name).ok()?;
        let group_path = encode_path(&group_id.replace('.', "/"));
        let artifact_id = encode_path(artifact_id);
        let base = self.base_url.trim_end_matches('/');
        let url = format!(
            "{base}/{group_path}/{artifact_id}/{v}/{artifact_id}-{v}.pom",
            v = encode_path(&pkg.version)
        );
        let resp = self.head(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_http_date)
    }

    pub(super) async fn fetch_metadata_xml(
        &self,
        pkg: &PackageId,
    ) -> Result<(String, Option<String>), CoreError> {
        let (group_id, artifact_id) = decode_name(&pkg.name)?;
        let group_path = encode_path(&group_id.replace('.', "/"));
        let artifact_id = encode_path(artifact_id);
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/{group_path}/{artifact_id}/maven-metadata.xml");

        let resp = self
            .get(&url)
            .send()
            .await
            .map_err(|e| CoreError::Registry(format!("Maven metadata request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "Maven metadata not found for {}:{}",
                group_id, artifact_id
            )));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "Maven metadata request returned {}: {artifact_id}",
                resp.status()
            )));
        }

        let cache_control = cache_control(&resp);

        let body = resp
            .text()
            .await
            .map_err(|e| CoreError::Registry(format!("reading Maven metadata: {e}")))?;

        Ok((body, cache_control))
    }
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for MavenRegistryClient {
    /// `maven-metadata.xml` — the document a Maven client resolves a version
    /// range (and `LATEST`/`RELEASE`) against.
    ///
    /// Returned as text with `text/xml`: the proxy has to send back XML, and an
    /// upstream that mislabels its own metadata as `text/plain` must not make
    /// this proxy mislabel it too.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if kind != DocumentKind::Versions {
            return Err(CoreError::NotSupported(format!(
                "maven has no '{kind}' listing document"
            )));
        }
        let pkg = PackageId::new("", package, "maven-metadata.xml");
        let (xml, _) = self.fetch_metadata_xml(&pkg).await?;
        Ok(VersionDocument::text("text/xml; charset=utf-8", xml))
    }

    fn registry_type(&self) -> &str {
        "maven"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let (xml, cache_control) = self.fetch_metadata_xml(pkg).await?;

        let latest_version = extract_xml_tag(&xml, "release")
            .or_else(|| extract_xml_tag(&xml, "latest"))
            .unwrap_or_default()
            .to_owned();

        // For specific versions, HEAD the .pom file to get a per-version Last-Modified
        // timestamp (more accurate than the artifact-collection <lastUpdated>).
        // Fall back to <lastUpdated> if the HEAD request fails or returns no date.
        let published_at = if pkg.version != "maven-metadata.xml" {
            self.head_pom_last_modified(pkg)
                .await
                .or_else(|| extract_xml_tag(&xml, "lastUpdated").and_then(parse_last_updated))
        } else {
            extract_xml_tag(&xml, "lastUpdated").and_then(parse_last_updated)
        };

        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({ "latest_version": latest_version }),
            cache_control,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let url = self.artifact_url(pkg)?;

        let resp = self
            .get(&url)
            .send()
            .await
            .map_err(|e| CoreError::Registry(format!("Maven artifact request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "Maven artifact not found: {}",
                pkg.cache_key()
            )));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "Maven artifact request returned {} for {}",
                resp.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&resp);

        let stream = resp.bytes_stream().map_err(to_registry_error);

        Ok(FetchedArtifact {
            stream: Box::pin(stream),
            cache_control,
        })
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let pkg = PackageId::new("", package, "maven-metadata.xml");
        let (xml, _) = self.fetch_metadata_xml(&pkg).await?;

        let mut versions: Vec<String> = extract_all_xml_tags(&xml, "version")
            .into_iter()
            .map(str::to_owned)
            .collect();

        // Maven metadata lists versions in ascending order by convention,
        // but some repositories serve them in arbitrary order; sort them.
        versions.sort();
        Ok(versions)
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        #[derive(serde::Deserialize)]
        struct SearchResponse {
            response: SolrResponse,
        }
        #[derive(serde::Deserialize)]
        struct SolrResponse {
            docs: Vec<SolrDoc>,
        }
        #[derive(serde::Deserialize)]
        struct SolrDoc {
            // "g:a" combined identifier
            id: String,
            #[serde(rename = "latestVersion")]
            latest_version: Option<String>,
        }

        let Some(ref search_base) = self.search_base else {
            return Ok(vec![]);
        };
        let url = format!(
            "{}/solrsearch/select?q={}&rows={}&wt=json",
            search_base,
            percent_encode(query),
            limit.min(50),
        );
        let res = self.get(&url).send().await.map_err(to_registry_error)?;

        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let body: SearchResponse = res.json().await.map_err(to_registry_error)?;

        Ok(body
            .response
            .docs
            .into_iter()
            .map(|d| UpstreamPackage {
                name: d.id,
                latest_version: d.latest_version.unwrap_or_else(|| "unknown".to_string()),
                description: None,
            })
            .collect())
    }
}
