use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};

use super::*;
use crate::entities::Role;
use crate::error::CoreError;
use crate::ports::ByteStream;

/// The operator every run in this file is attributed to.
fn admin() -> Identity {
    Identity {
        user_id: Some("admin-1".to_owned()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}
use crate::ports::{
    ArtifactCacheMeta, ArtifactInventory, ArtifactMeta, ArtifactMetaRecord, StorageBackend,
    StorageMeta, StoredArtifact,
};

// ── In-memory ArtifactMetaRepository ─────────────────────────────────────

#[derive(Default)]
struct InMemArtifactMeta {
    rows: Mutex<Vec<ArtifactMeta>>,
}

impl InMemArtifactMeta {
    fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn seed(&self, meta: ArtifactMeta) {
        self.rows.lock().unwrap().push(meta);
    }

    fn all(&self) -> Vec<ArtifactMeta> {
        self.rows.lock().unwrap().clone()
    }
}

#[async_trait]
impl ArtifactCacheMeta for InMemArtifactMeta {
    async fn record_artifact(&self, rec: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        let now = Utc::now();
        let mut rows = self.rows.lock().unwrap();
        if let Some(r) = rows.iter_mut().find(|r| r.artifact_key == rec.key) {
            r.size_bytes = rec.size;
            r.cached_at = now;
            r.last_accessed_at = now;
        } else {
            rows.push(ArtifactMeta {
                artifact_key: rec.key.to_owned(),
                registry: rec.registry.to_owned(),
                package_name: rec.package_name.to_owned(),
                version: rec.version.to_owned(),
                size_bytes: rec.size,
                cached_at: now,
                last_accessed_at: now,
            });
        }
        Ok(())
    }

    async fn get_artifact_checksum(&self, _key: &str) -> Result<Option<String>, CoreError> {
        Ok(None)
    }

    async fn touch_artifact(&self, key: &str) -> Result<(), CoreError> {
        let now = Utc::now();
        let mut rows = self.rows.lock().unwrap();
        if let Some(r) = rows.iter_mut().find(|r| r.artifact_key == key) {
            r.last_accessed_at = now;
        }
        Ok(())
    }

    async fn is_artifact_expired(
        &self,
        key: &str,
        older_than: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        let rows = self.rows.lock().unwrap();
        // No row → treat as expired (matches PgArtifactMetaRepository semantics).
        let fresh = rows
            .iter()
            .any(|r| r.artifact_key == key && r.cached_at >= older_than);
        Ok(!fresh)
    }

    async fn delete_artifact_meta(&self, key: &str) -> Result<(), CoreError> {
        self.rows.lock().unwrap().retain(|r| r.artifact_key != key);
        Ok(())
    }
}

#[async_trait]
impl ArtifactInventory for InMemArtifactMeta {
    async fn list_artifacts(&self, registry: &str) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|r| registry.is_empty() || r.registry == registry)
            .cloned()
            .collect())
    }

    async fn list_artifacts_by_package(&self) -> Result<Vec<ArtifactMeta>, CoreError> {
        let mut rows = self.rows.lock().unwrap().clone();
        rows.sort_by(|a, b| {
            a.registry
                .cmp(&b.registry)
                .then(a.package_name.cmp(&b.package_name))
                .then(b.cached_at.cmp(&a.cached_at)) // DESC
        });
        Ok(rows)
    }

    async fn list_expired_by_ttl(
        &self,
        registry: &str,
        older_than: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|r| (registry.is_empty() || r.registry == registry) && r.cached_at < older_than)
            .cloned()
            .collect())
    }

    async fn list_idle(
        &self,
        registry: &str,
        idle_since: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|r| {
                (registry.is_empty() || r.registry == registry) && r.last_accessed_at < idle_since
            })
            .cloned()
            .collect())
    }

    async fn total_size_bytes(&self, registry: &str) -> Result<u64, CoreError> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|r| registry.is_empty() || r.registry == registry)
            .map(|r| r.size_bytes.unwrap_or(0))
            .sum())
    }

    async fn list_lru(&self, registry: &str, limit: i64) -> Result<Vec<ArtifactMeta>, CoreError> {
        let mut rows = self.rows.lock().unwrap().clone();
        rows.retain(|r| registry.is_empty() || r.registry == registry);
        rows.sort_by_key(|r| r.last_accessed_at);
        rows.truncate(limit as usize);
        Ok(rows)
    }
}

// ── In-memory StorageBackend ──────────────────────────────────────────────

#[derive(Default)]
struct InMemStorage {
    data: Mutex<HashMap<String, Bytes>>,
}

impl InMemStorage {
    fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn seed(&self, key: &str, data: &[u8]) {
        self.data
            .lock()
            .unwrap()
            .insert(key.to_owned(), Bytes::copy_from_slice(data));
    }

    fn keys(&self) -> Vec<String> {
        self.data.lock().unwrap().keys().cloned().collect()
    }

    fn contains(&self, key: &str) -> bool {
        self.data.lock().unwrap().contains_key(key)
    }
}

