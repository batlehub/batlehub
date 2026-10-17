//! The upstream half of a `helm` registry (RFC 0029 §6.4).
//!
//! ```text
//! fetch_version_document("index.yaml", Versions) → GET {base}/index.yaml, parsed
//! PackageId { name: "alertmanager", version: "1.43.1", artifact: Some("chart") }
//!     → the entry's urls[0], resolved as helm resolves it → its bytes
//! PackageId { …, artifact: Some("prov") } → the same URL + ".prov"
//! ```
//!
//! **The URL comes from the index, never from the request.** A coordinate the
//! upstream index does not list has no URL and is a `404`; an absolute `urls[0]`
//! can name any host, so every fetch goes through
//! [`ssrf::fetch_following_redirects`](super::super::ssrf::fetch_following_redirects)
//! — the registry's credentials only while on the configured origin, the SSRF
//! guard on every hop that leaves it, the `302` GitHub's release assets answer
//! with followed here rather than by the client (§4.4).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::TryStreamExt;
use serde_json::Value;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
    services::helm::{
        chart_entries, find_entry, first_url, is_oci, parse_index, validate_chart_name,
        versions_of, CHART_ARTIFACT, INDEX_ADDRESS, PROV_ARTIFACT,
    },
};

use super::super::http_client::{
    cache_control, no_redirect_client_pair, to_registry_error, UpstreamHttpOptions,
};
use super::super::ssrf;

/// How long one fetched index answers `resolve_metadata` before it is re-read.
/// The *served* index has its own cache in `ProxyService`, keyed by the
/// registry's `metadata_ttl`; this one maps coordinates back to upstream URLs.
const INDEX_TTL: Duration = Duration::from_secs(300);

/// The largest `index.yaml` read. Bitnami's, the largest classic index known
/// to RFC 0029, is 27 MB.
const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;

struct CachedIndex {
    fetched_at: Instant,
    value: Arc<Value>,
}

pub struct HelmRegistryClient {
    credentialed: reqwest::Client,
    plain: reqwest::Client,
    /// The repository directory, as configured — query string included, which
    /// helm carries onto every relative chart URL.
    base: reqwest::Url,
    basic_auth: Option<(String, String)>,
    index: Mutex<Option<CachedIndex>>,
}

/// `ResolveReferenceURL` (`pkg/repo/chartrepo.go`), transcribed.
///
/// An absolute reference is taken as is. A relative one is resolved against
/// the repository URL with its path forced to end in `/`, and the result
/// carries the **base's** query string — the clause a repository behind a
/// signed-URL scheme (`?token=…` on the base) depends on.
pub fn resolve_reference(base: &reqwest::Url, reference: &str) -> Result<reqwest::Url, CoreError> {
    if let Ok(absolute) = reqwest::Url::parse(reference) {
        return Ok(absolute);
    }
    let mut dir = base.clone();
    let path = format!("{}/", dir.path().trim_end_matches('/'));
    dir.set_path(&path);
    let mut resolved = dir.join(reference).map_err(|e| {
        CoreError::Registry(format!("helm: cannot resolve chart URL '{reference}': {e}"))
    })?;
    resolved.set_query(base.query());
    Ok(resolved)
}

impl HelmRegistryClient {
    pub fn new(base_url: &str, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let base = reqwest::Url::parse(base_url).map_err(|e| {
            CoreError::InvalidInput(format!("helm: upstream '{base_url}' is not a URL: {e}"))
        })?;
        let (credentialed, plain) = no_redirect_client_pair(opts)?;
        Ok(Self {
            credentialed,
            plain,
            base,
            basic_auth: opts.basic_auth.clone(),
            index: Mutex::new(None),
        })
    }

    /// `{base}/index.yaml`, query kept — `DownloadIndexFile`'s `path.Join`.
    fn index_url(&self) -> reqwest::Url {
        let mut url = self.base.clone();
        let path = format!("{}/index.yaml", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url
    }

    /// GET with this registry's credentials while on its origin, through the
    /// SSRF guard everywhere else.
    async fn get(&self, url: reqwest::Url) -> Result<reqwest::Response, CoreError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(CoreError::NotFound(format!(
                "helm: '{url}' is not an http(s) URL this registry can fetch"
            )));
        }
        let origin = self.base.origin().ascii_serialization();
        let resp = ssrf::fetch_following_redirects(
            &self.credentialed,
            &self.plain,
            &self.basic_auth,
            &origin,
            url.clone(),
        )
        .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("{url} not found upstream")));
        }
        resp.error_for_status().map_err(to_registry_error)
    }

    /// Upstream's index, parsed.
    async fn fetch_index(&self) -> Result<Value, CoreError> {
        let mut resp = self.get(self.index_url()).await?;
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_INDEX_BYTES as u64)
        {
            return Err(CoreError::Registry(format!(
                "helm: index.yaml is larger than {MAX_INDEX_BYTES} bytes"
            )));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(to_registry_error)? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_INDEX_BYTES {
                return Err(CoreError::Registry(format!(
                    "helm: index.yaml is larger than {MAX_INDEX_BYTES} bytes"
                )));
            }
        }
        let text = String::from_utf8(body)
            .map_err(|_| CoreError::Registry("helm: index.yaml is not UTF-8".to_owned()))?;
        let value = parse_index(&text)?;
        *self.index.lock().expect("index lock") = Some(CachedIndex {
            fetched_at: Instant::now(),
            value: Arc::new(value.clone()),
        });
        Ok(value)
    }

    /// The index from the in-process copy when it is fresh.
    async fn index(&self) -> Result<Arc<Value>, CoreError> {
        if let Some(cached) = self.index.lock().expect("index lock").as_ref() {
            if cached.fetched_at.elapsed() < INDEX_TTL {
                return Ok(Arc::clone(&cached.value));
            }
        }
        self.fetch_index().await.map(Arc::new)
    }
}

