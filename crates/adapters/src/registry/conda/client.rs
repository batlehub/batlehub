use async_trait::async_trait;
use futures::TryStreamExt;

use super::super::http_client::{cache_control, fetch_json_document, to_registry_error};
use super::{models, CondaRegistryClient};
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
};
use models::{CondaIndexJson, CondaPackageInfo, CondaRepodata};

impl CondaRegistryClient {
    /// Fetch one platform's `repodata.json` and return all versions of `package` found in it.
    /// Returns an empty `Vec` on any network/parse error (fail-open for version listing).
    pub(super) async fn fetch_platform_versions(
        &self,
        base: &str,
        platform: &str,
        package: &str,
    ) -> Vec<String> {
        let url = format!("{base}/{platform}/repodata.json");
        let resp = match self.get(&url).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => return vec![],
        };
        let body = match resp.bytes().await {
            Ok(b) => b,
            Err(_) => return vec![],
        };
        let repodata: CondaRepodata = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        repodata
            .packages
            .values()
            .chain(repodata.packages_conda.values())
            .filter(|e| e.name.as_deref() == Some(package))
            .filter_map(|e| e.version.clone())
            .collect()
    }

    /// Look up a specific conda file in `{platform}/repodata.json`.
    pub(super) async fn lookup_file_in_repodata(
        &self,
        base: &str,
        platform: &str,
        filename: &str,
        pkg: &PackageId,
    ) -> Result<PackageMetadata, CoreError> {
        let repodata_url = format!("{base}/{platform}/repodata.json");
        let resp = self
            .get(&repodata_url)
            .send()
            .await
            .map_err(|e| CoreError::Registry(format!("conda: repodata request failed: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "conda repodata not found for platform '{platform}'"
            )));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "conda upstream returned {} fetching repodata",
                resp.status()
            )));
        }
        let cache_control = cache_control(&resp);
        let body = resp.bytes().await.map_err(to_registry_error)?;
        let repodata: CondaRepodata = serde_json::from_slice(&body)
            .map_err(|e| CoreError::Registry(format!("conda: parse repodata: {e}")))?;
        let entry = repodata
            .packages
            .get(filename)
            .or_else(|| repodata.packages_conda.get(filename));
        let entry = entry.ok_or_else(|| {
            CoreError::NotFound(format!(
                "conda: '{filename}' not found in {platform}/repodata.json"
            ))
        })?;
        let published_at = entry.timestamp.and_then(|ms| {
            chrono::DateTime::from_timestamp(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
        });
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at,
            download_url: Some(format!("{base}/{platform}/{filename}")),
            checksum: entry.sha256.clone(),
            is_signed: None,
            extra: serde_json::json!({
                "name": entry.name,
                "version": entry.version,
                "build": entry.build,
            }),
            cache_control,
        })
    }
}

// ── RegistryClient impl ───────────────────────────────────────────────────────

#[async_trait]
impl RegistryClient for CondaRegistryClient {
    fn registry_type(&self) -> &str {
        "conda"
    }