#[async_trait]
impl StorageBackend for InMemStorage {
    async fn store(&self, key: &str, data: Bytes, _: StorageMeta) -> Result<(), CoreError> {
        self.data.lock().unwrap().insert(key.to_owned(), data);
        Ok(())
    }
    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
        Ok(self.data.lock().unwrap().get(key).map(|b| {
            let b = b.clone();
            let stream: ByteStream =
                Box::pin(futures::stream::once(
                    async move { Ok::<Bytes, CoreError>(b) },
                ));
            StoredArtifact {
                stream,
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
        let mut m = self.data.lock().unwrap();
        let keys: Vec<_> = m
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        let n = keys.len();
        for k in keys {
            m.remove(&k);
        }
        Ok(n)
    }
    async fn stat_by_prefix(&self, prefix: &str) -> Result<(u64, u64), CoreError> {
        let m = self.data.lock().unwrap();
        let (c, b) = m
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .fold((0u64, 0u64), |(c, b), (_, v)| (c + 1, b + v.len() as u64));
        Ok((c, b))
    }
    async fn list_keys(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_meta(
    key: &str,
    registry: &str,
    pkg: &str,
    version: &str,
    size: u64,
    cached_ago: Duration,
    accessed_ago: Duration,
) -> ArtifactMeta {
    let now = Utc::now();
    ArtifactMeta {
        artifact_key: key.to_owned(),
        registry: registry.to_owned(),
        package_name: pkg.to_owned(),
        version: version.to_owned(),
        size_bytes: Some(size),
        cached_at: now - cached_ago,
        last_accessed_at: now - accessed_ago,
    }
}

fn svc(
    meta: Arc<InMemArtifactMeta>,
    storage: Arc<InMemStorage>,
    config: EvictionConfig,
) -> EvictionService {
    EvictionService::new(meta, storage, config)
}

// ── TTL tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_ttl_evicts_expired_artifacts() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();

    let old = make_meta(
        "artifact:npm/old:1.0",
        "npm",
        "old",
        "1.0",
        100,
        Duration::hours(2),
        Duration::hours(2),
    );
    let fresh = make_meta(
        "artifact:npm/fresh:1.0",
        "npm",
        "fresh",
        "1.0",
        100,
        Duration::minutes(5),
        Duration::minutes(5),
    );
    meta.seed(old.clone());
    meta.seed(fresh.clone());
    storage.seed(&old.artifact_key, b"old-data");
    storage.seed(&fresh.artifact_key, b"fresh-data");

    let config = EvictionConfig {
        artifact_ttl_secs: Some(3600), // 1 hour TTL → "old" (2h) is expired
        registry: "npm".to_owned(),
        ..Default::default()
    };
    let count = svc(meta.clone(), storage.clone(), config)
        .run_ttl(&mut EvictionReport::live())
        .await
        .unwrap();

    assert_eq!(count, 1);
    assert!(
        !storage.contains(&old.artifact_key),
        "expired artifact must be removed from storage"
    );
    assert!(
        storage.contains(&fresh.artifact_key),
        "fresh artifact must remain"
    );
    assert!(
        !meta
            .all()
            .iter()
            .any(|r| r.artifact_key == old.artifact_key),
        "expired meta must be removed"
    );
}

#[tokio::test]
async fn run_ttl_noop_when_not_configured() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    meta.seed(make_meta(
        "artifact:npm/x:1.0",
        "npm",
        "x",
        "1.0",
        10,
        Duration::days(365),
        Duration::days(365),
    ));
    storage.seed("artifact:npm/x:1.0", b"data");

    let count = svc(
        meta.clone(),
        storage.clone(),
        EvictionConfig {
            artifact_ttl_secs: None,
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_ttl(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 0);
    assert_eq!(storage.keys().len(), 1, "nothing should be deleted");
}

// ── Idle tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_idle_evicts_unaccessed_artifacts() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();

    let idle = make_meta(
        "artifact:npm/idle:1.0",
        "npm",
        "idle",
        "1.0",
        50,
        Duration::days(1),
        Duration::days(10),
    );
    let active = make_meta(
        "artifact:npm/active:1.0",
        "npm",
        "active",
        "1.0",
        50,
        Duration::days(1),
        Duration::hours(1),
    );
    meta.seed(idle.clone());
    meta.seed(active.clone());
    storage.seed(&idle.artifact_key, b"data");
    storage.seed(&active.artifact_key, b"data");

    let count = svc(
        meta.clone(),
        storage.clone(),
        EvictionConfig {
            idle_days: Some(7),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_idle(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 1);
    assert!(!storage.contains(&idle.artifact_key));
    assert!(storage.contains(&active.artifact_key));
}

#[tokio::test]
async fn run_idle_noop_when_not_configured() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    meta.seed(make_meta(
        "k",
        "npm",
        "p",
        "1.0",
        10,
        Duration::days(1),
        Duration::days(365),
    ));
    storage.seed("k", b"d");

    let count = svc(
        meta,
        storage.clone(),
        EvictionConfig {
            idle_days: None,
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_idle(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 0);
}

// ── Keep-latest-N tests ───────────────────────────────────────────────────

#[tokio::test]
async fn run_keep_latest_n_removes_oldest_versions() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    let now = Utc::now();

    // 3 versions of "serde", cached at t-3, t-2, t-1 (newest = t-1)
    for (ver, ago) in [("1.0", 3i64), ("2.0", 2), ("3.0", 1)] {
        let m = ArtifactMeta {
            artifact_key: format!("artifact:cargo/serde:{ver}"),
            registry: "cargo".to_owned(),
            package_name: "serde".to_owned(),
            version: ver.to_owned(),
            size_bytes: Some(50),
            cached_at: now - Duration::hours(ago),
            last_accessed_at: now - Duration::hours(ago),
        };
        meta.seed(m.clone());
        storage.seed(&m.artifact_key, b"data");
    }

    let count = svc(
        meta.clone(),
        storage.clone(),
        EvictionConfig {
            keep_latest_n: Some(2),
            registry: "cargo".to_owned(),
            ..Default::default()
        },
    )
    .run_keep_latest_n(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 1);
    assert!(
        !storage.contains("artifact:cargo/serde:1.0"),
        "oldest (v1.0) must be evicted"
    );
    assert!(
        storage.contains("artifact:cargo/serde:2.0"),
        "v2.0 must remain"
    );
    assert!(
        storage.contains("artifact:cargo/serde:3.0"),
        "v3.0 must remain"
    );
}

#[tokio::test]
async fn run_keep_latest_n_respects_package_boundaries() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    let now = Utc::now();

    for pkg in ["serde", "tokio"] {
        for (ver, ago) in [("1.0", 3i64), ("2.0", 2), ("3.0", 1)] {
            let m = ArtifactMeta {
                artifact_key: format!("artifact:cargo/{pkg}:{ver}"),
                registry: "cargo".to_owned(),
                package_name: pkg.to_owned(),
                version: ver.to_owned(),
                size_bytes: Some(50),
                cached_at: now - Duration::hours(ago),
                last_accessed_at: now - Duration::hours(ago),
            };
            meta.seed(m.clone());
            storage.seed(&m.artifact_key, b"data");
        }
    }

    let count = svc(
        meta.clone(),
        storage.clone(),
        EvictionConfig {
            keep_latest_n: Some(2),
            registry: "cargo".to_owned(),
            ..Default::default()
        },
    )
    .run_keep_latest_n(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 2, "one eviction per package");
    assert!(!storage.contains("artifact:cargo/serde:1.0"));
    assert!(!storage.contains("artifact:cargo/tokio:1.0"));
}

#[tokio::test]
async fn run_keep_latest_n_noop_when_not_configured() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    for ver in ["1.0", "2.0", "3.0"] {
        let m = make_meta(
            &format!("k:{ver}"),
            "npm",
            "pkg",
            ver,
            10,
            Duration::hours(1),
            Duration::hours(1),
        );
        meta.seed(m.clone());
        storage.seed(&m.artifact_key, b"d");
    }

    let count = svc(
        meta,
        storage.clone(),
        EvictionConfig {
            keep_latest_n: None,
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_keep_latest_n(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 0);
    assert_eq!(storage.keys().len(), 3);
}

// ── LRU size cap tests ────────────────────────────────────────────────────

#[tokio::test]
async fn run_lru_size_cap_evicts_until_under_cap() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    let now = Utc::now();

    // 3 artifacts: sizes 100+100+100 = 300 bytes, cap = 150
    // LRU order: "a" (oldest), "b", "c" (newest)
    let artifacts = [
        ("artifact:npm/a:1.0", "a", 100i64, 3i64),
        ("artifact:npm/b:1.0", "b", 100, 2),
        ("artifact:npm/c:1.0", "c", 100, 1),
    ];
    for (key, pkg, size, accessed_ago) in artifacts {
        let m = ArtifactMeta {
            artifact_key: key.to_owned(),
            registry: "npm".to_owned(),
            package_name: pkg.to_owned(),
            version: "1.0".to_owned(),
            size_bytes: Some(size as u64),
            cached_at: now - Duration::hours(accessed_ago),
            last_accessed_at: now - Duration::hours(accessed_ago),
        };
        meta.seed(m);
        storage.seed(key, b"x".repeat(size as usize).as_slice());
    }

    let count = svc(
        meta.clone(),
        storage.clone(),
        EvictionConfig {
            max_size_bytes: Some(150),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_lru_size_cap(&mut EvictionReport::live())
    .await
    .unwrap();

    assert!(count >= 1, "at least one artifact must be evicted");
    assert!(
        meta.all()
            .iter()
            .map(|r| r.size_bytes.unwrap_or(0))
            .sum::<u64>()
            <= 150,
        "total remaining size must be within cap"
    );
    // "a" (oldest LRU) must have been evicted first
    assert!(
        !storage.contains("artifact:npm/a:1.0"),
        "LRU artifact must be evicted first"
    );
}

#[tokio::test]
async fn run_lru_size_cap_evicts_across_multiple_batches() {
    // `list_lru` truncates each call to 256 candidates. Seed more than one
    // batch's worth (300, size 1 each) and require evicting down to a cap that
    // a single 256-item batch cannot reach on its own, to confirm the loop
    // keeps fetching further batches instead of giving up after the first.
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();

    for i in 0..300u64 {
        let key = format!("artifact:npm/p{i}:1.0");
        meta.seed(make_meta(
            &key,
            "npm",
            &format!("p{i}"),
            "1.0",
            1,
            Duration::seconds(300 - i as i64),
            Duration::seconds(300 - i as i64),
        ));
        storage.seed(&key, b"x");
    }

    let count = svc(
        meta.clone(),
        storage.clone(),
        EvictionConfig {
            max_size_bytes: Some(10),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_lru_size_cap(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 290, "must evict beyond the first 256-item batch");
    assert_eq!(
        meta.all()
            .iter()
            .map(|r| r.size_bytes.unwrap_or(0))
            .sum::<u64>(),
        10,
        "total remaining size must converge to the cap, not stop mid-way"
    );
}

#[tokio::test]
async fn run_lru_size_cap_noop_when_under_cap() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    meta.seed(make_meta(
        "k",
        "npm",
        "p",
        "1.0",
        50,
        Duration::hours(1),
        Duration::hours(1),
    ));
    storage.seed("k", b"data");

    let count = svc(
        meta,
        storage.clone(),
        EvictionConfig {
            max_size_bytes: Some(1_000_000),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_lru_size_cap(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 0);
    assert_eq!(storage.keys().len(), 1);
}

#[tokio::test]
async fn run_lru_size_cap_noop_when_not_configured() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    meta.seed(make_meta(
        "k",
        "npm",
        "p",
        "1.0",
        999_999,
        Duration::hours(1),
        Duration::hours(1),
    ));
    storage.seed("k", b"data");

    let count = svc(
        meta,
        storage.clone(),
        EvictionConfig {
            max_size_bytes: None,
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_lru_size_cap(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 0);
}

// ── run_all() tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn run_all_applies_all_enabled_strategies() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();

    // Artifact qualifies for both TTL and idle eviction
    let old = make_meta(
        "artifact:npm/old:1.0",
        "npm",
        "old",
        "1.0",
        100,
        Duration::hours(25),
        Duration::days(10),
    );
    meta.seed(old.clone());
    storage.seed(&old.artifact_key, b"data");

    let config = EvictionConfig {
        artifact_ttl_secs: Some(3600 * 24), // 24h TTL, artifact is 25h old
        idle_days: Some(7),
        keep_latest_n: Some(5), // high enough not to evict anything extra
        max_size_bytes: Some(10_000_000), // high enough not to evict anything extra
        registry: "npm".to_owned(),
    };
    let report = svc(meta, storage.clone(), config)
        .run_all(false, &admin())
        .await
        .unwrap();

    assert!(report.total >= 1);
    assert!(!storage.contains(&old.artifact_key));
}

#[tokio::test]
async fn run_all_noop_when_all_strategies_disabled() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    meta.seed(make_meta(
        "k",
        "npm",
        "p",
        "1.0",
        10,
        Duration::days(365),
        Duration::days(365),
    ));
    storage.seed("k", b"data");

    let report = svc(
        meta,
        storage.clone(),
        EvictionConfig {
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_all(false, &admin())
    .await
    .unwrap();

    assert_eq!(report.total, 0);
    assert_eq!(storage.keys().len(), 1, "nothing should be deleted");
}

// ── Coherence check tests ─────────────────────────────────────────────────

#[tokio::test]
async fn coherence_check_deletes_orphaned_storage_objects() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();

    // 3 storage objects, only 2 in meta → 1 orphan
    storage.seed("artifact:npm/a:1.0", b"data");
    storage.seed("artifact:npm/b:1.0", b"data");
    storage.seed("artifact:npm/orphan:1.0", b"orphan");

    meta.seed(make_meta(
        "artifact:npm/a:1.0",
        "npm",
        "a",
        "1.0",
        10,
        Duration::hours(1),
        Duration::hours(1),
    ));
    meta.seed(make_meta(
        "artifact:npm/b:1.0",
        "npm",
        "b",
        "1.0",
        10,
        Duration::hours(1),
        Duration::hours(1),
    ));

    let service = svc(
        meta,
        storage.clone(),
        EvictionConfig {
            registry: "npm".to_owned(),
            ..Default::default()
        },
    );

    // First pass defers deletion (two-pass grace closes the write/delete race):
    // the orphan is only recorded as pending, not yet deleted.
    let report1 = service.run_coherence_check(false, &admin()).await.unwrap();
    assert_eq!(
        report1.orphaned_deleted, 0,
        "first pass must defer deletion"
    );
    assert!(
        storage.contains("artifact:npm/orphan:1.0"),
        "orphan must survive the first pass"
    );

    // Second pass: still orphaned → deleted.
    let report = service.run_coherence_check(false, &admin()).await.unwrap();
    assert_eq!(report.orphaned_deleted, 1);
    assert_eq!(report.storage_keys, 3);
    assert_eq!(report.meta_rows, 2);
    assert!(
        !storage.contains("artifact:npm/orphan:1.0"),
        "orphan must be deleted from storage on the second pass"
    );
    assert!(
        storage.contains("artifact:npm/a:1.0"),
        "tracked artifact must remain"
    );
    assert!(
        storage.contains("artifact:npm/b:1.0"),
        "tracked artifact must remain"
    );
}

#[tokio::test]
async fn coherence_check_clean_when_no_orphans() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();

    storage.seed("artifact:npm/a:1.0", b"data");
    meta.seed(make_meta(
        "artifact:npm/a:1.0",
        "npm",
        "a",
        "1.0",
        10,
        Duration::hours(1),
        Duration::hours(1),
    ));

    let report = svc(
        meta,
        storage,
        EvictionConfig {
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_coherence_check(false, &admin())
    .await
    .unwrap();

    assert_eq!(report.orphaned_deleted, 0);
}

#[tokio::test]
async fn coherence_check_empty_registry_spans_all_namespaces() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();

    storage.seed("artifact:npm/x:1.0", b"data");
    storage.seed("artifact:cargo/y:1.0", b"data");
    // only npm is tracked in meta — cargo artifact is orphaned
    meta.seed(make_meta(
        "artifact:npm/x:1.0",
        "npm",
        "x",
        "1.0",
        10,
        Duration::hours(1),
        Duration::hours(1),
    ));

    let service = svc(
        meta,
        storage.clone(),
        EvictionConfig {
            registry: "".to_owned(), // empty = all namespaces
            ..Default::default()
        },
    );

    // Two-pass grace: first run defers, second run deletes the still-orphaned blob.
    let report1 = service.run_coherence_check(false, &admin()).await.unwrap();
    assert_eq!(report1.orphaned_deleted, 0, "first pass defers deletion");
    let report = service.run_coherence_check(false, &admin()).await.unwrap();

    assert_eq!(
        report.orphaned_deleted, 1,
        "orphan in cargo namespace must be found"
    );
    assert!(!storage.contains("artifact:cargo/y:1.0"));
    assert!(storage.contains("artifact:npm/x:1.0"));
}

// ── Error-path tests ──────────────────────────────────────────────────────

struct FailStorage;

#[async_trait]
impl StorageBackend for FailStorage {
    async fn store(&self, _: &str, _: Bytes, _: StorageMeta) -> Result<(), CoreError> {
        Ok(())
    }
    async fn retrieve(&self, _: &str) -> Result<Option<StoredArtifact>, CoreError> {
        Ok(None)
    }
    async fn exists(&self, _: &str) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn delete(&self, _: &str) -> Result<bool, CoreError> {
        Err(CoreError::Storage("injected delete failure".into()))
    }
    async fn delete_by_prefix(&self, _: &str) -> Result<usize, CoreError> {
        Ok(0)
    }
    async fn stat_by_prefix(&self, _: &str) -> Result<(u64, u64), CoreError> {
        Ok((0, 0))
    }
    async fn list_keys(&self, _: &str) -> Result<Vec<String>, CoreError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn run_ttl_storage_error_skips_artifact_but_continues() {
    let meta = InMemArtifactMeta::arc();
    let storage = Arc::new(FailStorage);

    let old = make_meta(
        "artifact:npm/old:1.0",
        "npm",
        "old",
        "1.0",
        100,
        Duration::hours(2),
        Duration::hours(2),
    );
    meta.seed(old);

    // Storage delete will fail → error path is exercised
    let count = EvictionService::new(
        meta,
        storage,
        EvictionConfig {
            artifact_ttl_secs: Some(3600),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_ttl(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(
        count, 0,
        "failed storage delete means artifact is not counted as evicted"
    );
}

#[tokio::test]
async fn run_idle_storage_error_skips_artifact_but_continues() {
    let meta = InMemArtifactMeta::arc();
    let storage = Arc::new(FailStorage);

    let idle = make_meta(
        "artifact:npm/idle:1.0",
        "npm",
        "idle",
        "1.0",
        50,
        Duration::days(1),
        Duration::days(10),
    );
    meta.seed(idle);

    let count = EvictionService::new(
        meta,
        storage,
        EvictionConfig {
            idle_days: Some(7),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_idle(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 0);
}

#[tokio::test]
async fn run_keep_latest_n_storage_error_skips_artifact() {
    let meta = InMemArtifactMeta::arc();
    let storage = Arc::new(FailStorage);
    let now = Utc::now();

    for (ver, ago) in [("1.0", 3i64), ("2.0", 2), ("3.0", 1)] {
        meta.seed(ArtifactMeta {
            artifact_key: format!("artifact:cargo/serde:{ver}"),
            registry: "cargo".to_owned(),
            package_name: "serde".to_owned(),
            version: ver.to_owned(),
            size_bytes: Some(50),
            cached_at: now - Duration::hours(ago),
            last_accessed_at: now - Duration::hours(ago),
        });
    }

    let count = EvictionService::new(
        meta,
        storage,
        EvictionConfig {
            keep_latest_n: Some(2),
            registry: "cargo".to_owned(),
            ..Default::default()
        },
    )
    .run_keep_latest_n(&mut EvictionReport::live())
    .await
    .unwrap();

    assert_eq!(count, 0);
}

// ── Dry run, and the trail ────────────────────────────────────────────────

/// A `PackageRepository` that only remembers what was recorded through it.
#[derive(Default)]
struct AuditSink {
    events: Mutex<Vec<AccessEvent>>,
}

impl AuditSink {
    fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn of(&self, action: AccessAction) -> Vec<AccessEvent> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.action == action)
            .cloned()
            .collect()
    }

    fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

#[async_trait]
impl crate::ports::PackageRepository for AuditSink {
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
    async fn get_status(&self, _: &PackageId) -> Result<crate::entities::PackageStatus, CoreError> {
        Ok(crate::entities::PackageStatus::Available)
    }
    async fn set_status(
        &self,
        _: &PackageId,
        _: crate::entities::PackageStatus,
    ) -> Result<(), CoreError> {
        Ok(())
    }
    async fn delete_package(&self, _: &PackageId) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn list_packages(
        &self,
        _: crate::entities::PackageFilter,
    ) -> Result<Vec<crate::entities::PackageSummary>, CoreError> {
        Ok(vec![])
    }
    async fn count_packages(&self, _: crate::entities::PackageFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn list_events(
        &self,
        _: crate::entities::EventFilter,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(vec![])
    }
    async fn count_events(&self, _: crate::entities::EventFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
}

/// Four artifacts, all stale enough for the TTL to take them.
fn stale_estate() -> (Arc<InMemArtifactMeta>, Arc<InMemStorage>, Vec<String>) {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    let mut keys = Vec::new();
    for i in 0..4 {
        let m = make_meta(
            &format!("artifact:npm/p{i}:1.0"),
            "npm",
            &format!("p{i}"),
            "1.0",
            100,
            Duration::hours(2),
            Duration::hours(2),
        );
        meta.seed(m.clone());
        storage.seed(&m.artifact_key, b"data");
        keys.push(m.artifact_key);
    }
    keys.sort();
    (meta, storage, keys)
}

fn ttl_config() -> EvictionConfig {
    EvictionConfig {
        artifact_ttl_secs: Some(3600),
        registry: "npm".to_owned(),
        ..Default::default()
    }
}

/// **The point of the preview.** It answers "what would go" with the
/// coordinates, and leaves every one of them exactly where it was.
#[tokio::test]
async fn a_dry_run_reports_the_keys_and_deletes_nothing() {
    let (meta, storage, keys) = stale_estate();
    let report = svc(meta.clone(), storage.clone(), ttl_config())
        .run_all(true, &admin())
        .await
        .unwrap();

    assert!(report.dry_run);
    assert_eq!(report.total, 4);
    assert_eq!(report.evicted_ttl, 4);
    let mut reported = report.evicted_keys.clone();
    reported.sort();
    assert_eq!(reported, keys, "the preview names what it would take");

    for key in &keys {
        assert!(storage.contains(key), "a preview must not delete: {key}");
    }
    assert_eq!(meta.all().len(), 4, "nor drop the meta rows");
}

/// The same estate, live: same count, and now actually gone. The two reports
/// have the same shape on purpose — an operator compares them.
#[tokio::test]
async fn a_live_run_reports_the_same_keys_it_deleted() {
    let (meta, storage, keys) = stale_estate();
    let report = svc(meta.clone(), storage.clone(), ttl_config())
        .run_all(false, &admin())
        .await
        .unwrap();

    assert!(!report.dry_run);
    assert_eq!(report.total, 4);
    let mut reported = report.evicted_keys.clone();
    reported.sort();
    assert_eq!(reported, keys);
    for key in &keys {
        assert!(!storage.contains(key));
    }
    assert!(meta.all().is_empty());
}

/// One event per run, registry-scoped — and **no** per-artifact events, which
/// is the deliberate difference from retention: an LRU sweep evicts by the
/// thousand and the copy it drops comes back on the next request.
#[tokio::test]
async fn a_live_run_records_one_registry_scoped_event() {
    let (meta, storage, _) = stale_estate();
    let sink = AuditSink::arc();
    let report = svc(meta, storage, ttl_config())
        .with_audit(sink.clone())
        .run_all(false, &admin())
        .await
        .unwrap();
    assert_eq!(report.total, 4, "four artifacts went");

    assert_eq!(sink.len(), 1, "four evictions, one event");
    let runs = sink.of(AccessAction::CacheEvictRun);
    assert_eq!(runs.len(), 1);
    let coord = runs[0].package_id.as_ref().unwrap();
    assert_eq!(coord.registry, "npm");
    assert!(
        coord.name.is_empty() && coord.version.is_empty(),
        "a sweep is about a registry, not about a package"
    );
    assert_eq!(runs[0].user_id.as_deref(), Some("admin-1"));
    assert!(sink.of(AccessAction::CacheEvict).is_empty());
}

/// A preview is on the record, and never as a run that could have written.
#[tokio::test]
async fn a_dry_run_records_itself_as_a_dry_run() {
    let (meta, storage, _) = stale_estate();
    let sink = AuditSink::arc();
    svc(meta, storage, ttl_config())
        .with_audit(sink.clone())
        .run_all(true, &admin())
        .await
        .unwrap();

    assert_eq!(sink.of(AccessAction::CacheEvictDryRun).len(), 1);
    assert!(sink.of(AccessAction::CacheEvictRun).is_empty());
}

/// A run that evicts nothing is still a run somebody started.
#[tokio::test]
async fn a_run_that_evicts_nothing_is_still_recorded() {
    let sink = AuditSink::arc();
    let report = svc(InMemArtifactMeta::arc(), InMemStorage::arc(), ttl_config())
        .with_audit(sink.clone())
        .run_all(false, &admin())
        .await
        .unwrap();

    assert_eq!(report.total, 0);
    assert_eq!(sink.of(AccessAction::CacheEvictRun).len(), 1);
}

/// **The size-cap preview must not count an artifact twice.**
///
/// The live loop re-queries `list_lru` after each batch, which is only correct
/// because the batch it evicted is gone. A preview deletes nothing, so the same
/// rows come back — walked the same way, it would report evicting far more
/// artifacts than exist, and an operator sizing a cap would read a number that
/// cannot happen.
#[tokio::test]
async fn a_size_cap_preview_counts_each_artifact_once() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    // 300 artifacts of 1 KiB: more than the live loop's 256-row batch, so the
    // live walk needs two passes and the preview must not imitate it.
    for i in 0..300 {
        let m = make_meta(
            &format!("artifact:npm/p{i}:1.0"),
            "npm",
            &format!("p{i}"),
            "1.0",
            1024,
            Duration::hours(1),
            Duration::minutes(i as i64 + 1),
        );
        meta.seed(m.clone());
        storage.seed(&m.artifact_key, b"data");
    }

    let config = EvictionConfig {
        // Keep 100 KiB of the 300 KiB cached: 200 artifacts have to go.
        max_size_bytes: Some(100 * 1024),
        registry: "npm".to_owned(),
        ..Default::default()
    };
    let report = svc(meta.clone(), storage.clone(), config)
        .run_all(true, &admin())
        .await
        .unwrap();

    assert_eq!(report.evicted_lru, 200, "300 KiB down to a 100 KiB cap");
    let unique: std::collections::HashSet<&String> = report.evicted_keys.iter().collect();
    assert_eq!(
        unique.len(),
        report.evicted_keys.len(),
        "a preview that lists the same key twice is counting a deletion that cannot happen twice"
    );
    assert_eq!(unique.len(), 200);
    assert!(report.incomplete_because.is_none(), "one page was enough");
    assert_eq!(
        storage.keys().len(),
        300,
        "and nothing was actually dropped"
    );
}

// ── The coherence sweep: preview, and the trail ───────────────────────────

/// Two tracked artifacts and one orphan, in a registry.
fn orphan_estate() -> (Arc<InMemArtifactMeta>, Arc<InMemStorage>) {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    storage.seed("artifact:npm/a:1.0", b"data");
    storage.seed("artifact:npm/orphan:1.0", b"orphan");
    meta.seed(make_meta(
        "artifact:npm/a:1.0",
        "npm",
        "a",
        "1.0",
        10,
        Duration::hours(1),
        Duration::hours(1),
    ));
    (meta, storage)
}

fn npm_config() -> EvictionConfig {
    EvictionConfig {
        registry: "npm".to_owned(),
        ..Default::default()
    }
}

/// **The one thing a coherence preview must not do.**
///
/// The two-pass grace is the whole safety property: a blob goes only if it
/// looked orphaned on the *previous* run too. A dry run that carried its
/// findings forward would arm the deletions it was asked to describe — preview
/// twice, and the second one deletes. So a preview leaves the pending set
/// exactly as it found it, and a live run after two previews still defers.
#[tokio::test]
async fn a_coherence_preview_does_not_advance_a_blob_toward_deletion() {
    let (meta, storage) = orphan_estate();
    let service = svc(meta, storage.clone(), npm_config());

    for _ in 0..2 {
        let report = service.run_coherence_check(true, &admin()).await.unwrap();
        assert!(report.dry_run);
        assert_eq!(report.orphaned_deleted, 0, "nothing is deletable yet");
        assert_eq!(report.first_seen_orphaned, 1);
        assert_eq!(report.first_seen_keys, vec!["artifact:npm/orphan:1.0"]);
    }

    // A live run now behaves as if the previews never happened: still a first
    // sighting, still deferred.
    let live = service.run_coherence_check(false, &admin()).await.unwrap();
    assert_eq!(
        live.orphaned_deleted, 0,
        "two previews must not have armed the deletion"
    );
    assert!(storage.contains("artifact:npm/orphan:1.0"));
}

/// Once a live run has seen the orphan, the preview says what the *next* run
/// would take — with the key, which is the only record of a blob whose meta row
/// is precisely what is missing.
#[tokio::test]
async fn a_coherence_preview_names_what_the_next_run_would_delete() {
    let (meta, storage) = orphan_estate();
    let service = svc(meta, storage.clone(), npm_config());

    service.run_coherence_check(false, &admin()).await.unwrap();

    let preview = service.run_coherence_check(true, &admin()).await.unwrap();
    assert_eq!(preview.orphaned_deleted, 1, "second strike, so it would go");
    assert_eq!(preview.deleted_keys, vec!["artifact:npm/orphan:1.0"]);
    assert_eq!(preview.first_seen_orphaned, 0);
    assert!(
        storage.contains("artifact:npm/orphan:1.0"),
        "and it is still there"
    );

    // The preview did not consume the pending state either: the live run that
    // follows still deletes.
    let live = service.run_coherence_check(false, &admin()).await.unwrap();
    assert_eq!(live.orphaned_deleted, 1);
    assert_eq!(live.deleted_keys, vec!["artifact:npm/orphan:1.0"]);
    assert!(!storage.contains("artifact:npm/orphan:1.0"));
}

/// A sweep is its own action: collecting a leak is not a policy trimming a
/// cache, and neither is a package deletion.
#[tokio::test]
async fn a_coherence_sweep_is_audited_as_its_own_action() {
    let (meta, storage) = orphan_estate();
    let sink = AuditSink::arc();
    let service = svc(meta, storage, npm_config()).with_audit(sink.clone());

    service.run_coherence_check(false, &admin()).await.unwrap();
    assert_eq!(sink.of(AccessAction::CacheCoherenceRun).len(), 1);
    let coord = sink.of(AccessAction::CacheCoherenceRun)[0]
        .package_id
        .clone()
        .unwrap();
    assert_eq!(coord.registry, "npm");

    service.run_coherence_check(true, &admin()).await.unwrap();
    assert_eq!(sink.of(AccessAction::CacheCoherenceDryRun).len(), 1);
    assert_eq!(
        sink.of(AccessAction::CacheCoherenceRun).len(),
        1,
        "the preview is not filed as a run that could have written"
    );

    for other in [
        AccessAction::CacheEvictRun,
        AccessAction::CacheEvict,
        AccessAction::Delete,
    ] {
        assert!(sink.of(other).is_empty(), "{other} is a different fact");
    }
}

#[test]
fn a_config_with_no_strategy_evicts_nothing() {
    assert!(!EvictionConfig::default().evicts_anything());
    assert!(!npm_config().evicts_anything());
    assert!(ttl_config().evicts_anything());
    assert!(EvictionConfig {
        keep_latest_n: Some(1),
        ..Default::default()
    }
    .evicts_anything());
}

// ── RFC 0014 §5.3: the upstream-disappearance hold ───────────────────────────

/// A status store that holds a fixed set of `hold_key` forms.
struct FixedHold(std::collections::HashSet<String>);

#[async_trait]
impl crate::ports::UpstreamStatusPort for FixedHold {
    async fn record_miss(
        &self,
        _: crate::entities::MissObservation<'_>,
    ) -> Result<crate::entities::UpstreamStatus, CoreError> {
        unreachable!("eviction never writes")
    }
    async fn confirm(
        &self,
        _: &crate::entities::UpstreamKey<'_>,
        _: chrono::DateTime<Utc>,
    ) -> Result<(), CoreError> {
        unreachable!("eviction never writes")
    }
    async fn clear(
        &self,
        _: &crate::entities::UpstreamKey<'_>,
    ) -> Result<Option<crate::entities::UpstreamStatus>, CoreError> {
        unreachable!("eviction never writes")
    }
    async fn get(
        &self,
        _: &crate::entities::UpstreamKey<'_>,
    ) -> Result<Option<crate::entities::UpstreamStatus>, CoreError> {
        Ok(None)
    }
    async fn list(
        &self,
        _: crate::entities::UpstreamStatusFilter,
    ) -> Result<Vec<crate::entities::UpstreamStatus>, CoreError> {
        Ok(vec![])
    }
    async fn count(&self, _: crate::entities::UpstreamStatusFilter) -> Result<u64, CoreError> {
        Ok(0)
    }
    async fn disappeared_keys(
        &self,
        _: &str,
    ) -> Result<std::collections::HashSet<String>, CoreError> {
        Ok(self.0.clone())
    }
}

fn held(keys: &[&str]) -> Arc<dyn crate::ports::UpstreamStatusPort> {
    Arc::new(FixedHold(keys.iter().map(ToString::to_string).collect()))
}

/// Two expired artifacts, one held by version and one by its whole package,
/// beside one expired and unheld.
fn seed_held_trio(meta: &InMemArtifactMeta, storage: &InMemStorage) {
    for (name, version) in [("gone", "1.0"), ("withdrawn", "2.0"), ("plain", "1.0")] {
        let key = format!("artifact:npm/{name}:{version}");
        meta.seed(make_meta(
            &key,
            "npm",
            name,
            version,
            100,
            Duration::days(30),
            Duration::days(30),
        ));
        storage.seed(&key, b"x");
    }
}

#[tokio::test]
async fn the_hold_keeps_a_disappeared_artifact_through_ttl_idle_and_keep_latest() {
    for pass in ["ttl", "idle", "keep_latest_n"] {
        let meta = InMemArtifactMeta::arc();
        let storage = InMemStorage::arc();
        seed_held_trio(&meta, &storage);
        // A second, newer version of each, so keep_latest_n = 1 has a tail.
        for name in ["gone", "withdrawn", "plain"] {
            let key = format!("artifact:npm/{name}:9.9");
            meta.seed(make_meta(
                &key,
                "npm",
                name,
                "9.9",
                100,
                Duration::minutes(1),
                Duration::minutes(1),
            ));
            storage.seed(&key, b"x");
        }
        let config = EvictionConfig {
            artifact_ttl_secs: (pass == "ttl").then_some(3600),
            idle_days: (pass == "idle").then_some(1),
            keep_latest_n: (pass == "keep_latest_n").then_some(1),
            registry: "npm".to_owned(),
            ..Default::default()
        };
        let svc = svc(meta.clone(), storage.clone(), config)
            .with_upstream_status(held(&["gone@1.0", "withdrawn"]));
        let mut report = EvictionReport::live();
        let count = match pass {
            "ttl" => svc.run_ttl(&mut report).await.unwrap(),
            "idle" => svc.run_idle(&mut report).await.unwrap(),
            _ => svc.run_keep_latest_n(&mut report).await.unwrap(),
        };
        assert_eq!(count, 1, "{pass}: only the unheld artifact goes");
        assert_eq!(report.held, 2, "{pass}");
        assert!(
            storage.contains("artifact:npm/gone:1.0"),
            "{pass}: held by version"
        );
        assert!(
            storage.contains("artifact:npm/withdrawn:2.0"),
            "{pass}: held by package"
        );
        assert!(!storage.contains("artifact:npm/plain:1.0"), "{pass}");
    }
}

#[tokio::test]
async fn the_size_cap_still_evicts_held_artifacts_but_last() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    // The held one is the *least* recently used, so LRU order alone would
    // take it first.
    for (name, age) in [("gone", 300), ("a", 200), ("b", 100)] {
        let key = format!("artifact:npm/{name}:1.0");
        meta.seed(make_meta(
            &key,
            "npm",
            name,
            "1.0",
            100,
            Duration::seconds(age),
            Duration::seconds(age),
        ));
        storage.seed(&key, b"x");
    }
    let svc = |cap: u64| {
        svc(
            meta.clone(),
            storage.clone(),
            EvictionConfig {
                max_size_bytes: Some(cap),
                registry: "npm".to_owned(),
                ..Default::default()
            },
        )
        .with_upstream_status(held(&["gone@1.0"]))
    };
    // Over by one artifact: the present LRU candidate goes, the held one stays.
    let mut report = EvictionReport::live();
    assert_eq!(svc(200).run_lru_size_cap(&mut report).await.unwrap(), 1);
    assert!(storage.contains("artifact:npm/gone:1.0"), "held sorts last");
    assert!(!storage.contains("artifact:npm/a:1.0"));
    assert_eq!(report.held, 1);
    // Over by more than the present candidates can cover: the hold yields.
    let mut report = EvictionReport::live();
    assert_eq!(svc(0).run_lru_size_cap(&mut report).await.unwrap(), 2);
    assert!(
        !storage.contains("artifact:npm/gone:1.0"),
        "the cap wins over the hold"
    );
}

#[tokio::test]
async fn without_a_status_store_nothing_is_held() {
    let meta = InMemArtifactMeta::arc();
    let storage = InMemStorage::arc();
    seed_held_trio(&meta, &storage);
    let mut report = EvictionReport::live();
    let count = svc(
        meta.clone(),
        storage.clone(),
        EvictionConfig {
            artifact_ttl_secs: Some(3600),
            registry: "npm".to_owned(),
            ..Default::default()
        },
    )
    .run_ttl(&mut report)
    .await
    .unwrap();
    assert_eq!((count, report.held), (3, 0));
}
