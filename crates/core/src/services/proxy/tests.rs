use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures::stream;

use std::time::Duration;

use super::*;
use crate::entities::{
    AccessEvent, AccessResult, EventFilter, Identity, PackageFilter, PackageId, PackageMetadata,
    PackageStatus, PackageSummary,
};
use crate::ports::ByteStream;
use crate::ports::{
    ArtifactCacheMeta, ArtifactMeta, ArtifactMetaRecord, CacheStore, FetchedArtifact,
    PackageRepository, RegistryClient, StorageBackend, StorageMeta, StoredArtifact,
};
use crate::services::hot_config::{new_hot_lock, HotConfig, RegistryPolicy};
use crate::services::metrics::ProxyMetrics;

fn make_hot(
    registry_name: &str,
    client: Arc<dyn RegistryClient>,
    policy: RegistryPolicy,
    max_bytes: Option<u64>,
) -> crate::services::hot_config::HotConfigLock {
    let mut registries = HashMap::new();
    registries.insert(registry_name.to_owned(), client);
    let mut policies = HashMap::new();
    policies.insert(registry_name.to_owned(), Arc::new(policy));
    new_hot_lock(HotConfig {
        registries,
        policies,
        max_artifact_size_bytes: max_bytes,
        ..Default::default()
    })
}

fn empty_hot(
    registry_name: &str,
    client: Arc<dyn RegistryClient>,
) -> crate::services::hot_config::HotConfigLock {
    let mut registries = HashMap::new();
    registries.insert(registry_name.to_owned(), client);
    new_hot_lock(HotConfig {
        registries,
        policies: HashMap::new(),
        ..Default::default()
    })
}

// ── Minimal in-memory mocks ───────────────────────────────────────────────

struct NoopArtifactMeta;
impl NoopArtifactMeta {
    fn arc() -> Arc<dyn ArtifactCacheMeta> {
        Arc::new(Self)
    }
}
#[async_trait]
impl ArtifactCacheMeta for NoopArtifactMeta {
    async fn record_artifact(&self, _rec: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        Ok(())
    }
    async fn get_artifact_checksum(&self, _key: &str) -> Result<Option<String>, CoreError> {
        Ok(None)
    }
    async fn touch_artifact(&self, _key: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn is_artifact_expired(
        &self,
        _key: &str,
        _older_than: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete_artifact_meta(&self, _key: &str) -> Result<(), CoreError> {
        Ok(())
    }
}

/// Records `record_artifact` and `touch_artifact` calls; returns configurable
/// expired-artifact list from `list_expired_by_ttl`.
struct SpyArtifactMeta {
    recorded: Mutex<Vec<String>>,
    touched: Mutex<Vec<String>>,
    expired: Mutex<Vec<ArtifactMeta>>,
    checksums: Mutex<HashMap<String, String>>,
    fail_checksum_lookup: std::sync::atomic::AtomicBool,
}
impl SpyArtifactMeta {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            recorded: Mutex::new(vec![]),
            touched: Mutex::new(vec![]),
            expired: Mutex::new(vec![]),
            checksums: Mutex::new(HashMap::new()),
            fail_checksum_lookup: std::sync::atomic::AtomicBool::new(false),
        })
    }
    fn with_expired(expired: Vec<ArtifactMeta>) -> Arc<Self> {
        Arc::new(Self {
            recorded: Mutex::new(vec![]),
            touched: Mutex::new(vec![]),
            expired: Mutex::new(expired),
            checksums: Mutex::new(HashMap::new()),
            fail_checksum_lookup: std::sync::atomic::AtomicBool::new(false),
        })
    }
    /// Make `get_artifact_checksum` return an error, simulating a transient
    /// metadata-store failure (used to assert the re-serve path fails closed).
    fn fail_checksum_lookups(&self) {
        self.fail_checksum_lookup
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn recorded_keys(&self) -> Vec<String> {
        self.recorded.lock().unwrap().clone()
    }
    fn touched_keys(&self) -> Vec<String> {
        self.touched.lock().unwrap().clone()
    }
}
#[async_trait]
impl ArtifactCacheMeta for SpyArtifactMeta {
    async fn record_artifact(&self, rec: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        self.recorded.lock().unwrap().push(rec.key.to_owned());
        if let Some(c) = rec.checksum {
            self.checksums
                .lock()
                .unwrap()
                .insert(rec.key.to_owned(), c.to_owned());
        }
        Ok(())
    }
    async fn get_artifact_checksum(&self, key: &str) -> Result<Option<String>, CoreError> {
        if self
            .fail_checksum_lookup
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(CoreError::Storage("checksum lookup failed".into()));
        }
        Ok(self.checksums.lock().unwrap().get(key).cloned())
    }
    async fn touch_artifact(&self, key: &str) -> Result<(), CoreError> {
        self.touched.lock().unwrap().push(key.to_owned());
        Ok(())
    }
    async fn is_artifact_expired(
        &self,
        key: &str,
        older_than: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, CoreError> {
        // If the key has been "recorded" (has metadata) and is NOT explicitly in the
        // expired list, treat it as fresh.  If no metadata has been recorded, treat
        // it as expired (matches PgArtifactMetaRepository semantics: missing row → expired).
        let recorded = self.recorded.lock().unwrap();
        let expired = self.expired.lock().unwrap();
        let has_meta =
            recorded.contains(&key.to_owned()) || expired.iter().any(|m| m.artifact_key == key);
        if !has_meta {
            return Ok(true);
        }
        let is_expired = expired
            .iter()
            .any(|m| m.artifact_key == key && m.cached_at < older_than);
        Ok(is_expired)
    }
    async fn delete_artifact_meta(&self, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
}

struct TestCacheStore {
    data: Mutex<HashMap<String, crate::ports::CacheEntry>>,
}

impl TestCacheStore {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(HashMap::new()),
        })
    }

    fn seed_expired(&self, key: &str, metadata: PackageMetadata) {
        let entry = crate::ports::CacheEntry {
            metadata,
            cached_at: Utc::now() - chrono::Duration::hours(2),
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        };
        self.data.lock().unwrap().insert(key.to_owned(), entry);
    }
}

#[async_trait]
impl CacheStore for TestCacheStore {
    async fn get(&self, key: &str) -> Result<Option<crate::ports::CacheEntry>, CoreError> {
        let map = self.data.lock().unwrap();
        Ok(map.get(key).filter(|e| !e.is_expired()).cloned())
    }
    async fn set(
        &self,
        key: &str,
        mut entry: crate::ports::CacheEntry,
        ttl: Option<std::time::Duration>,
    ) -> Result<(), CoreError> {
        if let Some(ttl) = ttl {
            entry.expires_at =
                Some(Utc::now() + chrono::Duration::from_std(ttl).unwrap_or_default());
        }
        self.data.lock().unwrap().insert(key.to_owned(), entry);
        Ok(())
    }
    async fn invalidate(&self, key: &str) -> Result<(), CoreError> {
        self.data.lock().unwrap().remove(key);
        Ok(())
    }
    async fn get_stale(&self, key: &str) -> Result<Option<crate::ports::CacheEntry>, CoreError> {
        Ok(self.data.lock().unwrap().get(key).cloned())
    }
}

struct SpyRepo {
    events: Mutex<Vec<AccessEvent>>,
}

impl SpyRepo {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(vec![]),
        })
    }

    fn events(&self) -> Vec<AccessEvent> {
        self.events.lock().unwrap().clone()
    }
}

#[async_trait]
impl PackageRepository for SpyRepo {
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
    async fn get_status(&self, _pkg: &PackageId) -> Result<PackageStatus, CoreError> {
        Ok(PackageStatus::Available)
    }
    async fn set_status(&self, _pkg: &PackageId, _status: PackageStatus) -> Result<(), CoreError> {
        Ok(())
    }
    async fn list_packages(
        &self,
        _filter: PackageFilter,
    ) -> Result<Vec<PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(&self, _filter: PackageFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(&self, _filter: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(self.events.lock().unwrap().clone())
    }
    async fn count_events(&self, _filter: EventFilter) -> Result<u64, CoreError> {
        Ok(self.events.lock().unwrap().len() as u64)
    }
    async fn delete_package(&self, _pkg: &PackageId) -> Result<bool, CoreError> {
        Ok(false)
    }
}

struct MemStorage {
    data: Mutex<HashMap<String, Bytes>>,
}

impl MemStorage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl StorageBackend for MemStorage {
    async fn store(&self, key: &str, data: Bytes, _meta: StorageMeta) -> Result<(), CoreError> {
        self.data.lock().unwrap().insert(key.to_owned(), data);
        Ok(())
    }
    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
        let lock = self.data.lock().unwrap();
        Ok(lock.get(key).map(|bytes| {
            let b = bytes.clone();
            let s: ByteStream = Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(b) }));
            StoredArtifact {
                stream: s,
                meta: StorageMeta::default(),
            }
        }))
    }
    async fn exists(&self, key: &str) -> Result<bool, CoreError> {
        Ok(self.data.lock().unwrap().contains_key(key))
    }
    async fn delete(&self, key: &str) -> Result<bool, CoreError> {
        Ok(self.data.lock().unwrap().remove(key).is_some())
    }
    async fn delete_by_prefix(&self, prefix: &str) -> Result<usize, CoreError> {
        let mut map = self.data.lock().unwrap();
        let keys: Vec<String> = map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        let count = keys.len();
        for k in keys {
            map.remove(&k);
        }
        Ok(count)
    }
    async fn stat_by_prefix(&self, prefix: &str) -> Result<(u64, u64), CoreError> {
        let map = self.data.lock().unwrap();
        let (count, bytes) = map
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .fold((0u64, 0u64), |(c, b), (_, v)| (c + 1, b + v.len() as u64));
        Ok((count, bytes))
    }
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        let map = self.data.lock().unwrap();
        Ok(map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

struct FixedRegistry;

#[async_trait]
impl RegistryClient for FixedRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(30)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({}),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let data = Bytes::from(format!("artifact:{}", pkg.cache_key()));
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: None,
        })
    }
}

struct DenyRegistry;

#[async_trait]
impl RegistryClient for DenyRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(30)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({}),
            cache_control: None,
        })
    }
    async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        Err(CoreError::Registry("should not be called".into()))
    }
}

struct AlwaysDenyRule;

#[async_trait]
impl crate::rules::Rule for AlwaysDenyRule {
    fn name(&self) -> &str {
        "always_deny"
    }
    async fn evaluate(&self, _ctx: &crate::rules::RuleContext<'_>) -> crate::rules::RuleDecision {
        crate::rules::RuleDecision::Deny {
            reason: "test denial".to_owned(),
        }
    }
}

fn req(registry: &str) -> ProxyRequest {
    ProxyRequest {
        package_id: PackageId::new(registry, "test-pkg", "1.0.0"),
        identity: Identity::anonymous(),
        action: Action::ReleasesRead,
        ip_address: None,
        user_agent: None,
    }
}

