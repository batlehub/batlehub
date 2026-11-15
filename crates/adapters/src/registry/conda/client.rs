use crate::registry::encode_path;
use async_trait::async_trait;
use futures::TryStreamExt;

use super::super::http_client::{cache_control, fetch_json_document, to_registry_error};
use super::{models, CachedShardIndex, CondaRegistryClient, ShardIndex};
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{
        DocumentEncoding, DocumentKind, DocumentProbe, FetchedArtifact, RegistryClient,
        StreamedDocument, VersionDocument,
    },
};
use models::{
    CondaIndexJson, CondaPackageEntry, CondaPackageInfo, CondaRepodata, CondaShardIndexDoc,
};
use std::sync::Arc;
use std::time::Instant;

impl CondaRegistryClient {
    /// Fetch one platform's `repodata.json` and return all versions of `package` found in it.
    /// Returns an empty `Vec` on any network/parse error (fail-open for version listing).
    pub(super) async fn fetch_platform_versions(
        &self,
        base: &str,
        platform: &str,
        package: &str,
    ) -> Vec<String> {
        let url = format!("{base}/{}/repodata.json", encode_path(platform));
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

    /// Look up one conda file: in its package's CEP-16 shard when the channel
    /// publishes shards, otherwise in `{platform}/repodata.json`.
    ///
    /// This runs on every proxied package download, and conda clients that
    /// fetch files directly (mise drives rattler in-process) never warm the
    /// index for us: reading `conda-forge/linux-64/repodata.json` here cost
    /// seconds and gigabytes per file and timed mise out. A shard is a few KB.
    pub(super) async fn lookup_file(
        &self,
        base: &str,
        platform: &str,
        filename: &str,
        pkg: &PackageId,
    ) -> Result<PackageMetadata, CoreError> {
        let (entry, cache_control) = match self.shard_index(base, platform).await {
            Some(index) => (
                self.lookup_in_shard(&index, filename).await?,
                index.cache_control.clone(),
            ),
            None => self.lookup_in_repodata(base, platform, filename).await?,
        };
        let entry = entry.ok_or_else(|| {
            CoreError::NotFound(format!("conda: '{filename}' not found in {platform}"))
        })?;
        let published_at = entry.timestamp.and_then(|ms| {
            chrono::DateTime::from_timestamp(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
        });
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at,
            download_url: Some(format!(
                "{base}/{}/{}",
                encode_path(platform),
                encode_path(filename)
            )),
            checksum: entry.sha256,
            is_signed: None,
            extra: serde_json::json!({
                "name": entry.name,
                "version": entry.version,
                "build": entry.build,
            }),
            cache_control,
        })
    }

    /// The subdir's shard index, from cache while fresh; `None` when the
    /// channel publishes none (cached) or it could not be read (not cached, so
    /// a transient failure does not pin the expensive fallback for a TTL).
    async fn shard_index(&self, base: &str, platform: &str) -> Option<Arc<ShardIndex>> {
        if let Ok(guard) = self.shard_indexes.lock() {
            if let Some(hit) = guard.get(platform) {
                if hit.read_at.elapsed() <= self.shard_index_ttl {
                    return hit.index.clone();
                }
            }
        }
        let url = format!(
            "{base}/{}/repodata_shards.msgpack.zst",
            encode_path(platform)
        );
        let index = match self.fetch_shard_index(&url).await {
            Ok(index) => index,
            Err(e) => {
                tracing::warn!(url = %url, error = %e, "conda: shard index unreadable, falling back to repodata.json");
                return None;
            }
        };
        if let Ok(mut guard) = self.shard_indexes.lock() {
            // The subdir comes from the request path; bound what it can grow.
            if guard.len() >= MAX_CACHED_SHARD_INDEXES && !guard.contains_key(platform) {
                if let Some(oldest) = guard
                    .iter()
                    .min_by_key(|(_, v)| v.read_at)
                    .map(|(k, _)| k.clone())
                {
                    guard.remove(&oldest);
                }
            }
            guard.insert(
                platform.to_owned(),
                CachedShardIndex {
                    read_at: Instant::now(),
                    index: index.clone(),
                },
            );
        }
        index
    }

