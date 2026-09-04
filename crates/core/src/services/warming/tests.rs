use bytes::Bytes;
use chrono::Utc;

use super::*;
use crate::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{
        ArtifactCacheMeta, ArtifactMetaRecord, FetchedArtifact, RegistryClient, StorageBackend,
        StorageMeta, StoredArtifact,
    },
};
use async_trait::async_trait;

struct PanicClient;
struct PanicStorage;
struct PanicMeta;

#[async_trait]
impl RegistryClient for PanicClient {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, _: &PackageId) -> Result<PackageMetadata, CoreError> {
        panic!("should not be called")
    }
    async fn fetch_artifact(&self, _: &PackageId) -> Result<FetchedArtifact, CoreError> {
        panic!("should not be called")
    }
    async fn list_versions(&self, _: &str) -> Result<Vec<String>, CoreError> {
        panic!("should not be called")
    }
}

#[async_trait]
impl StorageBackend for PanicStorage {
    async fn store(&self, _: &str, _: bytes::Bytes, _: StorageMeta) -> Result<(), CoreError> {
        panic!("should not be called")
    }
    async fn retrieve(&self, _: &str) -> Result<Option<StoredArtifact>, CoreError> {
        panic!("should not be called")
    }
    async fn exists(&self, _: &str) -> Result<bool, CoreError> {
        panic!("should not be called")
    }
    async fn delete(&self, _: &str) -> Result<bool, CoreError> {
        panic!("should not be called")
    }
    async fn delete_by_prefix(&self, _: &str) -> Result<usize, CoreError> {
        panic!("should not be called")
    }
    async fn stat_by_prefix(&self, _: &str) -> Result<(u64, u64), CoreError> {
        panic!("should not be called")
    }
    async fn list_keys(&self, _: &str) -> Result<Vec<String>, CoreError> {
        panic!("should not be called")
    }
}

#[async_trait]
impl ArtifactCacheMeta for PanicMeta {
    async fn record_artifact(&self, _: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        panic!("should not be called")
    }
    async fn get_artifact_checksum(&self, _: &str) -> Result<Option<String>, CoreError> {
        Ok(None)
    }
    async fn touch_artifact(&self, _: &str) -> Result<(), CoreError> {
        panic!("should not be called")
    }
    async fn is_artifact_expired(
        &self,
        _: &str,
        _: chrono::DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        panic!("should not be called")
    }
    async fn delete_artifact_meta(&self, _: &str) -> Result<(), CoreError> {
        panic!("should not be called")
    }
}

