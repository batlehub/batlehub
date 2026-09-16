//! Alpine `apk` as a path-addressed registry that also carries a coordinate.
//!
//! **A wrapper, not an arm** (RFC 0026 §6.2, decision 10). Every byte path is
//! [`PathProxyRegistryClient`]'s, unchanged: `fetch_artifact`, `probe_artifact`,
//! the `path_allow` glob, the traversal check and the base-URL re-read. Only
//! [`RegistryClient::resolve_metadata`] is overridden, and only to answer one
//! question the other path kinds cannot: **when was this package built?**
//!
//! That question has an answer here and nowhere else in the family because an
//! `APKINDEX` dates every package it lists — measured against
//! `v3.22/main/x86_64`, 5 647 of 5 647 entries carry a `t:` — so
//! `ReleaseAgeGateRule` gets a real `published_at` without a second upstream
//! request per artifact.
//!
//! The alternative was an `apk` branch inside `path_proxy.rs`, which would have
//! put an Alpine-shaped conditional on the path `deb`, `rpm`, `pacman`,
//! `jetbrains` and `generic` all take. A wrapper keeps those five unchanged by
//! construction rather than by review.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::TryStreamExt;

use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{FetchedArtifact, RegistryClient},
    services::apk::{apk_coordinate, ApkIndex},
};

use super::http_client::UpstreamHttpOptions;
use super::path_proxy::PathProxyRegistryClient;
// The format primitives live beside the other repository formats, in
// `repo/apk.rs`: this client reads an index, and phase 3's publish path writes
// one, and both go through the same member walker.
use crate::repo::apk::decode_index;

/// apk's own index freshness default (`database.c:1521`): four hours.
///
/// Used when the registry does not configure a `metadata_ttl`. Matching the
/// client's own cadence means a proxy that is neither staler nor chattier than
/// the mirror the client would otherwise talk to.
const DEFAULT_INDEX_TTL: Duration = Duration::from_secs(4 * 60 * 60);

/// How many `{branch}/{repo}/{arch}` indexes are held parsed at once.
///
/// An index for a real repository is ~5 600 entries, so one parsed `ApkIndex`
/// is a few hundred kilobytes. A fleet tracks a handful of branch/repo/arch
/// combinations; this caps the pathological case — a scanner walking every
/// branch Alpine has ever published — at a bounded cost rather than an
/// unbounded one.
const MAX_CACHED_INDEXES: usize = 16;

/// One parsed index and when it was read.
struct CachedIndex {
    index: ApkIndex,
    read_at: Instant,
}

pub struct ApkRegistryClient {
    inner: PathProxyRegistryClient,
    ttl: Duration,
    /// `{branch}/{repo}/{arch}` → the parsed index.
    ///
    /// A `Mutex` rather than an `RwLock`: the critical section is a hash lookup
    /// and an `Instant` comparison, and the expensive part — the upstream fetch
    /// and the parse — deliberately happens *outside* it. Two concurrent misses
    /// for the same repository each fetch once and the second insert wins,
    /// which costs one redundant request and avoids holding a lock across an
    /// `await`.
    indexes: Mutex<HashMap<String, CachedIndex>>,
}

impl ApkRegistryClient {
    pub fn new(base_url: impl Into<String>, opts: &UpstreamHttpOptions) -> Result<Self, CoreError> {
        Ok(Self {
            inner: PathProxyRegistryClient::new("apk", base_url, opts)?,
            ttl: DEFAULT_INDEX_TTL,
            indexes: Mutex::new(HashMap::new()),
        })
    }

    /// Restrict to `path_allow`, delegating to the inner client's compiler so
    /// an invalid glob fails identically for every path kind.
    pub fn with_path_allow(mut self, patterns: &[String]) -> Result<Self, CoreError> {
        self.inner = self.inner.with_path_allow(patterns)?;
        Ok(self)
    }

    /// How long a parsed index is reused before it is re-read upstream.
    pub fn with_index_ttl(mut self, ttl: Option<Duration>) -> Self {
        if let Some(ttl) = ttl {
            self.ttl = ttl;
        }
        self
    }

    /// The `{branch}/{repo}/{arch}` prefix of a `.apk` request, and the file
    /// name under it.
    ///
    /// `v3.22/main/x86_64/busybox-1.37.0-r20.apk` →
    /// (`v3.22/main/x86_64`, `busybox-1.37.0-r20.apk`). A path with no
    /// directory part has no index beside it, so it yields `None` and the
    /// coordinate reaches the gate undated.
    fn split_repo_path(path: &str) -> Option<(&str, &str)> {
        let (dir, file) = path.rsplit_once('/')?;
        if dir.is_empty() {
            return None;
        }
        Some((dir, file))
    }

