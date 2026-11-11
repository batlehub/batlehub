use crate::registry::encode_path;
use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;
use serde::Deserialize;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, UpstreamPackage, VersionDocument},
};

use super::client::ComposerRegistryClient;
use super::http_client::{fetch_json_document, percent_encode, to_registry_error};

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for ComposerRegistryClient {
    fn registry_type(&self) -> &str {
        "composer"
    }

    /// A package's p2 metadata — the document Composer resolves a constraint
    /// against.
    ///
    /// The `~dev` variant is a *different document* for the same package (it
    /// lists branch aliases rather than tagged releases) and gets its own
    /// `DocumentKind`, so the two do not collide in the metadata cache.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let suffix = match kind {
            DocumentKind::Versions => "",
            DocumentKind::P2_DEV => "~dev",
            other => {
                return Err(CoreError::NotSupported(format!(
                    "composer has no '{other}' listing document"
                )))
            }
        };
        let url = format!("{}/p2/{}{suffix}.json", self.base_url, encode_path(package));
        fetch_json_document(
            self.get(&url),
            &format!("composer p2{suffix} metadata for '{package}'"),
        )
        .await
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        // For p2 artifacts return synthetic metadata pointing to the upstream URL;
        // the actual bytes are fetched lazily by fetch_artifact.
        if let Some(art @ ("p2" | "p2~dev")) = pkg.artifact.as_deref() {
            let dev_suffix = if art == "p2~dev" { "~dev" } else { "" };
            let url = format!(
                "{}/p2/{}{dev_suffix}.json",
                self.base_url,
                encode_path(&pkg.name)
            );
            return Ok(PackageMetadata {
                id: pkg.clone(),
                published_at: None,
                download_url: Some(url),
                checksum: None,
                is_signed: None,
                extra: serde_json::Value::Null,
                cache_control: None,
            });
        }

        let p2 = self.fetch_p2_response(&pkg.name).await?;
        let versions = p2.packages.get(&pkg.name).ok_or_else(|| {
            CoreError::NotFound(format!(
                "composer package '{}' not found in p2 response",
                pkg.name
            ))
        })?;

        let entry = versions
            .iter()
            .find(|v| v.version == pkg.version)
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "composer package '{}@{}' not found",
                    pkg.name, pkg.version
                ))
            })?;

        let download_url = if pkg.artifact.as_deref() == Some("dist") {
            entry.dist.as_ref().map(|d| d.url.clone())
        } else {
            None
        };

        let checksum = entry
            .dist
            .as_ref()
            .and_then(|d| d.shasum.as_deref())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let published_at = entry
            .time
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let extra = serde_json::json!({
            "version": entry.version,
            "dist_url": entry.dist.as_ref().map(|d| &d.url),
            "dist_reference": entry.dist.as_ref().and_then(|d| d.reference.as_deref()),
        });

        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at,
            download_url,
            checksum,
            is_signed: None,
            extra,
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        // For p2 and p2~dev artifacts, stream the raw JSON bytes.
        if let Some(art @ ("p2" | "p2~dev")) = pkg.artifact.as_deref() {
            let suffix = if art == "p2~dev" { "~dev" } else { "" };
            let (bytes, cache_control) = self.fetch_p2_bytes(&pkg.name, suffix).await?;
            let once = futures::stream::once(async move { Ok::<bytes::Bytes, CoreError>(bytes) });
            return Ok(FetchedArtifact {
                stream: Box::pin(once),
                cache_control,
            });
        }

        // For "dist" artifact, resolve and stream from the dist URL.
        let p2 = self.fetch_p2_response(&pkg.name).await?;
        let versions = p2.packages.get(&pkg.name).ok_or_else(|| {
            CoreError::NotFound(format!(
                "composer package '{}' not found in p2 response",
                pkg.name
            ))
        })?;

        let entry = versions
            .iter()
            .find(|v| v.version == pkg.version)
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "composer package '{}@{}' not found",
                    pkg.name, pkg.version
                ))
            })?;

        let dist_url = entry.dist.as_ref().map(|d| d.url.clone()).ok_or_else(|| {
            CoreError::NotFound(format!(
                "no dist URL for composer package '{}@{}'",
                pkg.name, pkg.version
            ))
        })?;

        tracing::debug!(url = %dist_url, "fetching composer dist artifact");

        let response = self
            .get(&dist_url)
            .send()
            .await
            .map_err(to_registry_error)?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "composer dist artifact not found: {}@{}",
                pkg.name, pkg.version
            )));
        }
        if !response.status().is_success() {
            return Err(CoreError::Registry(format!(
                "composer dist upstream returned {} for {}@{}",
                response.status(),
                pkg.name,
                pkg.version
            )));
        }

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

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let p2 = self.fetch_p2_response(package).await?;
        let versions = p2
            .packages
            .get(package)
            .map(|entries| entries.iter().map(|e| e.version.clone()).collect())
            .unwrap_or_default();
        Ok(versions)
    }

    async fn search_packages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<UpstreamPackage>, CoreError> {
        #[derive(Deserialize)]
        struct SearchResponse {
            results: Vec<SearchResult>,
        }
        #[derive(Deserialize)]
        struct SearchResult {
            name: String,
            description: Option<String>,
        }

        let Some(ref search_base) = self.search_base else {
            return Ok(vec![]);
        };
        let url = format!(
            "{}/search.json?q={}&per_page={}",
            search_base,
            percent_encode(query),
            limit.min(50),
        );
        let res = self.get(&url).send().await.map_err(to_registry_error)?;

        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let body: SearchResponse = res.json().await.map_err(to_registry_error)?;

        // Packagist search doesn't include the latest version; use "latest" as a
        // placeholder — the proxy resolves the real version on first access.
        Ok(body
            .results
            .into_iter()
            .map(|r| UpstreamPackage {
                name: r.name,
                latest_version: "latest".to_string(),
                description: r.description,
            })
            .collect())
    }
}