    async fn fetch_shard_index(&self, url: &str) -> Result<Option<Arc<ShardIndex>>, CoreError> {
        let resp = self.get(url).send().await.map_err(to_registry_error)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status().map_err(to_registry_error)?;
        let cache_control = cache_control(&resp);
        let body = resp.bytes().await.map_err(to_registry_error)?;
        let doc: CondaShardIndexDoc = decode_msgpack_zst(body).await?;
        let bad = |e: <reqwest::Url as std::str::FromStr>::Err| {
            CoreError::Registry(format!("conda: shard index URL: {e}"))
        };
        let index_url = reqwest::Url::parse(url).map_err(bad)?;
        let shards_base = index_url
            .join(doc.info.shards_base_url.as_deref().unwrap_or(""))
            .map_err(bad)?;
        // An absolute `shards_base_url` elsewhere would get this registry's credential.
        if shards_base.origin() != index_url.origin() {
            return Err(CoreError::Registry(format!(
                "conda: shards_base_url points off the channel: {shards_base}"
            )));
        }
        Ok(Some(Arc::new(ShardIndex {
            shards: doc.shards.into_iter().map(|(k, v)| (k, v.0)).collect(),
            shards_base,
            cache_control,
        })))
    }

    async fn lookup_in_shard(
        &self,
        index: &ShardIndex,
        filename: &str,
    ) -> Result<Option<CondaPackageEntry>, CoreError> {
        let Some(digest) = package_name_candidates(filename).find_map(|n| index.shards.get(n))
        else {
            return Ok(None);
        };
        // The digest is joined into a URL; anything but hex could steer it.
        if digest.is_empty() || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CoreError::Registry(format!(
                "conda: shard digest is not hex: {digest}"
            )));
        }
        let url = index
            .shards_base
            .join(&format!("{digest}.msgpack.zst"))
            .map_err(|e| CoreError::Registry(format!("conda: shard URL: {e}")))?;
        let resp = self
            .get(url.as_str())
            .send()
            .await
            .map_err(to_registry_error)?
            .error_for_status()
            .map_err(to_registry_error)?;
        let body = resp.bytes().await.map_err(to_registry_error)?;
        let mut shard: CondaRepodata = decode_msgpack_zst(body).await?;
        Ok(shard
            .packages
            .remove(filename)
            .or_else(|| shard.packages_conda.remove(filename)))
    }

    /// The fallback for a channel without shards: `repodata.json` (its `.zst`
    /// when published), with only the wanted entry ever deserialized.
    async fn lookup_in_repodata(
        &self,
        base: &str,
        platform: &str,
        filename: &str,
    ) -> Result<(Option<CondaPackageEntry>, Option<String>), CoreError> {
        let repodata_url = format!("{base}/{}/repodata.json", encode_path(platform));
        let zst = self
            .get(&format!("{repodata_url}.zst"))
            .send()
            .await
            .ok()
            .filter(|r| r.status().is_success());
        let compressed = zst.is_some();
        let resp =
            match zst {
                Some(resp) => resp,
                None => self.get(&repodata_url).send().await.map_err(|e| {
                    CoreError::Registry(format!("conda: repodata request failed: {e}"))
                })?,
            };
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
        let filename = filename.to_owned();
        let entry = tokio::task::spawn_blocking(move || {
            let body = std::io::Cursor::new(body);
            let reader: Box<dyn std::io::Read> = if compressed {
                Box::new(zstd::Decoder::new(body).map_err(|e| e.to_string())?)
            } else {
                Box::new(body)
            };
            find_in_repodata(std::io::BufReader::new(reader), &filename).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| CoreError::Registry(format!("conda: repodata lookup task: {e}")))?
        .map_err(|e| CoreError::Registry(format!("conda: parse repodata: {e}")))?;
        Ok((entry, cache_control))
    }
}

/// How many subdirs' shard indexes are held at once. A channel has about ten.
const MAX_CACHED_SHARD_INDEXES: usize = 32;