    /// The parsed index for one repository directory, from cache when fresh.
    async fn index_for(&self, repo_dir: &str) -> Option<ApkIndex> {
        if let Some(hit) = self.cached(repo_dir) {
            return Some(hit);
        }

        let index_path = format!("{repo_dir}/APKINDEX.tar.gz");
        let pkg = PackageId::new("apk", "repo", "_").with_artifact(&index_path);
        let bytes = match self.inner.fetch_artifact(&pkg).await {
            Ok(fetched) => collect(fetched).await.ok()?,
            Err(err) => {
                // An index this instance cannot read is not an error for the
                // *artifact* request that triggered the read — it only means
                // the age gate has no date, which `deny_missing_timestamp`
                // exists to answer. Logged, not propagated.
                tracing::debug!(
                    path = %index_path,
                    error = %err,
                    "apk: no index available for the age gate"
                );
                return None;
            }
        };

        let text = decode_index(&bytes)?;
        let index = ApkIndex::parse(&text);
        tracing::debug!(
            path = %index_path,
            packages = index.len(),
            "apk: parsed upstream index for the age gate"
        );
        self.store(repo_dir, &index);
        Some(index)
    }

    fn cached(&self, repo_dir: &str) -> Option<ApkIndex> {
        let guard = self.indexes.lock().ok()?;
        let hit = guard.get(repo_dir)?;
        if hit.read_at.elapsed() > self.ttl {
            return None;
        }
        Some(hit.index.clone())
    }

    fn store(&self, repo_dir: &str, index: &ApkIndex) {
        let Ok(mut guard) = self.indexes.lock() else {
            return;
        };
        if guard.len() >= MAX_CACHED_INDEXES && !guard.contains_key(repo_dir) {
            // Drop whatever was read longest ago. A precise LRU would need a
            // second structure for a map that holds at most sixteen entries.
            if let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, v)| v.read_at)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest);
            }
        }
        guard.insert(
            repo_dir.to_owned(),
            CachedIndex {
                index: index.clone(),
                read_at: Instant::now(),
            },
        );
    }
}

/// Read a `FetchedArtifact` into memory.
///
/// Bounded by the index's real size — half a megabyte for a full Alpine
/// repository — and by `limits.max_artifact_size_bytes` on the streaming path
/// that already fetched it.
async fn collect(fetched: FetchedArtifact) -> Result<Vec<u8>, CoreError> {
    fetched
        .stream
        .try_fold(Vec::new(), |mut acc, chunk| async move {
            acc.extend_from_slice(&chunk);
            Ok(acc)
        })
        .await
}

#[async_trait]
impl RegistryClient for ApkRegistryClient {
    fn registry_type(&self) -> &str {
        self.inner.registry_type()
    }

    /// The one override: a `.apk` coordinate gets its build date from the
    /// index beside it; everything else delegates and stays undated.
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        // Delegate first, so the `path_allow` denial happens before any index
        // read — a path outside the allowlist must not cause an upstream
        // request, and metadata resolution runs before the artifact fetch.
        let mut meta = self.inner.resolve_metadata(pkg).await?;

        let Some(path) = pkg.artifact.as_deref() else {
            return Ok(meta);
        };
        let Some((repo_dir, file_name)) = Self::split_repo_path(path) else {
            return Ok(meta);
        };
        let Some((name, version)) = apk_coordinate(file_name) else {
            // The index itself, the key route, a directory listing: nothing
            // with a coordinate, so nothing to date.
            return Ok(meta);
        };

