use async_trait::async_trait;
use chrono::DateTime;
use futures::TryStreamExt;
use serde::Deserialize;

use batlehub_core::{
    entities::{MetadataLinks, PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, UpstreamPackage, VersionDocument},
};

use super::http_client::{
    basic_auth_get, cache_control, encode_path, fetch_text_document, new_http_client,
    percent_encode, to_registry_error, UpstreamHttpOptions,
};

/// crates.io (or compatible) registry client.
///
/// Supported `PackageId` conventions:
/// - `version = "latest"`   → resolve via crates.io API (max_version)
/// - `version = "1.2.3"`    → specific version metadata
/// - `artifact = Some("dl")` → stream the `.crate` file for that version
pub struct CargoRegistryClient {
    http: reqwest::Client,
    base_url: String,
    /// Sparse-index base (`https://index.crates.io`), which is a *different*
    /// host from the API `base_url` (`https://crates.io`). Carried here so the
    /// index can be served through `ProxyService` — authorised, audited and
    /// filtered — rather than by a bare GET beside it.
    index_url: Option<String>,
    basic_auth: Option<(String, String)>,
}

impl CargoRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        let http = new_http_client(Some(10), opts)?;
        Ok(Self {
            http,
            base_url: base_url.into(),
            index_url: None,
            basic_auth: opts.basic_auth.clone(),
        })
    }

    /// Point this client at the registry's sparse index.
    ///
    /// Separate from `new` because the index is configured independently of the
    /// API upstream (`index_url` in `[[registries]]`, defaulted per upstream),
    /// and a cargo registry in local-only mode has no index to proxy at all.
    pub fn with_index_url(mut self, index_url: impl Into<String>) -> Self {
        self.index_url = Some(index_url.into());
        self
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        basic_auth_get(&self.http, &self.basic_auth, url)
    }
}

/// The sparse index path for a crate name, per cargo's own layout.
///
/// `1/a`, `2/ab`, `3/a/abc`, then `ab/cd/abcdef`. Lower-cased, because the
/// index is stored that way and cargo lower-cases before looking a crate up.
pub fn sparse_index_path(name: &str) -> String {
    let lower = name.to_lowercase();
    match lower.chars().count() {
        0 => lower,
        1 => format!("1/{lower}"),
        2 => format!("2/{lower}"),
        3 => format!("3/{}/{lower}", &lower[..1]),
        _ => format!("{}/{}/{lower}", &lower[..2], &lower[2..4]),
    }
}

// ── Serde types ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    krate: CrateInfo,
    versions: Vec<CrateVersion>,
}

#[derive(Debug, Deserialize)]
struct CrateInfo {
    max_version: String,
    /// crates.io carries these on the crate, not the version: `Cargo.toml`'s
    /// `[package] repository` and `homepage` as of the most recent publish.
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CrateVersion {
    num: String,
    #[serde(rename = "dl_path")]
    dl_path: String,
    checksum: Option<String>,
    created_at: Option<String>,
    yanked: bool,
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for CargoRegistryClient {
    fn registry_type(&self) -> &str {
        "cargo"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let resp = self.fetch_crate_info(&pkg.name).await?;
        let version = Self::resolve_version(&resp, pkg)?;
        let resolved_version = version.num.clone();

        let download_url = if pkg.artifact.as_deref() == Some("dl") {
            // dl_path is a relative path like /api/v1/crates/serde/1.0.0/download
            Some(format!("{}{}", self.base_url, version.dl_path))
        } else {
            None
        };

        let published_at = version
            .created_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        let extra = serde_json::json!({
            "resolved_version": resolved_version,
            "dl_path": version.dl_path,
            "yanked": version.yanked,
            "links": MetadataLinks::new(
                resp.krate.repository.as_deref(),
                resp.krate.homepage.as_deref(),
            ),
        });

        Ok(PackageMetadata {
            id: PackageId {
                version: resolved_version,
                ..pkg.clone()
            },
            published_at,
            download_url,
            checksum: version.checksum.clone(),
            is_signed: None,
            extra,
            cache_control: None,
        })
    }

    /// The sparse index entry for one crate: newline-delimited JSON, one line
    /// per version.
    ///
    /// This is the document `cargo` resolves every dependency against, and
    /// routing it here is what puts it behind the rule chain, the audit trail
    /// and the metadata cache — none of which it went through when the handler
    /// fetched it with a bare `reqwest` GET.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if kind != DocumentKind::Versions {
            return Err(CoreError::NotSupported(format!(
                "cargo has no '{kind}' listing document"
            )));
        }
        let Some(index) = self.index_url.as_deref() else {
            return Err(CoreError::NotSupported(
                "this cargo registry has no sparse index configured".to_owned(),
            ));
        };
        let url = format!(
            "{}/{}",
            index.trim_end_matches('/'),
            encode_path(&sparse_index_path(package))
        );
        fetch_text_document(
            self.get(&url),
            &format!("cargo sparse index for '{package}'"),
            "text/plain; charset=utf-8",
        )
        .await
    }

    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let resp = self.fetch_crate_info(package).await?;
        // Return non-yanked versions sorted oldest-first (crates.io returns newest-first).
        let mut versions: Vec<String> = resp
            .versions
            .into_iter()
            .filter(|v| !v.yanked)
            .map(|v| v.num)
            .collect();
        versions.reverse();
        Ok(versions)
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let resp = self.fetch_crate_info(&pkg.name).await?;
        let version = Self::resolve_version(&resp, pkg)?;
        let url = format!("{}{}", self.base_url, version.dl_path);
        tracing::debug!(url = %url, "fetching cargo crate");