fn proxy(
    registry_name: &str,
    client: Arc<dyn RegistryClient>,
    repo: Arc<dyn PackageRepository>,
    rules: Vec<Box<dyn crate::rules::Rule>>,
) -> ProxyService {
    let policy = RegistryPolicy {
        metadata_ttl: None,
        firewall_only: false,
        serve_stale_metadata: false,
        artifact_ttl: None,
        rules,
    };
    ProxyService {
        hot: make_hot(registry_name, client, policy, None),
        storage: MemStorage::new(),
        cache: TestCacheStore::new(),
        repo,
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn unknown_registry_returns_error() {
    let svc = proxy("npm", Arc::new(FixedRegistry), SpyRepo::new(), vec![]);
    let result = svc.handle(req("unknown")).await;
    assert!(matches!(result, Err(CoreError::UnknownRegistry(_))));
}

#[tokio::test]
async fn rejects_path_traversal_in_coordinate() {
    let svc = proxy("npm", Arc::new(FixedRegistry), SpyRepo::new(), vec![]);

    // `..` in the name escapes the storage root once interpolated into the cache
    // key — the edge chokepoint must reject it before any cache/storage access.
    let bad_name = ProxyRequest {
        package_id: PackageId::new("npm", "../../../../etc/passwd", "1.0.0"),
        identity: Identity::anonymous(),
        action: Action::ReleasesRead,
        ip_address: None,
        user_agent: None,
    };
    assert!(
        matches!(svc.handle(bad_name).await, Err(CoreError::InvalidInput(_))),
        "traversal in name must be rejected"
    );

    // ...and in the version segment...
    let bad_version = ProxyRequest {
        package_id: PackageId::new("npm", "test-pkg", "../../etc"),
        identity: Identity::anonymous(),
        action: Action::ReleasesRead,
        ip_address: None,
        user_agent: None,
    };
    assert!(
        matches!(
            svc.handle(bad_version).await,
            Err(CoreError::InvalidInput(_))
        ),
        "traversal in version must be rejected"
    );

    // ...and in the sub-artifact.
    let bad_artifact = ProxyRequest {
        package_id: PackageId::new("npm", "test-pkg", "1.0.0").with_artifact("../evil"),
        identity: Identity::anonymous(),
        action: Action::SourceRead,
        ip_address: None,
        user_agent: None,
    };
    assert!(
        matches!(
            svc.handle(bad_artifact).await,
            Err(CoreError::InvalidInput(_))
        ),
        "traversal in artifact must be rejected"
    );
}

#[tokio::test]
async fn metadata_cache_miss_then_hit() {
    let repo = SpyRepo::new();
    let cache = TestCacheStore::new();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(FixedRegistry),
            RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![],
            },
            None,
        ),
        storage: MemStorage::new(),
        cache: cache.clone(),
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let cache_key = format!("meta:{}", req("npm").package_id.cache_key());

    // First call: cache miss — metadata is fetched and stored
    assert!(cache.get(&cache_key).await.unwrap().is_none());
    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    assert!(
        cache.get(&cache_key).await.unwrap().is_some(),
        "metadata should be cached after first call"
    );

    // Second call: cache hit — lines 86-87 are exercised
    let resp2 = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp2, ProxyResponse::Stream(_)),
        "second call must still return Stream"
    );
}

#[tokio::test]
async fn rule_denial_returns_denied_and_records_event() {
    let repo = SpyRepo::new();
    let svc = proxy(
        "npm",
        Arc::new(DenyRegistry),
        repo.clone(),
        vec![Box::new(AlwaysDenyRule)],
    );

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Denied { reason, .. } if reason == "test denial"),
        "expected Denied response"
    );
    let events = repo.events();
    assert_eq!(events.len(), 1, "one denied event should be recorded");
    assert!(matches!(events[0].result, AccessResult::Denied { .. }));
}

#[tokio::test]
async fn artifact_cache_hit_returns_stored_bytes() {
    let storage = MemStorage::new();
    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let artifact_key = format!("artifact:{}", pkg.cache_key());
    // Pre-populate storage
    storage
        .store(
            &artifact_key,
            Bytes::from("cached!"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    let repo = SpyRepo::new();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(FixedRegistry),
            RegistryPolicy {
                metadata_ttl: None,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![],
            },
            None,
        ),
        storage: storage.clone(),
        cache: TestCacheStore::new(),
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    // Access event should be recorded for the cache hit
    assert!(!repo.events().is_empty(), "access event should be recorded");
}

#[tokio::test]
async fn artifact_cache_miss_fetches_from_upstream() {
    let repo = SpyRepo::new();
    let svc = proxy("npm", Arc::new(FixedRegistry), repo.clone(), vec![]);

    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let artifact_key = format!("artifact:{}", pkg.cache_key());

    // Storage is empty — must fetch from upstream
    assert!(!svc.storage.exists(&artifact_key).await.unwrap());

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));

    // Artifact should now be stored
    assert!(
        svc.storage.exists(&artifact_key).await.unwrap(),
        "artifact should be stored after fetch"
    );
    assert!(!repo.events().is_empty(), "access event should be recorded");
}

#[tokio::test]
async fn payload_too_large_returns_error() {
    let repo = SpyRepo::new();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(FixedRegistry),
            RegistryPolicy {
                metadata_ttl: None,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![],
            },
            Some(5), // FixedRegistry sends >5 bytes
        ),
        storage: MemStorage::new(),
        cache: TestCacheStore::new(),
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let result = svc.handle(req("npm")).await;
    assert!(matches!(result, Err(CoreError::PayloadTooLarge(_))));
}

#[tokio::test]
async fn unused_registry_id_in_policies_does_not_panic() {
    let repo = SpyRepo::new();
    // no policy for "npm" — should use empty rule set
    let svc = ProxyService {
        hot: empty_hot("npm", Arc::new(FixedRegistry)),
        storage: MemStorage::new(),
        cache: TestCacheStore::new(),
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
}

#[tokio::test]
async fn firewall_only_streams_without_storing() {
    let storage = MemStorage::new();
    let repo = SpyRepo::new();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(FixedRegistry),
            RegistryPolicy {
                metadata_ttl: None,
                firewall_only: true,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![],
            },
            None,
        ),
        storage: storage.clone(),
        cache: TestCacheStore::new(),
        repo: repo.clone(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let artifact_key = format!("artifact:{}", pkg.cache_key());

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    assert!(
        !storage.exists(&artifact_key).await.unwrap(),
        "firewall-only: artifact must not be stored"
    );
    assert!(!repo.events().is_empty(), "access event should be recorded");
}

struct UnavailableRegistry;

#[async_trait]
impl RegistryClient for UnavailableRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, _pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Err(CoreError::Registry("upstream down".into()))
    }
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let data = Bytes::from(format!("artifact:{}", pkg.cache_key()));
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: None,
        })
    }
}

fn proxy_with_stale(
    client: Arc<dyn RegistryClient>,
    repo: Arc<dyn PackageRepository>,
    cache: Arc<dyn CacheStore>,
    serve_stale: bool,
) -> ProxyService {
    ProxyService {
        hot: make_hot(
            "npm",
            client,
            RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: serve_stale,
                artifact_ttl: None,
                rules: vec![],
            },
            None,
        ),
        storage: MemStorage::new(),
        cache,
        repo,
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    }
}

#[tokio::test]
async fn stale_metadata_served_when_upstream_unavailable() {
    let repo = SpyRepo::new();
    let cache = TestCacheStore::new();
    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let cache_key = format!("meta:{}", pkg.cache_key());
    let stale_meta = PackageMetadata {
        id: pkg.clone(),
        published_at: Some(Utc::now() - chrono::Duration::days(10)),
        download_url: None,
        checksum: None,
        is_signed: None,
        extra: serde_json::json!({}),
        cache_control: None,
    };
    cache.seed_expired(&cache_key, stale_meta);

    let svc = proxy_with_stale(Arc::new(UnavailableRegistry), repo.clone(), cache, true);
    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "stale fallback should succeed"
    );
    assert!(
        repo.events()
            .iter()
            .all(|e| !matches!(e.result, AccessResult::ProxyError { .. })),
        "no proxy_error should be recorded when stale metadata is served"
    );
}

#[tokio::test]
async fn stale_not_used_when_serve_stale_false() {
    let repo = SpyRepo::new();
    let cache = TestCacheStore::new();
    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let cache_key = format!("meta:{}", pkg.cache_key());
    let stale_meta = PackageMetadata {
        id: pkg.clone(),
        published_at: Some(Utc::now() - chrono::Duration::days(10)),
        download_url: None,
        checksum: None,
        is_signed: None,
        extra: serde_json::json!({}),
        cache_control: None,
    };
    cache.seed_expired(&cache_key, stale_meta);

    let svc = proxy_with_stale(Arc::new(UnavailableRegistry), repo.clone(), cache, false);
    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::Registry(_))),
        "should propagate the upstream error"
    );
    assert!(
        repo.events()
            .iter()
            .any(|e| matches!(e.result, AccessResult::ProxyError { .. })),
        "proxy_error must be recorded"
    );
}

#[tokio::test]
async fn cold_start_with_upstream_down_returns_error() {
    let repo = SpyRepo::new();
    let cache = TestCacheStore::new(); // empty — no stale entry

    let svc = proxy_with_stale(Arc::new(UnavailableRegistry), repo.clone(), cache, true);
    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::Registry(_))),
        "no stale entry + upstream down must return error"
    );
}

#[tokio::test]
async fn not_found_from_upstream_is_not_stale_eligible() {
    struct NotFoundRegistry;
    #[async_trait]
    impl RegistryClient for NotFoundRegistry {
        fn registry_type(&self) -> &str {
            "test"
        }
        async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
            Err(CoreError::NotFound(pkg.name.clone()))
        }
        async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
            Err(CoreError::NotFound("no artifact".into()))
        }
    }

    let repo = SpyRepo::new();
    let cache = TestCacheStore::new();
    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let cache_key = format!("meta:{}", pkg.cache_key());
    let stale_meta = PackageMetadata {
        id: pkg.clone(),
        published_at: Some(Utc::now() - chrono::Duration::days(10)),
        download_url: None,
        checksum: None,
        is_signed: None,
        extra: serde_json::json!({}),
        cache_control: None,
    };
    cache.seed_expired(&cache_key, stale_meta);

    let svc = proxy_with_stale(Arc::new(NotFoundRegistry), repo.clone(), cache, true);
    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::NotFound(_))),
        "NotFound must not fall back to stale"
    );
}

// ── Cache-Control and ArtifactMeta integration tests ─────────────────────

struct NoStoreMetaRegistry;

#[async_trait]
impl RegistryClient for NoStoreMetaRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(1)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({}),
            cache_control: Some("no-store".to_owned()),
        })
    }
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let data = Bytes::from(format!("artifact:{}", pkg.cache_key()));
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: None,
        })
    }
}

struct NoStoreArtifactRegistry;

#[async_trait]
impl RegistryClient for NoStoreArtifactRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(1)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({}),
            cache_control: None,
        })
    }
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let data = Bytes::from(format!("artifact:{}", pkg.cache_key()));
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: Some("no-store".to_owned()),
        })
    }
}

#[tokio::test]
async fn metadata_no_store_skips_cache() {
    let repo = SpyRepo::new();
    let cache = TestCacheStore::new();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(NoStoreMetaRegistry),
            RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![],
            },
            None,
        ),
        storage: MemStorage::new(),
        cache: cache.clone(),
        repo,
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let cache_key = format!("meta:{}", req("npm").package_id.cache_key());
    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "response must still be a stream"
    );
    assert!(
        cache.get(&cache_key).await.unwrap().is_none(),
        "metadata must NOT be cached when upstream returns Cache-Control: no-store"
    );
}