fn disabled_svc() -> WarmingService {
    WarmingService {
        client: Arc::new(PanicClient),
        storage: Arc::new(PanicStorage),
        artifact_meta: Arc::new(PanicMeta),
        registry_name: "test".into(),
        latest_n: 3,
        concurrency: 0,
        coordinator: Arc::new(crate::ports::NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(crate::services::metrics::ProxyMetrics::new(&[
            "test".into(),
            "test-reg".into(),
        ])),
    }
}

#[tokio::test]
async fn concurrency_zero_disables_warm_package() {
    let svc = disabled_svc();
    let report = svc.warm_package("lodash").await;
    assert_eq!(report.warmed, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn concurrency_zero_disables_warm_all() {
    let svc = disabled_svc();
    let report = svc.warm_all(&["lodash".into(), "react".into()]).await;
    assert_eq!(report.warmed, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors, 0);
}

// ── Functional mocks for active warming tests ─────────────────────────────

use futures::stream;
use std::collections::HashMap;
use tokio::sync::Mutex as TokioMutex;

struct StubClient {
    /// Drives `warm_artifact()`'s per-kind lookup: `"stub"` parses to no
    /// `RegistryKind`, so the sub-coordinate stays `None`.
    registry_type: &'static str,
    versions: Vec<String>,
    fail_fetch: bool,
    fail_list: bool,
    fail_stream: bool,
    panic_fetch: bool,
}

impl StubClient {
    fn with_versions(versions: Vec<&str>) -> Arc<Self> {
        Arc::new(Self {
            registry_type: "stub",
            versions: versions.into_iter().map(str::to_owned).collect(),
            fail_fetch: false,
            fail_list: false,
            fail_stream: false,
            panic_fetch: false,
        })
    }
    /// A client claiming a real registry kind, to exercise the per-kind
    /// artifact sub-coordinate.
    fn of_kind(registry_type: &'static str, versions: Vec<&str>) -> Arc<Self> {
        Arc::new(Self {
            registry_type,
            versions: versions.into_iter().map(str::to_owned).collect(),
            fail_fetch: false,
            fail_list: false,
            fail_stream: false,
            panic_fetch: false,
        })
    }
    fn failing_list() -> Arc<Self> {
        Arc::new(Self {
            registry_type: "stub",
            versions: vec![],
            fail_fetch: false,
            fail_list: true,
            fail_stream: false,
            panic_fetch: false,
        })
    }
    fn failing_fetch() -> Arc<Self> {
        Arc::new(Self {
            registry_type: "stub",
            versions: vec!["1.0.0".into()],
            fail_fetch: true,
            fail_list: false,
            fail_stream: false,
            panic_fetch: false,
        })
    }
    fn failing_stream() -> Arc<Self> {
        Arc::new(Self {
            registry_type: "stub",
            versions: vec!["1.0.0".into()],
            fail_fetch: false,
            fail_list: false,
            fail_stream: true,
            panic_fetch: false,
        })
    }
    fn panicking_fetch() -> Arc<Self> {
        Arc::new(Self {
            registry_type: "stub",
            versions: vec!["1.0.0".into()],
            fail_fetch: false,
            fail_list: false,
            fail_stream: false,
            panic_fetch: true,
        })
    }
}

#[async_trait]
impl RegistryClient for StubClient {
    fn registry_type(&self) -> &str {
        self.registry_type
    }
    async fn resolve_metadata(&self, _: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: PackageId::new("stub", "pkg", "0.0.0"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        })
    }
    async fn fetch_artifact(&self, _: &PackageId) -> Result<FetchedArtifact, CoreError> {
        if self.panic_fetch {
            panic!("simulated task panic");
        }
        if self.fail_fetch {
            return Err(CoreError::Registry("fetch failed".into()));
        }
        if self.fail_stream {
            let chunks = vec![
                Ok::<Bytes, CoreError>(Bytes::from("partial-")),
                Err(CoreError::Registry("stream failed".into())),
            ];
            return Ok(FetchedArtifact {
                stream: Box::pin(stream::iter(chunks)),
                cache_control: None,
            });
        }
        let data = Bytes::from("stub-artifact-data");
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: None,
        })
    }
    async fn list_versions(&self, _: &str) -> Result<Vec<String>, CoreError> {
        if self.fail_list {
            return Err(CoreError::Registry("list failed".into()));
        }
        Ok(self.versions.clone())
    }
}

struct StubStorage {
    data: Arc<TokioMutex<HashMap<String, Bytes>>>,
    fail_store: bool,
    fail_exists: bool,
}

impl StubStorage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(TokioMutex::new(HashMap::new())),
            fail_store: false,
            fail_exists: false,
        })
    }
    fn failing_store() -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(TokioMutex::new(HashMap::new())),
            fail_store: true,
            fail_exists: false,
        })
    }
    fn failing_exists() -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(TokioMutex::new(HashMap::new())),
            fail_store: false,
            fail_exists: true,
        })
    }
}