#[async_trait]
impl RegistryClient for HelmRegistryClient {
    fn registry_type(&self) -> &str {
        "helm"
    }

    /// The version's entry out of the upstream index. `published_at` is
    /// `created`, which `helm repo index` writes on every entry, so the age gate
    /// never sees an undated chart (§6.7). The checksum is the entry's `digest`
    /// for the archive only — a `.prov` has none of its own.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        validate_chart_name(&pkg.name)?;
        let index = self.index().await?;
        let version = if pkg.version == "latest" {
            batlehub_core::services::blocking::best_latest(&versions_of(&index, &pkg.name))
                .ok_or_else(|| {
                    CoreError::NotFound(format!("chart '{}' is not in the index", pkg.name))
                })?
        } else {
            pkg.version.clone()
        };
        let entry = find_entry(&index, &pkg.name, &version).ok_or_else(|| {
            CoreError::NotFound(format!(
                "chart '{}' version '{version}' is not in the upstream index",
                pkg.name
            ))
        })?;
        // An OCI entry is not this registry's to fetch (RFC 0029 §3): under
        // `drop` it is never served, under `relay` the client pulls it itself.
        if is_oci(entry) {
            return Err(CoreError::NotFound(format!(
                "{} {version} lives in an OCI registry, not in this chart repository",
                pkg.name
            )));
        }
        let reference = first_url(entry).ok_or_else(|| {
            CoreError::NotFound(format!("{} {version} has no URL in the index", pkg.name))
        })?;
        let mut url = resolve_reference(&self.base, reference)?;
        let prov = pkg.artifact.as_deref() == Some(PROV_ARTIFACT);
        if prov {
            // `DownloadTo` fetches `u.String() + ".prov"`.
            url = reqwest::Url::parse(&format!("{url}.prov"))
                .map_err(|e| CoreError::Registry(format!("helm: provenance URL: {e}")))?;
        }
        let checksum = (!prov)
            .then(|| entry.get("digest").and_then(Value::as_str))
            .flatten()
            .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_owned);
        let published_at = entry
            .get("created")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        // The entry minus its upstream URLs, for the console and for an
        // air-gapped instance that composes the index from what it holds.
        let mut facts = entry.clone();
        if let Some(obj) = facts.as_object_mut() {
            obj.remove("urls");
        }
        Ok(PackageMetadata {
            id: PackageId {
                version,
                ..pkg.clone()
            },
            published_at,
            download_url: matches!(
                pkg.artifact.as_deref(),
                Some(CHART_ARTIFACT | PROV_ARTIFACT)
            )
            .then(|| url.to_string()),
            checksum,
            is_signed: None,
            extra: serde_json::json!({ "helm": facts }),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let resolved = self.resolve_metadata(pkg).await?;
        match self.fetch_artifact_resolved(pkg, &resolved).await? {
            Some(fetched) => Ok(fetched),
            None => Err(CoreError::NotFound(format!(
                "{pkg} names no file of the chart; ask for the archive or its provenance"
            ))),
        }
    }

    /// The URL `resolve_metadata` read out of the upstream index, streamed.
    async fn fetch_artifact_resolved(
        &self,
        _pkg: &PackageId,
        resolved: &PackageMetadata,
    ) -> Result<Option<FetchedArtifact>, CoreError> {
        let Some(url) = resolved.download_url.as_deref() else {
            return Ok(None);
        };
        let url = reqwest::Url::parse(url)
            .map_err(|e| CoreError::Registry(format!("helm: chart URL '{url}': {e}")))?;
        let resp = self.get(url).await?;
        let cache_control = cache_control(&resp);
        Ok(Some(FetchedArtifact {
            stream: Box::pin(resp.bytes_stream().map_err(to_registry_error)),
            cache_control,
        }))
    }

    /// The whole index by its address, or — for the console's discovery read,
    /// which names a chart — an index holding that chart alone.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if kind != DocumentKind::Versions {
            return Err(CoreError::NotSupported(format!(
                "a helm repository has no '{kind}' document"
            )));
        }
        if package == INDEX_ADDRESS {
            return Ok(VersionDocument::json(self.fetch_index().await?));
        }
        validate_chart_name(package)?;
        let index = self.index().await?;
        let entries = chart_entries(&index, package);
        if entries.is_empty() {
            return Err(CoreError::NotFound(format!(
                "chart '{package}' is not in the index"
            )));
        }
        Ok(VersionDocument::json(
            serde_json::json!({ "apiVersion": "v1", "entries": { package: entries } }),
        ))
    }

    /// The chart's versions, oldest first as the trait asks.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        validate_chart_name(package)?;
        let index = self.index().await?;
        let mut versions = versions_of(&index, package);
        versions.sort_by(|a, b| batlehub_core::services::newest_first(b, a));
        Ok(versions)
    }
}