#[tokio::test]
async fn artifact_no_store_skips_storage() {
    let repo = SpyRepo::new();
    let storage = MemStorage::new();
    let svc = ProxyService {
        hot: empty_hot("npm", Arc::new(NoStoreArtifactRegistry)),
        storage: storage.clone(),
        cache: TestCacheStore::new(),
        repo,
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let artifact_key = format!("artifact:{}", pkg.cache_key());

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "response must still be a stream"
    );
    assert!(
        !storage.exists(&artifact_key).await.unwrap(),
        "artifact must NOT be stored when upstream returns Cache-Control: no-store"
    );
}

#[tokio::test]
async fn artifact_ttl_expired_refetches_from_upstream() {
    let storage = MemStorage::new();
    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let artifact_key = format!("artifact:{}", pkg.cache_key());
    storage
        .store(
            &artifact_key,
            Bytes::from("stale-bytes"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    // Spy meta says artifact is expired
    let expired_meta = ArtifactMeta {
        artifact_key: artifact_key.clone(),
        registry: "npm".to_owned(),
        package_name: "test-pkg".to_owned(),
        version: "1.0.0".to_owned(),
        size_bytes: Some(11),
        cached_at: Utc::now() - chrono::Duration::hours(2),
        last_accessed_at: Utc::now() - chrono::Duration::hours(2),
    };
    let spy_meta = SpyArtifactMeta::with_expired(vec![expired_meta]);

    let repo = SpyRepo::new();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(FixedRegistry),
            RegistryPolicy {
                metadata_ttl: None,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: Some(Duration::from_secs(3600)), // 1h TTL
                rules: vec![],
            },
            None,
        ),
        storage: storage.clone(),
        cache: TestCacheStore::new(),
        repo,
        artifact_meta: spy_meta.clone(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    // After re-fetch, record_artifact should have been called
    assert!(
        !spy_meta.recorded_keys().is_empty(),
        "record_artifact must be called after re-fetch"
    );
    // Storage should now contain the freshly fetched artifact
    assert!(storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn artifact_cache_hit_records_touch() {
    let storage = MemStorage::new();
    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let artifact_key = format!("artifact:{}", pkg.cache_key());
    storage
        .store(
            &artifact_key,
            Bytes::from("cached!"),
            StorageMeta::default(),
        )
        .await
        .unwrap();

    // Pre-seed the artifact metadata to simulate a previous record_artifact call.
    // Without this, is_artifact_expired treats the missing row as expired (correct
    // production behavior) and the proxy re-fetches instead of serving from cache.
    let spy_meta = SpyArtifactMeta::new();
    spy_meta
        .record_artifact(ArtifactMetaRecord {
            key: &artifact_key,
            registry: "npm",
            package_name: "test-pkg",
            version: "1.0.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(FixedRegistry),
            RegistryPolicy {
                metadata_ttl: None,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: Some(Duration::from_secs(3600)),
                rules: vec![],
            },
            None,
        ),
        storage: storage.clone(),
        cache: TestCacheStore::new(),
        repo: SpyRepo::new(),
        artifact_meta: spy_meta.clone(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    // touch_artifact is called from tokio::spawn — yield to let it complete
    tokio::task::yield_now().await;
    assert!(
        spy_meta.touched_keys().contains(&artifact_key),
        "touch_artifact must be called on cache hit"
    );
}

#[tokio::test]
async fn artifact_cache_miss_records_meta() {
    let spy_meta = SpyArtifactMeta::new();
    let repo = SpyRepo::new();
    let svc = ProxyService {
        hot: empty_hot("npm", Arc::new(FixedRegistry)),
        storage: MemStorage::new(),
        cache: TestCacheStore::new(),
        repo,
        artifact_meta: spy_meta.clone(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let artifact_key = format!("artifact:{}", pkg.cache_key());

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    assert!(
        spy_meta.recorded_keys().contains(&artifact_key),
        "record_artifact must be called after a cache miss and successful upstream fetch"
    );
}

#[tokio::test]
async fn metrics_artifact_miss_then_hit() {
    let proxy_metrics = Arc::new(ProxyMetrics::new(&["npm".to_owned()]));
    let storage = MemStorage::new();
    let svc = ProxyService {
        hot: make_hot(
            "npm",
            Arc::new(FixedRegistry),
            RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![],
            },
            None,
        ),
        storage: storage.clone(),
        cache: TestCacheStore::new(),
        repo: SpyRepo::new(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: proxy_metrics.clone(),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let npm = proxy_metrics.all().get("npm").unwrap();

    // First call: artifact not in storage → miss counter incremented
    svc.handle(req("npm")).await.unwrap();
    assert_eq!(npm.misses(), 1, "first call must register a miss");
    assert_eq!(npm.hits(), 0);

    // Second call: artifact now in storage → hit counter incremented
    svc.handle(req("npm")).await.unwrap();
    assert_eq!(npm.misses(), 1, "miss count must not change on second call");
    assert_eq!(npm.hits(), 1, "second call must register a hit");
}

// ── Integrity verification ─────────────────────────────────────────────────

/// Registry whose metadata advertises a configurable checksum and whose
/// artifact body is fixed, so a test can force verified / mismatch / missing.
struct ChecksumRegistry {
    checksum: Option<String>,
    body: &'static [u8],
}

#[async_trait]
impl RegistryClient for ChecksumRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(30)),
            download_url: None,
            checksum: self.checksum.clone(),
            is_signed: None,
            extra: serde_json::json!({}),
            cache_control: None,
        })
    }
    async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let data = Bytes::from_static(self.body);
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: None,
        })
    }
}

/// Like [`ChecksumRegistry`], but the fetched artifact is `Cache-Control:
/// no-store`, so it's served via `ProxyService::serve_no_store` rather than
/// the streaming-verifier path — exercises integrity checking on that
/// separate code path.
struct NoStoreChecksumRegistry {
    checksum: Option<String>,
    body: &'static [u8],
}

#[async_trait]
impl RegistryClient for NoStoreChecksumRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(30)),
            download_url: None,
            checksum: self.checksum.clone(),
            is_signed: None,
            extra: serde_json::json!({}),
            cache_control: None,
        })
    }
    async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let data = Bytes::from_static(self.body);
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
            cache_control: Some("no-store".to_owned()),
        })
    }
}

/// Build a proxy whose single registry has the given integrity policy (when
/// `None`, the registry has no explicit block so the default policy applies).
/// Returns the service together with its storage so caching can be asserted.
fn proxy_with_integrity(
    registry_name: &str,
    client: Arc<dyn RegistryClient>,
    integrity: Option<crate::services::IntegrityPolicy>,
) -> (ProxyService, Arc<MemStorage>) {
    let mut registries: HashMap<String, Arc<dyn RegistryClient>> = HashMap::new();
    registries.insert(registry_name.to_owned(), client);
    let mut policies = HashMap::new();
    policies.insert(
        registry_name.to_owned(),
        Arc::new(RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            serve_stale_metadata: false,
            artifact_ttl: None,
            rules: vec![],
        }),
    );
    let mut integrity_map = HashMap::new();
    if let Some(i) = integrity {
        integrity_map.insert(registry_name.to_owned(), i);
    }
    let hot = new_hot_lock(HotConfig {
        registries,
        policies,
        integrity: integrity_map,
        ..Default::default()
    });
    let storage = MemStorage::new();
    let svc = ProxyService {
        hot,
        storage: storage.clone(),
        cache: TestCacheStore::new(),
        repo: SpyRepo::new(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };
    (svc, storage)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

const BODY: &[u8] = b"the-real-artifact-bytes";

#[tokio::test]
async fn integrity_verified_artifact_is_cached_and_served() {
    let reg = Arc::new(ChecksumRegistry {
        checksum: Some(sha256_hex(BODY)),
        body: BODY,
    });
    // No explicit policy → default (enabled, block-on-mismatch).
    let (svc, storage) = proxy_with_integrity("npm", reg, None);

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));

    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(
        storage.exists(&artifact_key).await.unwrap(),
        "verified artifact must be cached"
    );
    // The block-on-mismatch path streams to a private `staging:` key and promotes
    // only after verification; once served, no staging key must remain behind.
    let staging = storage.list_keys("staging:").await.unwrap();
    assert!(
        staging.is_empty(),
        "staging keys leaked after promotion: {staging:?}"
    );
}

#[tokio::test]
async fn integrity_mismatch_never_exposes_a_servable_key() {
    let reg = Arc::new(ChecksumRegistry {
        checksum: Some(sha256_hex(b"some-other-bytes")),
        body: BODY,
    });
    // Default policy blocks on mismatch.
    let (svc, storage) = proxy_with_integrity("npm", reg, None);

    let result = svc.handle(req("npm")).await;
    assert!(matches!(result, Err(CoreError::IntegrityFailure(_))));

    // Neither the real key nor any staging key should be left servable — the
    // mismatched bytes were only ever under the private staging key, now evicted.
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(!storage.exists(&artifact_key).await.unwrap());
    let leaked = storage.list_keys("staging:").await.unwrap();
    assert!(
        leaked.is_empty(),
        "mismatch left staging keys behind: {leaked:?}"
    );
}

#[tokio::test]
async fn integrity_mismatch_blocks_and_is_not_cached() {
    let reg = Arc::new(ChecksumRegistry {
        // Advertise the checksum of *different* bytes → mismatch.
        checksum: Some(sha256_hex(b"some-other-bytes")),
        body: BODY,
    });
    let (svc, storage) = proxy_with_integrity("npm", reg, None);

    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::IntegrityFailure(_))),
        "mismatch must fail the download"
    );

    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(
        !storage.exists(&artifact_key).await.unwrap(),
        "bytes that fail verification must never be cached"
    );
}

#[tokio::test]
async fn integrity_missing_metadata_warns_and_serves_by_default() {
    let reg = Arc::new(ChecksumRegistry {
        checksum: None,
        body: BODY,
    });
    // Default policy: require_metadata = false → missing only warns.
    let (svc, storage) = proxy_with_integrity("npm", reg, None);

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn integrity_require_metadata_blocks_when_absent() {
    let reg = Arc::new(ChecksumRegistry {
        checksum: None,
        body: BODY,
    });
    let policy = crate::services::IntegrityPolicy {
        enabled: true,
        block_on_mismatch: true,
        require_metadata: true,
        bypass_roles: vec![],
        verify_on_serve: false,
    };
    let (svc, storage) = proxy_with_integrity("npm", reg, Some(policy));

    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::IntegrityFailure(_))),
        "require_metadata must block a checksum-less download"
    );
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(!storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn integrity_require_metadata_bypass_role_is_allowed() {
    let reg = Arc::new(ChecksumRegistry {
        checksum: None,
        body: BODY,
    });
    let policy = crate::services::IntegrityPolicy {
        enabled: true,
        block_on_mismatch: true,
        require_metadata: true,
        bypass_roles: vec![crate::entities::Role::Admin],
        verify_on_serve: false,
    };
    let (svc, _storage) = proxy_with_integrity("npm", reg, Some(policy));

    let admin_req = ProxyRequest {
        package_id: PackageId::new("npm", "test-pkg", "1.0.0"),
        identity: Identity {
            user_id: None,
            role: crate::entities::Role::Admin,
            auth_provider: None,
            groups: vec![],
        },
        action: Action::ReleasesRead,
        ip_address: None,
        user_agent: None,
    };
    let resp = svc.handle(admin_req).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "a bypass-role caller must be served despite missing metadata"
    );
}