        if let Some(index) = self.index_for(repo_dir).await {
            if let Some(secs) = index.built_at(name, version) {
                meta.published_at = chrono::DateTime::from_timestamp(secs, 0);
            }
        }
        Ok(meta)
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        self.inner.fetch_artifact(pkg).await
    }

    async fn probe_artifact(&self, pkg: &PackageId) -> Result<(), CoreError> {
        self.inner.probe_artifact(pkg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::apk::tests_support::index_archive;

    const INDEX_BODY: &str = "C:Q1a=\nP:busybox\nV:1.37.0-r20\nt:1763764856\n\n\
                              C:Q1b=\nP:curl\nV:8.14.1-r3\nt:1749000000\n\n";

    #[test]
    fn split_repo_path_separates_the_repository_from_the_file() {
        assert_eq!(
            ApkRegistryClient::split_repo_path("v3.22/main/x86_64/busybox-1.37.0-r20.apk"),
            Some(("v3.22/main/x86_64", "busybox-1.37.0-r20.apk"))
        );
        assert_eq!(ApkRegistryClient::split_repo_path("loose.apk"), None);
    }

    #[tokio::test]
    async fn resolve_metadata_dates_a_package_from_the_index() {
        let mut server = mockito::Server::new_async().await;
        let index = server
            .mock("GET", "/v3.22/main/x86_64/APKINDEX.tar.gz")
            .with_status(200)
            .with_body(index_archive(INDEX_BODY))
            .expect(1) // read once, then cached
            .create_async()
            .await;

        let client = ApkRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = PackageId::new("alpine", "busybox", "1.37.0-r20")
            .with_artifact("v3.22/main/x86_64/busybox-1.37.0-r20.apk");

        for _ in 0..2 {
            let meta = client.resolve_metadata(&pkg).await.unwrap();
            assert_eq!(
                meta.published_at.map(|d| d.timestamp()),
                Some(1763764856),
                "the age gate reads t: from the index"
            );
        }
        index.assert_async().await;
    }

    /// A version the cached index does not list is the undated case
    /// `deny_missing_timestamp` exists to answer — not an error.
    #[tokio::test]
    async fn resolve_metadata_leaves_an_unlisted_version_undated() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v3.22/main/x86_64/APKINDEX.tar.gz")
            .with_status(200)
            .with_body(index_archive(INDEX_BODY))
            .create_async()
            .await;

        let client = ApkRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = PackageId::new("alpine", "busybox", "9.9.9-r0")
            .with_artifact("v3.22/main/x86_64/busybox-9.9.9-r0.apk");
        let meta = client.resolve_metadata(&pkg).await.unwrap();
        assert!(meta.published_at.is_none());
    }

    /// The index has no coordinate, so it must not trigger a read of itself —
    /// that would be one wasted upstream request per `apk update`.
    #[tokio::test]
    async fn resolve_metadata_does_not_read_an_index_for_the_index() {
        let server = mockito::Server::new_async().await;
        let client = ApkRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = PackageId::new("alpine", "repo", "_")
            .with_artifact("v3.22/main/x86_64/APKINDEX.tar.gz");
        let meta = client.resolve_metadata(&pkg).await.unwrap();
        assert!(meta.published_at.is_none());
        // No mock was registered: any upstream request would fail the test.
    }

    /// A missing index must not fail the artifact request that triggered it.
    #[tokio::test]
    async fn resolve_metadata_survives_an_index_that_is_not_there() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v3.22/main/x86_64/APKINDEX.tar.gz")
            .with_status(404)
            .create_async()
            .await;

        let client = ApkRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = PackageId::new("alpine", "busybox", "1.37.0-r20")
            .with_artifact("v3.22/main/x86_64/busybox-1.37.0-r20.apk");
        let meta = client.resolve_metadata(&pkg).await.unwrap();
        assert!(meta.published_at.is_none());
    }

    /// `path_allow` is the inner client's, and it must deny *before* the
    /// wrapper reaches for an index — otherwise a denied path still causes an
    /// upstream request.
    #[tokio::test]
    async fn path_allow_denies_before_any_index_read() {
        let server = mockito::Server::new_async().await;
        let client = ApkRegistryClient::new(server.url(), &UpstreamHttpOptions::default())
            .unwrap()
            .with_path_allow(&["v3.22/**".to_owned()])
            .unwrap();
        let pkg = PackageId::new("alpine", "busybox", "1.0-r0")
            .with_artifact("edge/main/x86_64/busybox-1.0-r0.apk");
        assert!(matches!(
            client.resolve_metadata(&pkg).await,
            Err(CoreError::AccessDenied(_))
        ));
    }

    #[tokio::test]
    async fn fetch_artifact_delegates_to_the_path_client() {
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("GET", "/v3.22/main/x86_64/busybox-1.37.0-r20.apk")
            .with_status(200)
            .with_body("apk bytes")
            .create_async()
            .await;

        let client = ApkRegistryClient::new(server.url(), &UpstreamHttpOptions::default()).unwrap();
        let pkg = PackageId::new("alpine", "busybox", "1.37.0-r20")
            .with_artifact("v3.22/main/x86_64/busybox-1.37.0-r20.apk");
        let body = collect(client.fetch_artifact(&pkg).await.unwrap())
            .await
            .unwrap();
        assert_eq!(body, b"apk bytes");
        assert_eq!(client.registry_type(), "apk");
    }
}