        let response = self
            .get(&url)
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
            crates: Vec<SearchCrate>,
        }
        #[derive(Deserialize)]
        struct SearchCrate {
            name: String,
            max_version: String,
            description: Option<String>,
        }

        let url = format!(
            "{}/api/v1/crates?q={}&per_page={}",
            self.base_url,
            percent_encode(query),
            limit.min(100),
        );
        let res = self.get(&url).send().await.map_err(to_registry_error)?;

        if !res.status().is_success() {
            return Ok(vec![]);
        }

        let body: SearchResponse = res.json().await.map_err(to_registry_error)?;

        Ok(body
            .crates
            .into_iter()
            .map(|c| UpstreamPackage {
                name: c.name,
                latest_version: c.max_version,
                description: c.description,
            })
            .collect())
    }
}

impl CargoRegistryClient {
    fn resolve_version<'a>(
        resp: &'a CratesIoResponse,
        pkg: &PackageId,
    ) -> Result<&'a CrateVersion, CoreError> {
        let resolved = if pkg.version == "latest" {
            resp.krate.max_version.as_str()
        } else {
            pkg.version.as_str()
        };
        resp.versions
            .iter()
            .find(|v| v.num == resolved && !v.yanked)
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "crate {}@{} not found or yanked",
                    pkg.name, resolved
                ))
            })
    }

    async fn fetch_crate_info(&self, name: &str) -> Result<CratesIoResponse, CoreError> {
        let url = format!("{}/api/v1/crates/{}", self.base_url, encode_path(name));
        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!("crate {name} not found")));
        }

        resp.error_for_status()
            .map_err(to_registry_error)?
            .json::<CratesIoResponse>()
            .await
            .map_err(to_registry_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(max: &str, versions: &[(&str, bool)]) -> CratesIoResponse {
        CratesIoResponse {
            krate: CrateInfo {
                max_version: max.to_string(),
                repository: None,
                homepage: None,
            },
            versions: versions
                .iter()
                .map(|(num, yanked)| CrateVersion {
                    num: num.to_string(),
                    dl_path: format!("/api/v1/crates/test/{}/download", num),
                    checksum: None,
                    created_at: None,
                    yanked: *yanked,
                })
                .collect(),
        }
    }

    #[test]
    fn resolve_exact_version() {
        let r = resp("1.0.1", &[("1.0.1", false), ("1.0.0", false)]);
        let pkg = PackageId::new("cargo", "serde", "1.0.0");
        assert_eq!(
            CargoRegistryClient::resolve_version(&r, &pkg).unwrap().num,
            "1.0.0"
        );
    }

    #[test]
    fn resolve_latest_uses_max_version() {
        let r = resp("1.5.0", &[("1.5.0", false), ("1.0.0", false)]);
        let pkg = PackageId::new("cargo", "serde", "latest");
        assert_eq!(
            CargoRegistryClient::resolve_version(&r, &pkg).unwrap().num,
            "1.5.0"
        );
    }

    #[test]
    fn resolve_yanked_version_returns_not_found() {
        let r = resp("1.0.0", &[("1.0.0", true)]);
        let pkg = PackageId::new("cargo", "serde", "1.0.0");
        assert!(matches!(
            CargoRegistryClient::resolve_version(&r, &pkg),
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn resolve_missing_version_returns_not_found() {
        let r = resp("2.0.0", &[("2.0.0", false)]);
        let pkg = PackageId::new("cargo", "serde", "1.0.0");
        assert!(matches!(
            CargoRegistryClient::resolve_version(&r, &pkg),
            Err(CoreError::NotFound(_))
        ));
    }
}