#[tokio::test]
async fn integrity_mismatch_warn_only_serves_and_caches() {
    let reg = Arc::new(ChecksumRegistry {
        checksum: Some(sha256_hex(b"some-other-bytes")),
        body: BODY,
    });
    // block_on_mismatch = false → warn but do not block; bytes are still cached.
    let policy = crate::services::IntegrityPolicy {
        enabled: true,
        block_on_mismatch: false,
        require_metadata: false,
        bypass_roles: vec![],
        verify_on_serve: false,
    };
    let (svc, storage) = proxy_with_integrity("npm", reg, Some(policy));

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "warn-only mismatch must still serve the artifact"
    );
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn integrity_no_store_matching_checksum_serves() {
    let reg = Arc::new(NoStoreChecksumRegistry {
        checksum: Some(sha256_hex(BODY)),
        body: BODY,
    });
    let policy = crate::services::IntegrityPolicy {
        enabled: true,
        block_on_mismatch: true,
        require_metadata: false,
        bypass_roles: vec![],
        verify_on_serve: false,
    };
    let (svc, _storage) = proxy_with_integrity("npm", reg, Some(policy));

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "a matching checksum on the no-store path must serve normally"
    );
}

#[tokio::test]
async fn integrity_no_store_mismatch_blocks() {
    let reg = Arc::new(NoStoreChecksumRegistry {
        checksum: Some(sha256_hex(b"some-other-bytes")),
        body: BODY,
    });
    let policy = crate::services::IntegrityPolicy {
        enabled: true,
        block_on_mismatch: true,
        require_metadata: false,
        bypass_roles: vec![],
        verify_on_serve: false,
    };
    let (svc, storage) = proxy_with_integrity("npm", reg, Some(policy));

    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::IntegrityFailure(_))),
        "a checksum mismatch on the no-store path must block, same as the streaming path"
    );
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(
        !storage.exists(&artifact_key).await.unwrap(),
        "no-store artifacts are never written to storage regardless of outcome"
    );
}

#[tokio::test]
async fn integrity_unparseable_checksum_warns_and_serves() {
    let reg = Arc::new(ChecksumRegistry {
        // Not SRI and not a known-length hex string → Unparseable.
        checksum: Some("not-a-real-checksum".to_owned()),
        body: BODY,
    });
    // Default policy: an unverifiable checksum is treated like missing (serve).
    let (svc, storage) = proxy_with_integrity("npm", reg, None);

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn integrity_disabled_skips_verification_even_on_mismatch() {
    let reg = Arc::new(ChecksumRegistry {
        checksum: Some(sha256_hex(b"some-other-bytes")),
        body: BODY,
    });
    let policy = crate::services::IntegrityPolicy {
        enabled: false,
        block_on_mismatch: true,
        require_metadata: false,
        bypass_roles: vec![],
        verify_on_serve: false,
    };
    let (svc, storage) = proxy_with_integrity("npm", reg, Some(policy));

    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "disabled integrity must not block a mismatching artifact"
    );
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    assert!(storage.exists(&artifact_key).await.unwrap());
}

// ── Re-serve verification (verify_on_serve) ────────────────────────────────

/// Build a ProxyService whose cached artifact (`storage`) and recorded checksum
/// (`spy_meta`) can be set up independently, so a test can simulate a cached
/// artifact whose stored bytes no longer match the checksum recorded at cache time.
fn reverify_proxy(
    verify_on_serve: bool,
    spy_meta: Arc<SpyArtifactMeta>,
    storage: Arc<MemStorage>,
) -> ProxyService {
    let mut registries: HashMap<String, Arc<dyn RegistryClient>> = HashMap::new();
    registries.insert("npm".to_owned(), Arc::new(FixedRegistry));
    let mut policies = HashMap::new();
    policies.insert(
        "npm".to_owned(),
        Arc::new(RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            serve_stale_metadata: false,
            artifact_ttl: Some(Duration::from_secs(3600)),
            rules: vec![],
        }),
    );
    let mut integrity_map = HashMap::new();
    integrity_map.insert(
        "npm".to_owned(),
        crate::services::IntegrityPolicy {
            enabled: true,
            block_on_mismatch: true,
            require_metadata: false,
            bypass_roles: vec![],
            verify_on_serve,
        },
    );
    let hot = new_hot_lock(HotConfig {
        registries,
        policies,
        integrity: integrity_map,
        ..Default::default()
    });
    ProxyService {
        hot,
        storage,
        cache: TestCacheStore::new(),
        repo: SpyRepo::new(),
        artifact_meta: spy_meta,
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    }
}