#[async_trait]
impl StorageBackend for StubStorage {
    async fn store(&self, key: &str, data: Bytes, _: StorageMeta) -> Result<(), CoreError> {
        if self.fail_store {
            return Err(CoreError::Storage("store failed".into()));
        }
        self.data.lock().await.insert(key.to_owned(), data);
        Ok(())
    }
    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
        Ok(self.data.lock().await.get(key).map(|d| StoredArtifact {
            stream: Box::pin(stream::once({
                let b = d.clone();
                async move { Ok::<Bytes, CoreError>(b) }
            })),
            meta: StorageMeta::default(),
        }))
    }
    async fn exists(&self, key: &str) -> Result<bool, CoreError> {
        if self.fail_exists {
            return Err(CoreError::Storage("exists failed".into()));
        }
        Ok(self.data.lock().await.contains_key(key))
    }
    async fn delete(&self, key: &str) -> Result<bool, CoreError> {
        Ok(self.data.lock().await.remove(key).is_some())
    }
    async fn delete_by_prefix(&self, prefix: &str) -> Result<usize, CoreError> {
        let mut m = self.data.lock().await;
        let before = m.len();
        m.retain(|k, _| !k.starts_with(prefix));
        Ok(before - m.len())
    }
    async fn stat_by_prefix(&self, prefix: &str) -> Result<(u64, u64), CoreError> {
        let m = self.data.lock().await;
        let matching: Vec<_> = m.iter().filter(|(k, _)| k.starts_with(prefix)).collect();
        Ok((
            matching.len() as u64,
            matching.iter().map(|(_, v)| v.len() as u64).sum(),
        ))
    }
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        Ok(self
            .data
            .lock()
            .await
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

struct NoopMeta;
#[async_trait]
impl ArtifactCacheMeta for NoopMeta {
    async fn record_artifact(&self, _: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        Ok(())
    }
    async fn get_artifact_checksum(&self, _: &str) -> Result<Option<String>, CoreError> {
        Ok(None)
    }
    async fn touch_artifact(&self, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn is_artifact_expired(
        &self,
        _: &str,
        _: chrono::DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete_artifact_meta(&self, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
}

struct FailingRecordMeta;
#[async_trait]
impl ArtifactCacheMeta for FailingRecordMeta {
    async fn record_artifact(&self, _: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        Err(CoreError::Storage("record_artifact failed".into()))
    }
    async fn get_artifact_checksum(&self, _: &str) -> Result<Option<String>, CoreError> {
        Ok(None)
    }
    async fn touch_artifact(&self, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn is_artifact_expired(
        &self,
        _: &str,
        _: chrono::DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete_artifact_meta(&self, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
}

fn active_svc(client: Arc<dyn RegistryClient>, storage: Arc<dyn StorageBackend>) -> WarmingService {
    WarmingService {
        client,
        storage,
        artifact_meta: Arc::new(NoopMeta),
        registry_name: "test-reg".into(),
        latest_n: 3,
        concurrency: 4,
        coordinator: Arc::new(crate::ports::NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(crate::services::metrics::ProxyMetrics::new(&[
            "test".into(),
            "test-reg".into(),
        ])),
    }
}

#[tokio::test]
async fn warm_package_fetches_and_stores_new_version() {
    let storage = StubStorage::new();
    let svc = active_svc(StubClient::with_versions(vec!["1.0.0"]), storage.clone());
    let report = svc.warm_package("mylib@1.0.0").await;
    assert_eq!(report.warmed, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors, 0);
    // Must be the exact key the proxy read path reads:
    // `artifact:` + PackageId::cache_key().
    let key = format!(
        "artifact:{}",
        PackageId::new("test-reg", "mylib", "1.0.0").cache_key()
    );
    assert_eq!(key, "artifact:test-reg/mylib/1.0.0");
    assert!(storage.exists(&key).await.unwrap());
}

#[tokio::test]
async fn warm_package_uses_the_per_kind_artifact_coordinate() {
    let storage = StubStorage::new();
    let svc = active_svc(
        StubClient::of_kind("jetbrains-marketplace", vec!["1.0.0"]),
        storage.clone(),
    );
    let report = svc.warm_package("org.rust.lang@1.0.0").await;
    assert_eq!(report.warmed, 1);
    assert_eq!(report.errors, 0);
    // JetBrains plugin archives are served from the `plugin` sub-coordinate, so
    // that is where warming must put them.
    let key = format!(
        "artifact:{}",
        PackageId::new("test-reg", "org.rust.lang", "1.0.0")
            .with_artifact("plugin")
            .cache_key()
    );
    assert_eq!(key, "artifact:test-reg/org.rust.lang/1.0.0/plugin");
    assert!(storage.exists(&key).await.unwrap());
}

#[tokio::test]
async fn warm_package_pinned_scoped_npm_name() {
    let storage = StubStorage::new();
    let svc = active_svc(StubClient::with_versions(vec!["1.0.0"]), storage.clone());
    let report = svc.warm_package("@babel/core@1.0.0").await;
    assert_eq!(report.warmed, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors, 0);
    let key = "artifact:test-reg/@babel/core/1.0.0";
    assert!(storage.exists(key).await.unwrap());
}

#[tokio::test]
async fn warm_package_unpinned_scoped_npm_name() {
    let storage = StubStorage::new();
    let svc = WarmingService {
        client: StubClient::with_versions(vec!["1.0.0"]),
        storage: storage.clone(),
        artifact_meta: Arc::new(NoopMeta),
        registry_name: "test-reg".into(),
        latest_n: 1,
        concurrency: 4,
        coordinator: Arc::new(crate::ports::NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(crate::services::metrics::ProxyMetrics::new(&[
            "test".into(),
            "test-reg".into(),
        ])),
    };
    let report = svc.warm_package("@babel/core").await;
    assert_eq!(report.warmed, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors, 0);
    let key = "artifact:test-reg/@babel/core/1.0.0";
    assert!(storage.exists(key).await.unwrap());
}

#[tokio::test]
async fn warm_package_skips_already_cached_version() {
    let storage = StubStorage::new();
    storage
        .store(
            "artifact:test-reg/mylib/1.0.0",
            Bytes::from("old"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    let svc = active_svc(StubClient::with_versions(vec!["1.0.0"]), storage.clone());
    let report = svc.warm_package("mylib@1.0.0").await;
    assert_eq!(report.warmed, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn warm_package_lists_versions_and_warms_latest_n() {
    let storage = StubStorage::new();
    let svc = WarmingService {
        client: StubClient::with_versions(vec!["1.0.0", "1.1.0", "1.2.0", "2.0.0"]),
        storage: storage.clone(),
        artifact_meta: Arc::new(NoopMeta),
        registry_name: "test-reg".into(),
        latest_n: 2,
        concurrency: 4,
        coordinator: Arc::new(crate::ports::NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(crate::services::metrics::ProxyMetrics::new(&[
            "test".into(),
            "test-reg".into(),
        ])),
    };
    let report = svc.warm_package("mylib").await;
    assert_eq!(report.warmed, 2);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn warm_package_returns_error_when_list_versions_fails() {
    let svc = active_svc(StubClient::failing_list(), StubStorage::new());
    let report = svc.warm_package("mylib").await;
    assert_eq!(report.errors, 1);
    assert_eq!(report.warmed, 0);
}

#[tokio::test]
async fn warming_a_path_stores_under_the_proxy_cache_key() {
    let storage = StubStorage::new();
    let svc = active_svc(StubClient::with_versions(vec!["unused"]), storage.clone());
    let report = svc
        .warm_all_paths(&["idea/idea-2026.1.3.tar.gz".to_owned()])
        .await;
    assert_eq!(report.warmed, 1);
    assert_eq!(report.errors, 0);
    // Must be the exact key the proxy read path reads: artifact:{reg}/repo/_/{path}.
    let key = "artifact:test-reg/repo/_/idea/idea-2026.1.3.tar.gz";
    assert!(storage.exists(key).await.unwrap());
}

#[tokio::test]
async fn warm_all_paths_skips_already_cached() {
    let storage = StubStorage::new();
    storage
        .store(
            "artifact:test-reg/repo/_/a.bin",
            Bytes::from("cached"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    let svc = active_svc(StubClient::with_versions(vec!["unused"]), storage.clone());
    let report = svc
        .warm_all_paths(&["a.bin".to_owned(), "b.bin".to_owned()])
        .await;
    assert_eq!(report.warmed, 1, "b.bin warmed");
    assert_eq!(report.skipped, 1, "a.bin already cached");
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn warm_all_paths_disabled_when_concurrency_zero() {
    let mut svc = active_svc(
        StubClient::with_versions(vec!["unused"]),
        StubStorage::new(),
    );
    svc.concurrency = 0;
    let report = svc.warm_all_paths(&["a.bin".to_owned()]).await;
    assert_eq!(report.warmed, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn warm_package_records_error_when_fetch_fails() {
    let svc = active_svc(StubClient::failing_fetch(), StubStorage::new());
    let report = svc.warm_package("mylib@1.0.0").await;
    assert_eq!(report.errors, 1);
    assert_eq!(report.warmed, 0);
}

#[tokio::test]
async fn warm_package_records_error_when_store_fails() {
    let svc = active_svc(
        StubClient::with_versions(vec!["1.0.0"]),
        StubStorage::failing_store(),
    );
    let report = svc.warm_package("mylib@1.0.0").await;
    assert_eq!(report.errors, 1);
    assert_eq!(report.warmed, 0);
}

#[tokio::test]
async fn warm_all_aggregates_multiple_packages() {
    let storage = StubStorage::new();
    let svc = active_svc(StubClient::with_versions(vec!["1.0.0"]), storage.clone());
    let report = svc
        .warm_all(&["pkgA@1.0.0".to_string(), "pkgB@1.0.0".to_string()])
        .await;
    assert_eq!(report.warmed, 2);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn with_latest_n_creates_new_service_with_different_n() {
    let svc = active_svc(StubClient::with_versions(vec![]), StubStorage::new());
    assert_eq!(svc.latest_n, 3);
    let svc2 = svc.with_latest_n(10);
    assert_eq!(svc2.latest_n, 10);
    assert_eq!(svc2.registry_name, "test-reg");
}

#[tokio::test]
async fn warm_package_records_error_when_exists_check_fails() {
    let svc = active_svc(
        StubClient::with_versions(vec!["1.0.0"]),
        StubStorage::failing_exists(),
    );
    let report = svc.warm_package("mylib@1.0.0").await;
    assert_eq!(report.errors, 1);
    assert_eq!(report.warmed, 0);
    assert_eq!(report.skipped, 0);
}

#[tokio::test]
async fn warm_package_records_error_on_mid_stream_failure() {
    let svc = active_svc(StubClient::failing_stream(), StubStorage::new());
    let report = svc.warm_package("mylib@1.0.0").await;
    assert_eq!(report.errors, 1);
    assert_eq!(report.warmed, 0);
}

#[tokio::test]
async fn warm_package_succeeds_despite_record_artifact_failure() {
    let storage = StubStorage::new();
    let svc = WarmingService {
        client: StubClient::with_versions(vec!["1.0.0"]),
        storage: storage.clone(),
        artifact_meta: Arc::new(FailingRecordMeta),
        registry_name: "test-reg".into(),
        latest_n: 3,
        concurrency: 4,
        coordinator: Arc::new(crate::ports::NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(crate::services::metrics::ProxyMetrics::new(&[
            "test".into(),
            "test-reg".into(),
        ])),
    };
    let report = svc.warm_package("mylib@1.0.0").await;
    // record_artifact failure is logged but non-fatal: the artifact is
    // still considered warmed.
    assert_eq!(report.warmed, 1);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn warm_package_latest_n_larger_than_available_versions() {
    let storage = StubStorage::new();
    let svc = WarmingService {
        client: StubClient::with_versions(vec!["1.0.0", "1.1.0"]),
        storage: storage.clone(),
        artifact_meta: Arc::new(NoopMeta),
        registry_name: "test-reg".into(),
        latest_n: 10,
        concurrency: 4,
        coordinator: Arc::new(crate::ports::NoopWarmCoordinator),
        platforms: Vec::new(),
        metrics: Arc::new(crate::services::metrics::ProxyMetrics::new(&[
            "test".into(),
            "test-reg".into(),
        ])),
    };
    let report = svc.warm_package("mylib").await;
    assert_eq!(report.warmed, 2);
    assert_eq!(report.errors, 0);
}

#[tokio::test]
async fn warm_package_records_error_when_task_panics() {
    let svc = active_svc(StubClient::panicking_fetch(), StubStorage::new());
    let report = svc.warm_package("mylib@1.0.0").await;
    assert_eq!(report.errors, 1);
    assert_eq!(report.warmed, 0);
}

#[tokio::test]
async fn warming_report_add_assign_aggregates() {
    let mut a = WarmingReport {
        warmed: 1,
        skipped: 2,
        errors: 3,
        failures: vec![failure("a")],
    };
    let b = WarmingReport {
        warmed: 10,
        skipped: 20,
        errors: 30,
        failures: vec![failure("b")],
    };
    a += b;
    assert_eq!(a.warmed, 11);
    assert_eq!(a.skipped, 22);
    assert_eq!(a.errors, 33);
    // Concatenated, not summed: the point of RFC 0004-bis A3 is that a batch
    // report names each failure, and a merge that kept only one side would put
    // the count and the list out of step.
    assert_eq!(
        a.failures
            .iter()
            .map(|f| f.package.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

fn failure(package: &str) -> crate::services::WarmFailure {
    crate::services::WarmFailure {
        package: package.to_owned(),
        version: Some("1.0.0".to_owned()),
        error: "boom".to_owned(),
    }
}

/// A named failure, not just a count (RFC 0004-bis A3).
///
/// An operator warming eleven registries read `errors: 3` and had no way to
/// learn which three without shell access to the instance they are
/// administering through a console — the information existed, and went only to
/// the server log.
#[tokio::test]
async fn a_failed_fetch_names_the_package_and_version() {
    let svc = active_svc(StubClient::failing_fetch(), StubStorage::new());
    let report = svc.warm_package("mylib@1.0.0").await;

    assert_eq!(report.errors, 1);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].package, "mylib");
    assert_eq!(report.failures[0].version.as_deref(), Some("1.0.0"));
    assert!(!report.failures[0].error.is_empty());
}

/// When *listing* the versions fails there is no single version at fault, and
/// naming one would be a guess.
#[tokio::test]
async fn a_failed_version_listing_names_the_package_and_no_version() {
    let svc = active_svc(StubClient::failing_list(), StubStorage::new());
    let report = svc.warm_package("mylib").await;

    assert_eq!(report.errors, 1);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].package, "mylib");
    assert_eq!(report.failures[0].version, None);
}