    /// A channel's `repodata.json` (or `current_repodata.json`) for one
    /// platform.
    ///
    /// The `package` argument carries the **platform** here — `linux-64`,
    /// `noarch` — because a conda listing is scoped to a subdir rather than to a
    /// package. That is also why this document goes through
    /// `ProxyService::multi_package_document`: it describes the whole channel.
    async fn fetch_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        let filename = match kind {
            DocumentKind::Versions => "repodata.json",
            DocumentKind::CURRENT_REPODATA => "current_repodata.json",
            // Channel-root rather than per-subdir: `channeldata.json` describes
            // every platform at once, so it takes no `{package}` path segment
            // (RFC 0009 §7.5).
            DocumentKind::CHANNELDATA => {
                let base = self.base_url.trim_end_matches('/');
                return fetch_json_document(
                    self.get(&format!("{base}/channeldata.json")),
                    "conda channeldata.json",
                )
                .await;
            }
            other => {
                return Err(CoreError::NotSupported(format!(
                    "conda has no '{other}' listing document"
                )))
            }
        };
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}/{package}/{filename}");
        fetch_json_document(
            self.get(&url),
            &format!("conda {filename} for platform '{package}'"),
        )
        .await
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let (platform, filename) = super::platform_and_file(pkg);

        // For specific package files, look them up in repodata.json.
        if pkg.name != "repodata" {
            if let Some(filename) = filename {
                return self
                    .lookup_file_in_repodata(base, platform, filename, pkg)
                    .await;
            }
        }

        // For repodata.json itself, return the URL as the download URL.
        let url = self.artifact_url(pkg);
        let resp = self
            .get(&url)
            .send()
            .await
            .map_err(|e| CoreError::Registry(format!("conda metadata request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "conda resource not found: {}",
                pkg.cache_key()
            )));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "conda upstream returned {} for {}",
                resp.status(),
                pkg.cache_key()
            )));
        }

        let cache_control = cache_control(&resp);

        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: None,
            download_url: Some(url),
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let url = self.artifact_url(pkg);

        tracing::debug!(url = %url, "fetching conda artifact");

        let resp = self.get(&url).send().await.map_err(to_registry_error)?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CoreError::NotFound(format!(
                "conda artifact not found: {}",
                pkg.cache_key()
            )));
        }
        if !resp.status().is_success() {
            return Err(CoreError::Registry(format!(
                "conda upstream returned {} for {}",
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

    /// Synthesise a version list by scanning `repodata.json` for each of the
    /// configured `list_platforms`.  Platforms that return a 404 or network
    /// error are silently skipped so a missing platform never blocks warming.
    /// Versions are collected into a sorted, deduplicated list.
    async fn list_versions(&self, package: &str) -> Result<Vec<String>, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let mut versions: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for platform in &self.list_platforms {
            let platform_versions = self.fetch_platform_versions(base, platform, package).await;
            versions.extend(platform_versions);
        }

        Ok(versions.into_iter().collect())
    }
}

// ── Local publish helpers ─────────────────────────────────────────────────────

/// Parse a conda package (`.tar.bz2` or `.conda`) and extract `info/index.json`.
///
/// Supports:
/// - `.tar.bz2`: bzip2-compressed tar archive directly containing `info/index.json`
/// - `.conda`: ZIP archive containing `info-*.tar.zst` (zstd-compressed tar with `info/index.json`)
#[cfg(feature = "local-registry")]
pub fn parse_conda_metadata(data: &[u8]) -> Result<CondaPackageInfo, CoreError> {
    if is_zip(data) {
        parse_conda_format(data)
    } else {
        parse_tar_bz2(data)
    }
}

fn is_zip(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == b"PK\x03\x04"
}

#[cfg(feature = "local-registry")]
fn parse_tar_bz2(data: &[u8]) -> Result<CondaPackageInfo, CoreError> {
    use bzip2::read::BzDecoder;
    use std::io::Cursor;

    let cursor = Cursor::new(data);
    let bz = BzDecoder::new(cursor);
    let mut archive = tar::Archive::new(bz);

    let index_bytes = find_in_tar(&mut archive, "info/index.json")
        .map_err(|e| CoreError::Registry(format!("conda: read .tar.bz2 archive: {e}")))?;

    parse_index_json(&index_bytes)
}

#[cfg(feature = "local-registry")]
fn parse_conda_format(data: &[u8]) -> Result<CondaPackageInfo, CoreError> {
    use std::io::{Cursor, Read};
    use zip::ZipArchive;

    let cursor = Cursor::new(data);
    let mut zip = ZipArchive::new(cursor)
        .map_err(|e| CoreError::Registry(format!("conda: open .conda ZIP: {e}")))?;

    // Find the info-*.tar.zst member (collect names first to avoid borrow issues)
    let mut info_entry_name: Option<String> = None;
    for i in 0..zip.len() {
        if let Ok(f) = zip.by_index(i) {
            let name = f.name().to_owned();
            if name.starts_with("info-") && name.ends_with(".tar.zst") {
                info_entry_name = Some(name);
                break;
            }
        }
    }
    let info_entry_name = info_entry_name
        .ok_or_else(|| CoreError::Registry("conda: info-*.tar.zst not found in .conda".into()))?;

    let mut entry = zip
        .by_name(&info_entry_name)
        .map_err(|e| CoreError::Registry(format!("conda: open {info_entry_name}: {e}")))?;

    // The `.conda` archive is publisher-supplied; bound the compressed info
    // member so a zip bomb cannot OOM the process before we even decompress it.
    const MAX_INFO_COMPRESSED: u64 = 32 * 1024 * 1024; // 32 MiB
    let mut zst_bytes = Vec::new();
    Read::take(&mut entry, MAX_INFO_COMPRESSED)
        .read_to_end(&mut zst_bytes)
        .map_err(|e| CoreError::Registry(format!("conda: read {info_entry_name}: {e}")))?;
    if zst_bytes.len() as u64 >= MAX_INFO_COMPRESSED {
        return Err(CoreError::Registry(format!(
            "conda: {info_entry_name} exceeds the maximum allowed size"
        )));
    }

    let decoder = zstd::Decoder::new(zst_bytes.as_slice())
        .map_err(|e| CoreError::Registry(format!("conda: zstd decoder: {e}")))?;
    let mut archive = tar::Archive::new(decoder);

    let index_bytes = find_in_tar(&mut archive, "info/index.json")
        .map_err(|e| CoreError::Registry(format!("conda: read info tar: {e}")))?;

    parse_index_json(&index_bytes)
}

#[cfg(feature = "local-registry")]
fn find_in_tar<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    target: &str,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    // `archive` wraps a decompressor (bzip2 / zstd) over publisher-supplied
    // bytes, so the extracted entry could be a decompression bomb. Cap the
    // read so a small archive expanding to many GB cannot OOM the process;
    // the entries we look for (`info/index.json`) are tiny in practice.
    const MAX_ENTRY_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB
    for entry in archive.entries()? {
        let mut entry = entry?;
        let matches = entry
            .path()
            .map(|p| p.as_os_str() == target || p.to_str() == Some(target))
            .unwrap_or(false);
        if matches {
            let mut buf = Vec::new();
            Read::take(&mut entry, MAX_ENTRY_BYTES).read_to_end(&mut buf)?;
            if buf.len() as u64 >= MAX_ENTRY_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{target} exceeds the maximum allowed size"),
                ));
            }
            return Ok(buf);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{target} not found in archive"),
    ))
}

pub(super) fn parse_index_json(bytes: &[u8]) -> Result<CondaPackageInfo, CoreError> {
    let idx: CondaIndexJson = serde_json::from_slice(bytes)
        .map_err(|e| CoreError::Registry(format!("conda: parse info/index.json: {e}")))?;
    Ok(CondaPackageInfo {
        name: idx.name,
        version: idx.version,
        build: idx.build,
        build_number: idx.build_number,
        depends: idx.depends,
        subdir: idx.subdir,
        license: idx.license,
    })
}