#[tokio::test]
async fn reverify_serves_when_stored_bytes_match() {
    let storage = MemStorage::new();
    let spy_meta = SpyArtifactMeta::new();
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    storage
        .store(
            &artifact_key,
            Bytes::from_static(BODY),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    spy_meta
        .record_artifact(ArtifactMetaRecord {
            key: &artifact_key,
            registry: "npm",
            package_name: "test-pkg",
            version: "1.0.0",
            size: None,
            checksum: Some(&sha256_hex(BODY)),
        })
        .await
        .unwrap();

    let svc = reverify_proxy(true, spy_meta, storage);
    let resp = svc.handle(req("npm")).await.unwrap();
    let ProxyResponse::Stream(mut s) = resp else {
        panic!("matching stored bytes must be served on re-verify");
    };
    // Drain the re-opened stream: it must yield exactly the verified bytes.
    use futures::StreamExt as _;
    let mut served = Vec::new();
    while let Some(chunk) = s.next().await {
        served.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(
        served, BODY,
        "re-served bytes must match the verified content"
    );
}

#[tokio::test]
async fn reverify_blocks_and_evicts_corrupted_cache() {
    let storage = MemStorage::new();
    let spy_meta = SpyArtifactMeta::new();
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    // Stored bytes are corrupted, but the recorded checksum is for the real bytes.
    storage
        .store(
            &artifact_key,
            Bytes::from_static(b"corrupted!"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    spy_meta
        .record_artifact(ArtifactMetaRecord {
            key: &artifact_key,
            registry: "npm",
            package_name: "test-pkg",
            version: "1.0.0",
            size: None,
            checksum: Some(&sha256_hex(BODY)),
        })
        .await
        .unwrap();

    let svc = reverify_proxy(true, spy_meta, storage.clone());
    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::IntegrityFailure(_))),
        "a corrupted cached artifact must fail re-serve verification"
    );
    assert!(
        !storage.exists(&artifact_key).await.unwrap(),
        "the corrupt cache entry must be evicted"
    );
}

#[tokio::test]
async fn reverify_off_serves_corrupted_bytes() {
    let storage = MemStorage::new();
    let spy_meta = SpyArtifactMeta::new();
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    storage
        .store(
            &artifact_key,
            Bytes::from_static(b"corrupted!"),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    spy_meta
        .record_artifact(ArtifactMetaRecord {
            key: &artifact_key,
            registry: "npm",
            package_name: "test-pkg",
            version: "1.0.0",
            size: None,
            checksum: Some(&sha256_hex(BODY)),
        })
        .await
        .unwrap();

    // verify_on_serve = false → no re-check, the (corrupt) bytes are served as before.
    let svc = reverify_proxy(false, spy_meta, storage.clone());
    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(matches!(resp, ProxyResponse::Stream(_)));
    assert!(storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn reverify_fails_closed_on_checksum_lookup_error() {
    let storage = MemStorage::new();
    let spy_meta = SpyArtifactMeta::new();
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    // Bytes are fine and a checksum was recorded, but the lookup itself errors
    // (transient metadata-store failure). verify_on_serve must fail closed rather
    // than serve bytes it could not re-verify.
    storage
        .store(
            &artifact_key,
            Bytes::from_static(BODY),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    spy_meta
        .record_artifact(ArtifactMetaRecord {
            key: &artifact_key,
            registry: "npm",
            package_name: "test-pkg",
            version: "1.0.0",
            size: None,
            checksum: Some(&sha256_hex(BODY)),
        })
        .await
        .unwrap();
    spy_meta.fail_checksum_lookups();

    let svc = reverify_proxy(true, spy_meta, storage.clone());
    let result = svc.handle(req("npm")).await;
    assert!(
        matches!(result, Err(CoreError::IntegrityFailure(_))),
        "a checksum-lookup error must fail closed, not serve unverified bytes"
    );
    // The cache entry is left intact — the bytes may be fine; we just couldn't check.
    assert!(storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn reverify_serves_when_no_checksum_recorded() {
    let storage = MemStorage::new();
    let spy_meta = SpyArtifactMeta::new();
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    storage
        .store(
            &artifact_key,
            Bytes::from_static(BODY),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    // Recorded with no checksum (entry cached before verify_on_serve existed):
    // documented skip — serve as-is until the entry is next refreshed.
    spy_meta
        .record_artifact(ArtifactMetaRecord {
            key: &artifact_key,
            registry: "npm",
            package_name: "test-pkg",
            version: "1.0.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();

    let svc = reverify_proxy(true, spy_meta, storage.clone());
    let resp = svc.handle(req("npm")).await.unwrap();
    assert!(
        matches!(resp, ProxyResponse::Stream(_)),
        "an entry with no recorded checksum is served (skip), not blocked"
    );
    assert!(storage.exists(&artifact_key).await.unwrap());
}

#[tokio::test]
async fn reverify_serves_oversized_artifact_via_reretrieve() {
    // A body larger than REVERIFY_BUFFER_LIMIT is hashed by streaming (its
    // bytes are not retained) and then served by re-opening a fresh stream from
    // storage — the bounded-memory fallback path.
    let body = vec![0xABu8; super::handle::REVERIFY_BUFFER_LIMIT + 1];
    let storage = MemStorage::new();
    let spy_meta = SpyArtifactMeta::new();
    let artifact_key = format!("artifact:{}", req("npm").package_id.cache_key());
    storage
        .store(
            &artifact_key,
            Bytes::from(body.clone()),
            StorageMeta::default(),
        )
        .await
        .unwrap();
    spy_meta
        .record_artifact(ArtifactMetaRecord {
            key: &artifact_key,
            registry: "npm",
            package_name: "test-pkg",
            version: "1.0.0",
            size: None,
            checksum: Some(&sha256_hex(&body)),
        })
        .await
        .unwrap();

    let svc = reverify_proxy(true, spy_meta, storage);
    let resp = svc.handle(req("npm")).await.unwrap();
    let ProxyResponse::Stream(mut s) = resp else {
        panic!("oversized verified artifact must be served");
    };
    use futures::StreamExt as _;
    let mut served_len = 0usize;
    let mut first = None;
    while let Some(chunk) = s.next().await {
        let chunk = chunk.unwrap();
        if first.is_none() {
            first = chunk.first().copied();
        }
        served_len += chunk.len();
    }
    // The re-retrieved stream must deliver the complete artifact.
    assert_eq!(
        served_len,
        body.len(),
        "oversized artifact must serve in full"
    );
    assert_eq!(
        first,
        Some(0xAB),
        "re-served bytes must match the stored content"
    );
}

// ── resolve_metadata_for (metadata-only entry point) ──────────────────────

/// Counts `resolve_metadata` calls so cache hits are observable.
struct CountingRegistry {
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl RegistryClient for CountingRegistry {
    fn registry_type(&self) -> &str {
        "test"
    }
    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(30)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({"marker": "from-upstream"}),
            cache_control: None,
        })
    }
    async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        Err(CoreError::Registry("metadata-only test".into()))
    }
}

#[tokio::test]
async fn resolve_metadata_for_miss_fetches_then_serves_from_cache() {
    let client = CountingRegistry::new();
    let svc = ProxyService {
        hot: make_hot(
            "jbm",
            client.clone(),
            RegistryPolicy {
                metadata_ttl: Some(Duration::from_secs(300)),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![],
            },
            None,
        ),
        storage: MemStorage::new(),
        cache: TestCacheStore::new(),
        repo: SpyRepo::new(),
        artifact_meta: NoopArtifactMeta::arc(),
        metrics: Arc::new(ProxyMetrics::new(&[])),
        sbom: None,
        readme: None,
        discovery: Default::default(),
    };

    let meta = svc.resolve_metadata_for(&req("jbm")).await.unwrap();
    assert_eq!(meta.extra["marker"], "from-upstream");
    assert_eq!(client.calls(), 1);

    // Second resolution must come from the metadata cache, not upstream.
    let meta = svc.resolve_metadata_for(&req("jbm")).await.unwrap();
    assert_eq!(meta.extra["marker"], "from-upstream");
    assert_eq!(client.calls(), 1, "second call must be a cache hit");
}

#[tokio::test]
async fn resolve_metadata_for_serves_stale_on_upstream_error() {
    let cache = TestCacheStore::new();
    let pkg = PackageId::new("npm", "test-pkg", "1.0.0");
    let cache_key = format!("meta:{}", pkg.cache_key());
    cache.seed_expired(
        &cache_key,
        PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now() - chrono::Duration::days(10)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({"marker": "stale"}),
            cache_control: None,
        },
    );

    let svc = proxy_with_stale(Arc::new(UnavailableRegistry), SpyRepo::new(), cache, true);
    let meta = svc.resolve_metadata_for(&req("npm")).await.unwrap();
    assert_eq!(meta.extra["marker"], "stale");
}

#[tokio::test]
async fn resolve_metadata_for_upstream_error_without_stale_propagates() {
    let svc = proxy_with_stale(
        Arc::new(UnavailableRegistry),
        SpyRepo::new(),
        TestCacheStore::new(),
        false,
    );
    let result = svc.resolve_metadata_for(&req("npm")).await;
    assert!(matches!(result, Err(CoreError::Registry(_))));
}

#[tokio::test]
async fn resolve_metadata_for_denied_by_rules() {
    let svc = proxy(
        "npm",
        Arc::new(FixedRegistry),
        SpyRepo::new(),
        vec![Box::new(AlwaysDenyRule)],
    );
    let result = svc.resolve_metadata_for(&req("npm")).await;
    assert!(matches!(result, Err(CoreError::AccessDenied(_))));
}

#[tokio::test]
async fn resolve_metadata_for_rejects_traversal() {
    let svc = proxy("npm", Arc::new(FixedRegistry), SpyRepo::new(), vec![]);
    let bad = ProxyRequest {
        package_id: PackageId::new("npm", "../../etc/passwd", "1.0.0"),
        identity: Identity::anonymous(),
        action: Action::ReleasesRead,
        ip_address: None,
        user_agent: None,
    };
    assert!(matches!(
        svc.resolve_metadata_for(&bad).await,
        Err(CoreError::InvalidInput(_))
    ));
}

// ── Cached passthrough: the three rungs (RFC 0009 §4.2) ───────────────────────
//
// The endpoints that bypass `handle` — `npm audit`, the Go vulnerability
// database, the checksum log — used to make a bare outbound call with no cache
// at all, so each failed outright the moment its upstream went away. These pin
// the rungs that fixed it, and the two cases that must *not* fall back.

mod passthrough_rungs {
    use super::*;
    use crate::services::proxy::{FetchOutcome, Freshness, UpstreamBytes};
    use base64::Engine as _;

    /// Returns the service *and* the concrete cache, because only the concrete
    /// type can seed an already-expired entry — which is the whole point of the
    /// rung-3 tests.
    fn svc(serve_stale: bool) -> (ProxyService, Arc<TestCacheStore>) {
        let policy = RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            serve_stale_metadata: serve_stale,
            artifact_ttl: None,
            rules: vec![],
        };
        let cache = TestCacheStore::new();
        let svc = ProxyService {
            hot: make_hot("npm1", Arc::new(FixedRegistry), policy, None),
            storage: MemStorage::new(),
            cache: cache.clone(),
            repo: SpyRepo::new(),
            artifact_meta: NoopArtifactMeta::arc(),
            metrics: Arc::new(ProxyMetrics::new(&[])),
            sbom: None,
            readme: None,
            discovery: Default::default(),
        };
        (svc, cache)
    }

    fn bytes(s: &str) -> UpstreamBytes {
        UpstreamBytes::json(s.as_bytes().to_vec())
    }

    fn stale_entry(key: &str, body: &str) -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("npm1", key, ""),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({
                "passthrough": {
                    "content_type": "application/json",
                    "body_b64": base64::engine::general_purpose::STANDARD.encode(body.as_bytes()),
                }
            }),
            cache_control: None,
        }
    }

    #[tokio::test]
    async fn rung_2_fetches_upstream_then_rung_1_serves_it_without_asking_again() {
        let (s, _cache) = svc(false);
        let first = s
            .cached_passthrough("npm1", "audit:npm1:abc", None, || async {
                Ok(FetchOutcome::Cacheable(bytes(r#"{"advisories":[]}"#)))
            })
            .await
            .unwrap();
        assert_eq!(first.freshness, Freshness::Fresh);

        // The closure now panics: if it runs, the cache did not answer.
        let second = s
            .cached_passthrough("npm1", "audit:npm1:abc", None, || async {
                panic!("upstream must not be asked twice for one cached question")
            })
            .await
            .unwrap();
        assert_eq!(second.freshness, Freshness::Cached);
        assert_eq!(second.bytes, bytes(r#"{"advisories":[]}"#));
    }

    /// The reason this exists: an unreachable advisory database must not stop
    /// the pipeline that is asking about it.
    #[tokio::test]
    async fn rung_3_answers_from_stale_when_upstream_is_unreachable() {
        let (s, cache) = svc(true);
        cache.seed_expired(
            "audit:npm1:abc",
            stale_entry("audit:npm1:abc", r#"{"advisories":["stale"]}"#),
        );

        let got = s
            .cached_passthrough("npm1", "audit:npm1:abc", None, || async {
                Err(CoreError::Registry("connection refused".into()))
            })
            .await
            .unwrap();
        assert_eq!(got.freshness, Freshness::Stale);
        assert_eq!(got.bytes, bytes(r#"{"advisories":["stale"]}"#));
    }

    /// `serve_stale = false` is a deliberate operator choice — for their estate
    /// a stale answer is worse than none — and it governs this path too, so
    /// nobody has to discover a second switch.
    #[tokio::test]
    async fn rung_3_is_declined_when_the_registry_disallows_stale() {
        let (s, cache) = svc(false);
        cache.seed_expired("audit:npm1:abc", stale_entry("audit:npm1:abc", "{}"));

        let got = s
            .cached_passthrough("npm1", "audit:npm1:abc", None, || async {
                Err(CoreError::Registry("connection refused".into()))
            })
            .await;
        assert!(
            got.is_err(),
            "stale must not be served when the registry's policy forbids it"
        );
    }

    /// An upstream that is up and says `404` has answered. Serving a stale
    /// `200` over it would be inventing data, not surviving an outage.
    #[tokio::test]
    async fn a_definite_non_success_is_forwarded_and_never_cached() {
        let (s, _cache) = svc(true);
        let got = s
            .cached_passthrough("npm1", "audit:npm1:missing", None, || async {
                Ok(FetchOutcome::Definite {
                    status: 404,
                    bytes: bytes(r#"{"error":"not found"}"#),
                })
            })
            .await
            .unwrap();
        assert_eq!(got.status, 404);
        assert_eq!(got.freshness, Freshness::Fresh);

        // Not cached: the next call reaches the closure again.
        let second = s
            .cached_passthrough("npm1", "audit:npm1:missing", None, || async {
                Ok(FetchOutcome::Cacheable(bytes(r#"{"advisories":[]}"#)))
            })
            .await
            .unwrap();
        assert_eq!(
            second.freshness,
            Freshness::Fresh,
            "a 404 must not have been stored as if it were an answer worth keeping"
        );
    }
}

// ── Search: the rung that answers when the upstream is gone (RFC 0009 §7.7) ──
//
// Rungs 1 and 2 are the ordinary cache. Rung 3b is the one that makes search
// defensible enough for NuGet's service index to keep advertising it: when the
// upstream cannot be reached, the answer is the packages this registry already
// holds — never an error, and never the empty list that shipped before.

mod search_rungs {
    use super::*;
    use crate::entities::{PackageStatus, PackageSummary};
    use crate::services::SearchMode;

    /// A repository that holds two packages, so rung 3b has something to say.
    struct HeldRepo;

    #[async_trait]
    impl PackageRepository for HeldRepo {
        async fn record_access(&self, _e: crate::entities::AccessEvent) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get_status(&self, _pkg: &PackageId) -> Result<PackageStatus, CoreError> {
            Ok(PackageStatus::Available)
        }
        async fn set_status(
            &self,
            _pkg: &PackageId,
            _status: PackageStatus,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn count_packages(&self, _filter: PackageFilter) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn list_events(
            &self,
            _filter: EventFilter,
        ) -> Result<Vec<crate::entities::AccessEvent>, CoreError> {
            Ok(vec![])
        }
        async fn count_events(&self, _filter: EventFilter) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn delete_package(&self, _pkg: &PackageId) -> Result<bool, CoreError> {
            Ok(false)
        }
        async fn list_packages(
            &self,
            filter: PackageFilter,
        ) -> Result<Vec<PackageSummary>, CoreError> {
            // `blocked_only` matters: the default `blocked_in_registry` derives
            // the registry's blocked set from this very query, so a stub that
            // ignores the flag reports every held package as blocked — and the
            // search filter then correctly removes all of them.
            if filter.blocked_only {
                return Ok(vec![]);
            }
            let held = [("held-alpha", "1.0.0"), ("other-beta", "2.0.0")];
            Ok(held
                .iter()
                .filter(|(name, _)| {
                    filter
                        .name_contains
                        .as_deref()
                        .is_none_or(|q| name.contains(q))
                })
                .map(|(name, version)| PackageSummary {
                    id: uuid::Uuid::nil(),
                    package_id: PackageId::new("r1", *name, *version),
                    status: PackageStatus::Available,
                    last_accessed: None,
                    last_accessed_by: None,
                    access_count: 0,
                })
                .collect())
        }
    }

    fn svc_with_unreachable_upstream() -> ProxyService {
        let policy = RegistryPolicy {
            metadata_ttl: None,
            firewall_only: false,
            // Stale is allowed, but there is nothing stale cached — so the only
            // rung left is the held set.
            serve_stale_metadata: true,
            artifact_ttl: None,
            rules: vec![],
        };
        ProxyService {
            hot: make_hot("r1", Arc::new(UnavailableSearch), policy, None),
            storage: MemStorage::new(),
            cache: TestCacheStore::new(),
            repo: Arc::new(HeldRepo),
            artifact_meta: NoopArtifactMeta::arc(),
            metrics: Arc::new(ProxyMetrics::new(&[])),
            sbom: None,
            readme: None,
            discovery: Default::default(),
        }
    }

    /// Like `UnavailableRegistry`, but the failure that matters here is search.
    struct UnavailableSearch;

    #[async_trait]
    impl RegistryClient for UnavailableSearch {
        fn registry_type(&self) -> &str {
            "npm"
        }
        async fn resolve_metadata(&self, _pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
            Err(CoreError::Registry("upstream down".into()))
        }
        async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
            Err(CoreError::Registry("upstream down".into()))
        }
        async fn search_packages(
            &self,
            _query: &str,
            _limit: usize,
        ) -> Result<Vec<crate::ports::UpstreamPackage>, CoreError> {
            Err(CoreError::Registry("search upstream unreachable".into()))
        }
    }

    #[tokio::test]
    async fn an_unreachable_upstream_answers_from_held_packages() {
        let svc = svc_with_unreachable_upstream();
        let results = svc
            .search(
                "r1",
                "held",
                20,
                SearchMode::Proxy,
                crate::services::search::LocalSearch::empty(),
            )
            .await
            .expect("search must not fail when the upstream is unreachable");

        let names: Vec<&str> = results.hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(
            names,
            ["held-alpha"],
            "the answer must be what this registry holds, not an empty list"
        );
        assert_eq!(results.total, 1, "the total counts what survived");
        assert_eq!(
            results.freshness,
            Freshness::Stale,
            "a degraded answer must say so, or the UI shows a short list as complete"
        );
    }

    /// What a local registry has published. In production this comes from
    /// `LocalRegistryBackend` via the handler, not from `PackageRepository` —
    /// the two are different stores (see `ProxyService::search`).
    fn held_hits() -> crate::services::search::LocalSearch {
        let hits: Vec<crate::services::SearchHit> = ["held-alpha", "other-beta"]
            .iter()
            .map(|n| crate::services::SearchHit {
                name: (*n).to_owned(),
                version: "1.0.0".to_owned(),
                description: None,
            })
            .collect();
        crate::services::search::LocalSearch {
            all_names: hits.iter().map(|h| h.name.clone()).collect(),
            hits,
        }
    }

    /// Local mode has no upstream and nothing proxied through, so published
    /// packages are the whole answer.
    #[tokio::test]
    async fn local_mode_searches_only_what_is_held() {
        let svc = svc_with_unreachable_upstream();
        let results = svc
            .search("r1", "", 20, SearchMode::Local, held_hits())
            .await
            .unwrap();
        let mut names: Vec<&str> = results.hits.iter().map(|h| h.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["held-alpha", "other-beta"]);
    }

    /// An empty result set is a legitimate answer; what must never happen is an
    /// error reaching the client because the upstream was down.
    #[tokio::test]
    async fn a_query_matching_nothing_is_an_empty_answer_not_a_failure() {
        let svc = svc_with_unreachable_upstream();
        let results = svc
            .search(
                "r1",
                "no-such-package",
                20,
                SearchMode::Proxy,
                crate::services::search::LocalSearch::empty(),
            )
            .await
            .expect("still not an error");
        assert!(results.hits.is_empty());
        assert_eq!(results.total, 0);
    }
}

// ── README capture on the resolve path (RFC 0007 §5.1) ────────────────────────

mod readme_capture {
    use super::*;
    use crate::entities::{MetadataReadme, PackageReadme, ReadmeFormat, ReadmeSource};
    use crate::ports::ReadmeRepository;
    use crate::services::hot_config::ReadmeConfig;
    use crate::services::ReadmeService;
    use std::sync::Mutex;

    /// A registry whose metadata document carries a README, and one that
    /// carries only a link to one.
    struct ReadmeRegistry {
        linked: bool,
        /// How many times the linked README was actually read.
        linked_reads: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl RegistryClient for ReadmeRegistry {
        fn registry_type(&self) -> &str {
            "test"
        }

        async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
            let found = if self.linked {
                MetadataReadme::linked("https://upstream.invalid/README.md", ReadmeFormat::Markdown)
            } else {
                MetadataReadme::text("# from the packument", ReadmeFormat::Markdown)
            };
            Ok(PackageMetadata {
                id: pkg.clone(),
                published_at: Some(Utc::now() - chrono::Duration::days(30)),
                download_url: None,
                checksum: None,
                is_signed: None,
                extra: serde_json::json!({ "readme": found }),
                cache_control: None,
            })
        }

        async fn fetch_linked_readme(
            &self,
            _url: &str,
            _max_bytes: usize,
        ) -> Result<Option<String>, CoreError> {
            *self.linked_reads.lock().unwrap() += 1;
            Ok(Some("# fetched from the link".to_owned()))
        }

        async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
            let data = Bytes::from(format!("artifact:{}", pkg.cache_key()));
            Ok(FetchedArtifact {
                stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(data) })),
                cache_control: None,
            })
        }
    }

    #[derive(Default)]
    pub(super) struct RecordingRepo {
        pub(super) rows: Mutex<Vec<PackageReadme>>,
    }

    #[async_trait]
    impl ReadmeRepository for RecordingRepo {
        async fn upsert(&self, readme: PackageReadme) -> Result<(), CoreError> {
            self.rows.lock().unwrap().push(readme);
            Ok(())
        }
        async fn get(
            &self,
            registry: &str,
            name: &str,
            version: &str,
        ) -> Result<Option<PackageReadme>, CoreError> {
            Ok(self
                .rows
                .lock()
                .unwrap()
                .iter()
                .find(|r| r.registry == registry && r.name == name && r.version == version)
                .cloned())
        }
        async fn get_latest_with_readme(
            &self,
            _registry: &str,
            _name: &str,
            _exclude_versions: &[String],
        ) -> Result<Option<PackageReadme>, CoreError> {
            Ok(None)
        }
        async fn list_versions_with_readme(
            &self,
            _registry: &str,
            _name: &str,
        ) -> Result<Vec<String>, CoreError> {
            Ok(vec![])
        }
        async fn delete_for_version(
            &self,
            _registry: &str,
            _name: &str,
            _version: &str,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn delete_for_package(&self, _registry: &str, _name: &str) -> Result<(), CoreError> {
            Ok(())
        }
        async fn search(
            &self,
            _registries: &[String],
            _query: &str,
            _limit: u64,
        ) -> Result<Vec<crate::ports::ReadmeSearchHit>, CoreError> {
            Ok(vec![])
        }
    }

    fn proxy_with_readme(
        client: Arc<dyn RegistryClient>,
        repo: Arc<RecordingRepo>,
        cfg: Option<ReadmeConfig>,
    ) -> ProxyService {
        let mut hot = HotConfig {
            registries: HashMap::from([("r1".to_owned(), client)]),
            policies: HashMap::from([(
                "r1".to_owned(),
                Arc::new(RegistryPolicy {
                    metadata_ttl: None,
                    firewall_only: false,
                    serve_stale_metadata: false,
                    artifact_ttl: None,
                    rules: vec![],
                }),
            )]),
            ..Default::default()
        };
        if let Some(cfg) = cfg {
            hot.readme.insert("r1".to_owned(), cfg);
        }
        ProxyService {
            hot: new_hot_lock(hot),
            storage: MemStorage::new(),
            cache: TestCacheStore::new(),
            repo: SpyRepo::new(),
            artifact_meta: NoopArtifactMeta::arc(),
            metrics: Arc::new(ProxyMetrics::new(&["r1".to_owned()])),
            sbom: None,
            readme: Some(Arc::new(ReadmeService::new(
                repo as Arc<dyn ReadmeRepository>,
            ))),
            discovery: Default::default(),
        }
    }

    /// Capture is a detached task, so a test has to wait for it. Bounded, and
    /// it fails the test rather than hanging if nothing ever lands.
    async fn wait_for_row(repo: &RecordingRepo) -> Option<PackageReadme> {
        for _ in 0..200 {
            if let Some(row) = repo.get("r1", "test-pkg", "1.0.0").await.unwrap() {
                return Some(row);
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        None
    }

    /// The document has just been parsed and the README is a field of it, so
    /// resolving a version stores it — without the package manager's request
    /// waiting on the write.
    #[tokio::test]
    async fn resolving_a_version_records_the_readme_its_metadata_carried() {
        let repo = Arc::new(RecordingRepo::default());
        let svc = proxy_with_readme(
            Arc::new(ReadmeRegistry {
                linked: false,
                linked_reads: Arc::new(Mutex::new(0)),
            }),
            Arc::clone(&repo),
            None,
        );

        svc.handle(req("r1")).await.unwrap();

        let row = wait_for_row(&repo).await.expect("README recorded");
        assert_eq!(row.content, "# from the packument");
        assert_eq!(row.source, ReadmeSource::UpstreamMetadata);
        assert_eq!(row.format, ReadmeFormat::Markdown);
    }

    /// A linked README is followed — but off the resolve path, in the same
    /// detached task, and it is recorded as having come from the upstream's own
    /// answer rather than from bytes we opened.
    #[tokio::test]
    async fn a_linked_readme_is_followed_off_the_resolve_path() {
        let reads = Arc::new(Mutex::new(0));
        let repo = Arc::new(RecordingRepo::default());
        let svc = proxy_with_readme(
            Arc::new(ReadmeRegistry {
                linked: true,
                linked_reads: Arc::clone(&reads),
            }),
            Arc::clone(&repo),
            None,
        );

        svc.handle(req("r1")).await.unwrap();

        let row = wait_for_row(&repo).await.expect("README recorded");
        assert_eq!(row.content, "# fetched from the link");
        assert_eq!(row.source, ReadmeSource::UpstreamMetadata);
        assert_eq!(*reads.lock().unwrap(), 1);
    }

    /// A registry with the feature turned off stores nothing and, for a linked
    /// README, makes no outbound request at all.
    #[tokio::test]
    async fn a_disabled_registry_records_nothing_and_fetches_nothing() {
        let reads = Arc::new(Mutex::new(0));
        let repo = Arc::new(RecordingRepo::default());
        let svc = proxy_with_readme(
            Arc::new(ReadmeRegistry {
                linked: true,
                linked_reads: Arc::clone(&reads),
            }),
            Arc::clone(&repo),
            Some(ReadmeConfig {
                enabled: false,
                ..ReadmeConfig::default()
            }),
        );

        svc.handle(req("r1")).await.unwrap();

        // Give the task that would have run a chance to run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(repo.rows.lock().unwrap().is_empty());
        assert_eq!(*reads.lock().unwrap(), 0);
    }

    /// A cache hit returns before the capture, so a second request within the
    /// TTL neither re-reads the document nor rewrites the row.
    #[tokio::test]
    async fn a_metadata_cache_hit_does_not_re_record() {
        let repo = Arc::new(RecordingRepo::default());
        let svc = proxy_with_readme(
            Arc::new(ReadmeRegistry {
                linked: false,
                linked_reads: Arc::new(Mutex::new(0)),
            }),
            Arc::clone(&repo),
            None,
        );

        svc.handle(req("r1")).await.unwrap();
        wait_for_row(&repo).await.expect("README recorded");
        svc.handle(req("r1")).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(
            repo.rows.lock().unwrap().len(),
            1,
            "the second request hit the metadata cache and must not have re-recorded"
        );
    }

    // ── The single introspection pass (RFC 0007 §5.2) ────────────────────────

    /// An extractor that finds a README and nothing else — the shape the five
    /// README-only registry kinds have.
    struct ReadmeOnlyExtractor;

    impl crate::ports::SbomExtractor for ReadmeOnlyExtractor {
        fn extract(&self, _data: &Bytes, _registry_type: &str) -> crate::ports::ExtractedManifest {
            crate::ports::ExtractedManifest {
                readme: Some(crate::ports::ExtractedReadme {
                    content: "# from the archive".to_owned(),
                    format: ReadmeFormat::Markdown,
                    path: "package/README.md".to_owned(),
                    truncated: false,
                }),
                ..Default::default()
            }
        }
    }

    struct NoopSbomRepo;

    #[async_trait]
    impl crate::ports::SbomRepository for NoopSbomRepo {
        async fn upsert_sbom(&self, _: crate::entities::ArtifactSbom) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get_sbom(
            &self,
            _: &str,
            _: &crate::entities::SbomFormat,
        ) -> Result<Option<crate::entities::ArtifactSbom>, CoreError> {
            Ok(None)
        }
        async fn get_sbom_by_coordinates(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &crate::entities::SbomFormat,
        ) -> Result<Option<crate::entities::ArtifactSbom>, CoreError> {
            Ok(None)
        }
        async fn get_license_for_coordinate(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<String>, CoreError> {
            Ok(None)
        }
        async fn list_sboms_for_export(
            &self,
            _: Option<&str>,
            _: Option<chrono::DateTime<Utc>>,
            _: Option<chrono::DateTime<Utc>>,
            _: u64,
            _: u64,
        ) -> Result<Vec<crate::entities::ArtifactSbom>, CoreError> {
            Ok(vec![])
        }
    }

    /// The early return changed from *SBOM is off* to *SBOM is off **and**
    /// README-from-archive is off*. With SBOM disabled and README capture on,
    /// the artifact is still read — and this is the assertion that fails if the
    /// old early return comes back.
    #[tokio::test]
    async fn the_archive_is_read_for_the_readme_even_with_sbom_off() {
        let repo = Arc::new(RecordingRepo::default());
        let mut svc = proxy_with_readme(
            // A plain registry: nothing in its metadata carries a README, so
            // the archive is the only source.
            Arc::new(FixedRegistry),
            Arc::clone(&repo),
            None,
        );
        svc.sbom = Some(Arc::new(crate::services::SbomService::new(
            Arc::new(NoopSbomRepo),
            Some(Arc::new(ReadmeOnlyExtractor)),
            None,
        )));
        // `hot.sbom` has no entry for this registry, so SBOM generation is off.

        svc.handle(req("r1")).await.unwrap();

        let row = wait_for_row(&repo).await.expect("README recorded");
        assert_eq!(row.content, "# from the archive");
        assert_eq!(row.source, ReadmeSource::Archive);
    }

    /// `from_archive = false` is the operator saying "do not open the artifact
    /// for this", and it has to mean that even when the bytes are already being
    /// read for something else.
    #[tokio::test]
    async fn from_archive_false_stores_no_archive_readme() {
        let repo = Arc::new(RecordingRepo::default());
        let mut svc = proxy_with_readme(
            Arc::new(FixedRegistry),
            Arc::clone(&repo),
            Some(ReadmeConfig {
                from_archive: false,
                ..ReadmeConfig::default()
            }),
        );
        svc.sbom = Some(Arc::new(crate::services::SbomService::new(
            Arc::new(NoopSbomRepo),
            Some(Arc::new(ReadmeOnlyExtractor)),
            None,
        )));

        svc.handle(req("r1")).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(repo.rows.lock().unwrap().is_empty());
    }
}

// ── The discovery read (RFC 0007 §5.5) ────────────────────────────────────────

mod discovery {
    use super::*;
    use crate::entities::RegistryKind;
    use crate::ports::{DocumentKind, VersionDocument};
    use crate::services::hot_config::UpstreamDetailConfig;
    use crate::services::proxy::Freshness;
    use std::sync::Mutex;

    /// A registry whose listing document is whatever the test says, counting
    /// how many times it was actually asked.
    struct CountingRegistry {
        kind: &'static str,
        document: Mutex<Result<VersionDocument, CoreError>>,
        fetches: Arc<Mutex<usize>>,
        /// Milliseconds to hold the fetch open, so a second caller reaches the
        /// single-flight wait rather than racing past it.
        delay_ms: u64,
    }

    impl CountingRegistry {
        fn npm(document: VersionDocument, fetches: Arc<Mutex<usize>>) -> Arc<Self> {
            Arc::new(Self {
                kind: "npm",
                document: Mutex::new(Ok(document)),
                fetches,
                delay_ms: 0,
            })
        }
    }

    #[async_trait]
    impl RegistryClient for CountingRegistry {
        fn registry_type(&self) -> &str {
            self.kind
        }

        async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
            Ok(PackageMetadata {
                id: pkg.clone(),
                published_at: None,
                download_url: None,
                checksum: None,
                is_signed: None,
                extra: serde_json::json!({}),
                cache_control: None,
            })
        }

        async fn fetch_version_document(
            &self,
            _package: &str,
            _kind: DocumentKind,
        ) -> Result<VersionDocument, CoreError> {
            *self.fetches.lock().unwrap() += 1;
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            match &*self.document.lock().unwrap() {
                Ok(doc) => Ok(doc.clone()),
                Err(CoreError::NotFound(m)) => Err(CoreError::NotFound(m.clone())),
                Err(e) => Err(CoreError::Registry(e.to_string())),
            }
        }

        async fn list_versions(&self, _package: &str) -> Result<Vec<String>, CoreError> {
            *self.fetches.lock().unwrap() += 1;
            Ok(vec!["1.0.0".to_owned(), "2.0.0".to_owned()])
        }

        async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
            Err(CoreError::Registry("not used".into()))
        }
    }

    fn packument() -> VersionDocument {
        VersionDocument::json(serde_json::json!({
            "dist-tags": { "latest": "2.0.0" },
            "readme": "# express",
            "versions": { "1.0.0": {}, "2.0.0": {} }
        }))
    }

    fn svc_with(
        client: Arc<dyn RegistryClient>,
        cfg: Option<UpstreamDetailConfig>,
    ) -> ProxyService {
        let mut hot = HotConfig {
            registries: HashMap::from([("r1".to_owned(), client)]),
            policies: HashMap::from([(
                "r1".to_owned(),
                Arc::new(RegistryPolicy {
                    metadata_ttl: None,
                    firewall_only: false,
                    serve_stale_metadata: false,
                    artifact_ttl: None,
                    rules: vec![],
                }),
            )]),
            ..Default::default()
        };
        if let Some(cfg) = cfg {
            hot.upstream_detail.insert("r1".to_owned(), cfg);
        }
        ProxyService {
            hot: new_hot_lock(hot),
            storage: MemStorage::new(),
            cache: TestCacheStore::new(),
            repo: SpyRepo::new(),
            artifact_meta: NoopArtifactMeta::arc(),
            metrics: Arc::new(ProxyMetrics::new(&["r1".to_owned()])),
            sbom: None,
            discovery: Default::default(),
            readme: None,
        }
    }

    /// A package this instance holds nothing of comes back with its versions
    /// and its README — the test that would have failed before this RFC, and
    /// the whole point of §2.3.
    #[tokio::test]
    async fn a_package_with_no_local_rows_comes_back_from_upstream() {
        let fetches = Arc::new(Mutex::new(0));
        let svc = svc_with(
            CountingRegistry::npm(packument(), Arc::clone(&fetches)),
            None,
        );

        let outcome = svc
            .upstream_detail("r1", "express", &Identity::anonymous())
            .await
            .unwrap()
            .expect("attempted");

        let mut versions: Vec<&str> = outcome
            .detail
            .versions
            .iter()
            .map(|v| v.version.as_str())
            .collect();
        versions.sort_unstable();
        assert_eq!(versions, ["1.0.0", "2.0.0"]);
        assert_eq!(outcome.freshness, Freshness::Fresh);
        assert!(!outcome.truncated);
        // The packument carries the README, so one fetch answered both halves.
        assert_eq!(
            outcome.detail.readmes["2.0.0"].content.as_deref(),
            Some("# express")
        );
        assert_eq!(*fetches.lock().unwrap(), 1);
    }

    /// A second read within the TTL makes no upstream call: the document is in
    /// the metadata cache, which is rung 1.
    #[tokio::test]
    async fn a_second_read_within_the_ttl_makes_no_upstream_call() {
        let fetches = Arc::new(Mutex::new(0));
        let svc = svc_with(
            CountingRegistry::npm(packument(), Arc::clone(&fetches)),
            None,
        );

        let first = svc
            .upstream_detail("r1", "express", &Identity::anonymous())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.freshness, Freshness::Fresh);

        let second = svc
            .upstream_detail("r1", "express", &Identity::anonymous())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.freshness, Freshness::Cached);
        assert_eq!(*fetches.lock().unwrap(), 1, "the cache was not consulted");
    }

    /// Ten operators opening the same new package must produce one upstream
    /// request. Without this the console amplifies requests under exactly the
    /// conditions that make a package interesting.
    #[tokio::test]
    async fn concurrent_readers_produce_exactly_one_upstream_request() {
        let fetches = Arc::new(Mutex::new(0));
        let client = Arc::new(CountingRegistry {
            kind: "npm",
            document: Mutex::new(Ok(packument())),
            fetches: Arc::clone(&fetches),
            delay_ms: 60,
        });
        let svc = Arc::new(svc_with(client, None));

        let readers: Vec<_> = (0..10)
            .map(|_| {
                let svc = Arc::clone(&svc);
                tokio::spawn(async move {
                    svc.upstream_detail("r1", "express", &Identity::anonymous())
                        .await
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        for reader in readers {
            assert!(reader.await.unwrap(), "every reader got an answer");
        }
        assert_eq!(*fetches.lock().unwrap(), 1);
    }

    /// A `404` is a fact — upstream *answered* — so it is remembered, and a
    /// reload loop or a crawler cannot turn every page view into a request.
    #[tokio::test]
    async fn an_upstream_404_is_remembered_for_the_negative_ttl() {
        let fetches = Arc::new(Mutex::new(0));
        let client = Arc::new(CountingRegistry {
            kind: "npm",
            document: Mutex::new(Err(CoreError::NotFound("no such package".into()))),
            fetches: Arc::clone(&fetches),
            delay_ms: 0,
        });
        let svc = svc_with(client, None);

        for _ in 0..3 {
            assert!(svc
                .upstream_detail("r1", "nope", &Identity::anonymous())
                .await
                .unwrap()
                .is_none());
        }
        assert_eq!(
            *fetches.lock().unwrap(),
            1,
            "the absence was not remembered"
        );
    }

    /// A connection failure is not a fact about the package, so it is not
    /// remembered — the next reader tries again, and the page meanwhile says
    /// the upstream could not be reached rather than that the package is gone.
    #[tokio::test]
    async fn a_connection_failure_is_not_cached_as_an_absence() {
        let fetches = Arc::new(Mutex::new(0));
        let client = Arc::new(CountingRegistry {
            kind: "npm",
            document: Mutex::new(Err(CoreError::Registry("connection refused".into()))),
            fetches: Arc::clone(&fetches),
            delay_ms: 0,
        });
        let svc = svc_with(client, None);

        for _ in 0..3 {
            assert!(svc
                .upstream_detail("r1", "express", &Identity::anonymous())
                .await
                .is_err());
        }
        assert_eq!(
            *fetches.lock().unwrap(),
            3,
            "a failure was cached as absence"
        );
    }

    /// `enabled = false` makes no upstream call at all — the switch an
    /// air-gapped estate sets, and the one an operator whose threat model is
    /// "this box talks upstream only when a build needs bytes" reaches for.
    #[tokio::test]
    async fn a_disabled_registry_is_never_asked() {
        let fetches = Arc::new(Mutex::new(0));
        let svc = svc_with(
            CountingRegistry::npm(packument(), Arc::clone(&fetches)),
            Some(UpstreamDetailConfig {
                enabled: false,
                ..UpstreamDetailConfig::default()
            }),
        );

        assert!(svc
            .upstream_detail("r1", "express", &Identity::anonymous())
            .await
            .unwrap()
            .is_none());
        assert_eq!(*fetches.lock().unwrap(), 0);
    }

    /// `max_versions` truncates newest-first and says so. A truncated list that
    /// dropped the *newest* versions would be worse than no list at all.
    #[tokio::test]
    async fn max_versions_truncates_newest_first_and_reports_it() {
        let doc = VersionDocument::json(serde_json::json!({
            "dist-tags": { "latest": "3.0.0" },
            "versions": { "1.0.0": {}, "2.0.0": {}, "3.0.0": {} }
        }));
        let svc = svc_with(
            CountingRegistry::npm(doc, Arc::new(Mutex::new(0))),
            Some(UpstreamDetailConfig {
                max_versions: 2,
                ..UpstreamDetailConfig::default()
            }),
        );

        let outcome = svc
            .upstream_detail("r1", "express", &Identity::anonymous())
            .await
            .unwrap()
            .unwrap();
        assert!(outcome.truncated);
        let versions: Vec<&str> = outcome
            .detail
            .versions
            .iter()
            .map(|v| v.version.as_str())
            .collect();
        assert_eq!(versions, ["3.0.0", "2.0.0"]);
    }

    /// A kind with no listing document but a version list still answers — with
    /// rows carrying no publish times, which is honest and still the difference
    /// between a versions table and an empty state.
    #[tokio::test]
    async fn a_list_versions_kind_answers_with_bare_rows() {
        let fetches = Arc::new(Mutex::new(0));
        let client = Arc::new(CountingRegistry {
            kind: "openvsx",
            document: Mutex::new(Err(CoreError::NotSupported("no document".into()))),
            fetches: Arc::clone(&fetches),
            delay_ms: 0,
        });
        let svc = svc_with(client, None);

        let outcome = svc
            .upstream_detail("r1", "pub.ext", &Identity::anonymous())
            .await
            .unwrap()
            .expect("attempted");
        assert_eq!(outcome.detail.versions.len(), 2);
        assert!(outcome
            .detail
            .versions
            .iter()
            .all(|v| v.published_at.is_none()));
        assert_eq!(*fetches.lock().unwrap(), 1);
    }

    /// A kind with nothing to ask is not asked. `generic` is path-addressed:
    /// there is no package identity to ask about.
    #[tokio::test]
    async fn a_kind_with_no_upstream_detail_is_not_asked() {
        assert!(matches!(
            RegistryKind::Generic.upstream_detail(),
            crate::entities::UpstreamDetailSupport::None(_)
        ));

        let fetches = Arc::new(Mutex::new(0));
        let client = Arc::new(CountingRegistry {
            kind: "generic",
            document: Mutex::new(Ok(packument())),
            fetches: Arc::clone(&fetches),
            delay_ms: 0,
        });
        let svc = svc_with(client, None);

        assert!(svc
            .upstream_detail("r1", "anything", &Identity::anonymous())
            .await
            .unwrap()
            .is_none());
        assert_eq!(*fetches.lock().unwrap(), 0);
    }

    /// The read writes nothing to the catalogue: no access event, and nothing
    /// that would make the package appear in `/api/v1/explore/packages`. A page
    /// view must not be able to change what the catalogue claims this instance
    /// has (§4.4).
    #[tokio::test]
    async fn a_discovery_read_records_no_access_event() {
        let repo = SpyRepo::new();
        let mut svc = svc_with(
            CountingRegistry::npm(packument(), Arc::new(Mutex::new(0))),
            None,
        );
        svc.repo = Arc::clone(&repo) as Arc<dyn PackageRepository>;

        svc.upstream_detail("r1", "express", &Identity::anonymous())
            .await
            .unwrap()
            .unwrap();

        assert!(
            repo.events().is_empty(),
            "the discovery read recorded an access event: {:?}",
            repo.events()
        );
    }
}

// ── The per-version README read (RFC 0007 §5.6, open question 7) ─────────────

mod version_readme {
    use super::*;
    use crate::entities::{MetadataReadme, ReadmeFormat};

    /// The invariant that makes the derived read safe: a page view resolves a
    /// version's metadata and **writes no `package_readmes` row**.
    ///
    /// A row created because somebody looked at a page has nothing that ever
    /// deletes it — deletion keys on a version being deleted, and a version
    /// never held here is never deleted. Asserted at the unit boundary as well
    /// as through the handler, because the recording hook and the resolve sit
    /// one line apart and a future edit could reunite them without any HTTP
    /// test noticing.
    #[tokio::test]
    async fn a_per_version_readme_read_records_nothing() {
        use crate::services::readme::ReadmeService;
        use std::sync::Mutex;

        struct PypiLike {
            resolves: Arc<Mutex<usize>>,
        }

        #[async_trait]
        impl RegistryClient for PypiLike {
            fn registry_type(&self) -> &str {
                "pypi"
            }
            async fn resolve_metadata(
                &self,
                pkg: &PackageId,
            ) -> Result<PackageMetadata, CoreError> {
                *self.resolves.lock().unwrap() += 1;
                Ok(PackageMetadata {
                    id: pkg.clone(),
                    published_at: None,
                    download_url: None,
                    checksum: None,
                    is_signed: None,
                    extra: serde_json::json!({
                        "readme": MetadataReadme::text("# requests", ReadmeFormat::Markdown),
                    }),
                    cache_control: None,
                })
            }
            async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
                Err(CoreError::Registry("not used".into()))
            }
        }

        let repo = Arc::new(super::readme_capture::RecordingRepo::default());
        let resolves = Arc::new(Mutex::new(0));
        let svc = ProxyService {
            hot: new_hot_lock(HotConfig {
                registries: HashMap::from([(
                    "r1".to_owned(),
                    Arc::new(PypiLike {
                        resolves: Arc::clone(&resolves),
                    }) as Arc<dyn RegistryClient>,
                )]),
                policies: HashMap::from([(
                    "r1".to_owned(),
                    Arc::new(RegistryPolicy {
                        metadata_ttl: None,
                        firewall_only: false,
                        serve_stale_metadata: false,
                        artifact_ttl: None,
                        rules: vec![],
                    }),
                )]),
                ..Default::default()
            }),
            storage: MemStorage::new(),
            cache: TestCacheStore::new(),
            repo: SpyRepo::new(),
            artifact_meta: NoopArtifactMeta::arc(),
            metrics: Arc::new(ProxyMetrics::new(&["r1".to_owned()])),
            sbom: None,
            discovery: Default::default(),
            readme: Some(Arc::new(ReadmeService::new(
                Arc::clone(&repo) as Arc<dyn crate::ports::ReadmeRepository>
            ))),
        };

        let answer = svc
            .upstream_version_readme("r1", "requests", "2.31.0", &Identity::anonymous())
            .await
            .unwrap()
            .expect("a per-version README");
        assert_eq!(answer.0, "# requests");
        assert_eq!(answer.3, crate::services::proxy::Freshness::Fresh);

        // The capture path spawns, so give it every chance to have run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            repo.rows.lock().unwrap().is_empty(),
            "a page view stored a README row: {:?}",
            repo.rows.lock().unwrap()
        );

        // Cache-first: a second reader of the same version costs no request,
        // and reports rung 1.
        let again = svc
            .upstream_version_readme("r1", "requests", "2.31.0", &Identity::anonymous())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.3, crate::services::proxy::Freshness::Cached);
        assert_eq!(*resolves.lock().unwrap(), 1);
    }

    /// An archive-borne kind is never asked: its text is inside bytes we do not
    /// hold, so resolving a version would be a request for nothing.
    #[tokio::test]
    async fn an_archive_borne_kind_is_not_resolved_for_a_readme() {
        let repo = Arc::new(super::readme_capture::RecordingRepo::default());
        let svc = proxy_for_kind("cargo", Arc::clone(&repo));
        assert!(svc
            .upstream_version_readme("r1", "mylib", "1.0.0", &Identity::anonymous())
            .await
            .unwrap()
            .is_none());
    }

    fn proxy_for_kind(
        kind: &'static str,
        repo: Arc<super::readme_capture::RecordingRepo>,
    ) -> ProxyService {
        use crate::services::readme::ReadmeService;

        struct Bare(&'static str);

        #[async_trait]
        impl RegistryClient for Bare {
            fn registry_type(&self) -> &str {
                self.0
            }
            async fn resolve_metadata(
                &self,
                _pkg: &PackageId,
            ) -> Result<PackageMetadata, CoreError> {
                panic!("an archive-borne kind must not be resolved for a README")
            }
            async fn fetch_artifact(&self, _pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
                Err(CoreError::Registry("not used".into()))
            }
        }

        ProxyService {
            hot: new_hot_lock(HotConfig {
                registries: HashMap::from([(
                    "r1".to_owned(),
                    Arc::new(Bare(kind)) as Arc<dyn RegistryClient>,
                )]),
                policies: HashMap::from([(
                    "r1".to_owned(),
                    Arc::new(RegistryPolicy {
                        metadata_ttl: None,
                        firewall_only: false,
                        serve_stale_metadata: false,
                        artifact_ttl: None,
                        rules: vec![],
                    }),
                )]),
                ..Default::default()
            }),
            storage: MemStorage::new(),
            cache: TestCacheStore::new(),
            repo: SpyRepo::new(),
            artifact_meta: NoopArtifactMeta::arc(),
            metrics: Arc::new(ProxyMetrics::new(&["r1".to_owned()])),
            sbom: None,
            discovery: Default::default(),
            readme: Some(Arc::new(ReadmeService::new(
                repo as Arc<dyn crate::ports::ReadmeRepository>,
            ))),
        }
    }
}