/// Past this a decompressed shard or shard index is not one (conda-forge's
/// whole `linux-64` index is ~1.5 MB).
const MAX_DECODED_SHARD_BYTES: u64 = 64 * 1024 * 1024;

/// The package names `filename` could belong to, longest first.
///
/// Matched against the shard index's keys rather than taken from a split,
/// because a name may contain `-` (`ruamel-yaml-0.17.21-py311_0.conda`).
fn package_name_candidates(filename: &str) -> impl Iterator<Item = &str> {
    filename
        .rmatch_indices('-')
        .map(move |(i, _)| &filename[..i])
}

/// Decompress and decode a msgpack+zstd document off the async workers.
async fn decode_msgpack_zst<T: serde::de::DeserializeOwned + Send + 'static>(
    body: bytes::Bytes,
) -> Result<T, CoreError> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let reader = zstd::Decoder::new(std::io::Cursor::new(body))
            .map_err(|e| e.to_string())?
            .take(MAX_DECODED_SHARD_BYTES);
        rmp_serde::from_read(reader).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| CoreError::Registry(format!("conda: shard decode task: {e}")))?
    .map_err(|e: String| CoreError::Registry(format!("conda: decode shard: {e}")))
}

/// The entry for `filename` in a `repodata.json`, every other entry skipped
/// as it streams past, so memory is one entry rather than the document.
fn find_in_repodata<R: std::io::Read>(
    reader: R,
    filename: &str,
) -> Result<Option<CondaPackageEntry>, serde_json::Error> {
    use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};

    struct Doc<'a>(&'a str);
    struct Packages<'a>(&'a str);

    impl<'de> DeserializeSeed<'de> for Doc<'_> {
        type Value = Option<CondaPackageEntry>;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_map(self)
        }
    }
    impl<'de> Visitor<'de> for Doc<'_> {
        type Value = Option<CondaPackageEntry>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a repodata.json object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut found = None;
            while let Some(key) = map.next_key::<String>()? {
                if key == "packages" || key == "packages.conda" {
                    let hit = map.next_value_seed(Packages(self.0))?;
                    found = found.or(hit);
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(found)
        }
    }
    impl<'de> DeserializeSeed<'de> for Packages<'_> {
        type Value = Option<CondaPackageEntry>;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_map(self)
        }
    }
    impl<'de> Visitor<'de> for Packages<'_> {
        type Value = Option<CondaPackageEntry>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a map of filename to package entry")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut found = None;
            while let Some(key) = map.next_key::<String>()? {
                if found.is_none() && key == self.0 {
                    found = Some(map.next_value::<CondaPackageEntry>()?);
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(found)
        }
    }

    Doc(filename).deserialize(&mut serde_json::Deserializer::from_reader(reader))
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
    /// Which index encodings this channel publishes, asked with `HEAD`.
    ///
    /// conda clients probe before they fetch — micromamba sends a `HEAD` for
    /// every subdir and encoding it might use — and answering those by pulling
    /// the body and discarding it is, for `conda-forge/linux-64`, 57 MiB per
    /// probe. This asks upstream the same question the client asked.
    async fn probe_version_document(
        &self,
        package: &str,
        kind: DocumentKind,
        accept: &[DocumentEncoding],
    ) -> Result<Option<DocumentProbe>, CoreError> {
        let Some((url_base, _)) = self.index_url(package, kind) else {
            return Ok(None);
        };

        for encoding in accept {
            let url = format!("{url_base}{}", encoding.suffix());
            let resp = self
                .head(&url)
                .send()
                .await
                .map_err(super::super::http_client::to_registry_error)?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                continue;
            }
            // **Any other failure gives up probing, it does not fail the
            // request.** A probe is an optimisation: the caller's fallback is
            // the body path, which worked before this method existed. Plenty
            // of CDNs answer `405` to a `HEAD`, and S3-style backends answer
            // `403` for a missing key, so propagating here turns a channel
            // that serves fine over `GET` into a 502 for every conda and
            // micromamba probe.
            if !resp.status().is_success() {
                tracing::debug!(
                    url = %url,
                    status = %resp.status(),
                    "conda: upstream refused the index probe; falling back to the body path"
                );
                return Ok(None);
            }
            let header = |name: reqwest::header::HeaderName| {
                resp.headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            };
            return Ok(Some(DocumentProbe {
                encoding: *encoding,
                // **The header, not `content_length()`.** On a `HEAD` response
                // `reqwest` reports the *body* size hint, and hyper hard-codes
                // that to zero for `HEAD` regardless of what the server
                // advertised — so this read `Some(0)` for every probe, and
                // micromamba was told a 55 MiB index was empty. The service's
                // own note says a probe that promises the wrong length is
                // worse than one that promises none.
                content_length: header(reqwest::header::CONTENT_LENGTH)
                    .and_then(|v| v.trim().parse::<u64>().ok()),
                etag: header(reqwest::header::ETAG),
                last_modified: header(reqwest::header::LAST_MODIFIED),
            }));
        }
        Ok(None)
    }

    /// The channel index as bytes, in the first encoding the caller accepts that
    /// the channel actually publishes.
    ///
    /// This is the path that makes a conda channel affordable. `repodata.json`
    /// for `conda-forge/linux-64` is 424 MiB and parsing it costs several GB;
    /// its `.zst` is 55 MiB and, when there is nothing to filter out of it,
    /// neither the proxy nor anyone else has any reason to look inside.
    ///
    /// A `404` on one encoding is not an error — a channel need not publish all
    /// three — so the next one is tried, and exhausting the list answers `None`,
    /// which puts the caller back on the parsed path.
    async fn fetch_version_document_stream(
        &self,
        package: &str,
        kind: DocumentKind,
        accept: &[DocumentEncoding],
    ) -> Result<Option<StreamedDocument>, CoreError> {
        let Some((url_base, what)) = self.index_url(package, kind) else {
            // Anything else has no byte path; the parsed one answers it.
            return Ok(None);
        };

        for encoding in accept {
            let url = format!("{url_base}{}", encoding.suffix());
            let resp = self.get(&url).send().await.map_err(to_registry_error)?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                continue;
            }
            let resp = resp.error_for_status().map_err(to_registry_error)?;
            tracing::debug!(url = %url, what, "streaming conda index");
            let cache_control = cache_control(&resp);
            return Ok(Some(StreamedDocument {
                stream: Box::pin(resp.bytes_stream().map_err(to_registry_error)),
                encoding: *encoding,
                cache_control,
            }));
        }
        Ok(None)
    }

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
        let url = format!("{base}/{}/{filename}", encode_path(package));
        fetch_json_document(
            self.get(&url),
            &format!("conda {filename} for platform '{package}'"),
        )
        .await
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        let base = self.base_url.trim_end_matches('/');
        let (platform, filename) = super::platform_and_file(pkg);

        // A CEP-16 shard is **content-addressed**: the coordinate is the digest,
        // the bytes under it never change, and there is nothing about it to look
        // up. Looking one up in `repodata.json` would parse the 424 MiB document
        // that sharding exists to avoid — once per shard, which is the opposite
        // of the point.
        if filename.is_some_and(|f| f.ends_with(".msgpack.zst")) {
            return Ok(PackageMetadata {
                id: pkg.clone(),
                published_at: None,
                download_url: Some(self.artifact_url(pkg)),
                checksum: None,
                is_signed: None,
                extra: serde_json::Value::Null,
                // Immutable by construction, so the cache never has to ask
                // again. The upstream says so too, but this does not depend on
                // it saying so.
                cache_control: Some("public, max-age=31536000, immutable".to_owned()),
            });
        }

        // For specific package files, look up their checksum and date.
        if pkg.name != "repodata" {
            if let Some(filename) = filename {
                return self.lookup_file(base, platform, filename, pkg).await;
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
        if let Some(name) = zip
            .by_index(i)
            .ok()
            .and_then(|f| f.name().ok().map(std::borrow::Cow::into_owned))
        {
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
