use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use batlehub_core::{
    error::CoreError,
    ports::{ArtifactCacheMeta, ArtifactInventory, ArtifactMeta, ArtifactMetaRecord},
};

/// A no-op artifact-meta repository that discards all writes and returns
/// empty / non-expired results for all reads. Implements both
/// [`ArtifactCacheMeta`] and [`ArtifactInventory`] (so it satisfies the full
/// `ArtifactMetaRepository` supertrait too).
///
/// Appropriate for tests that exercise proxy or publish paths but do not
/// need eviction or cache-coherence checks.
#[derive(Debug, Default)]
pub struct NoopArtifactMetaRepository;

impl NoopArtifactMetaRepository {
    /// Returns the concrete type behind an `Arc`; it coerces to whichever of the
    /// artifact-meta `dyn` traits the caller's field requires.
    pub fn arc() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl ArtifactCacheMeta for NoopArtifactMetaRepository {
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
        _older_than: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn delete_artifact_meta(&self, _key: &str) -> Result<(), CoreError> {
        Ok(())
    }
}

#[async_trait]
impl ArtifactInventory for NoopArtifactMetaRepository {
    async fn list_artifacts(&self, _registry: &str) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }

    async fn list_artifacts_by_package(&self) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }

    async fn list_expired_by_ttl(
        &self,
        _registry: &str,
        _older_than: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }

    async fn list_idle(
        &self,
        _registry: &str,
        _idle_since: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }

    async fn total_size_bytes(&self, _registry: &str) -> Result<u64, CoreError> {
        Ok(0)
    }

    async fn list_lru(&self, _registry: &str, _limit: i64) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(vec![])
    }
}

/// An artifact-meta store that remembers what it was told, for the tests
/// and the deployments that keep no database yet want a held set to answer
/// from (RFC 0008-bis §5.1). The same rows Postgres keeps, in a map.
#[derive(Debug, Default)]
pub struct InMemoryArtifactMetaRepository {
    rows: std::sync::Mutex<std::collections::HashMap<String, Row>>,
}

#[derive(Debug, Clone)]
struct Row {
    meta: ArtifactMeta,
    checksum: Option<String>,
}

impl InMemoryArtifactMetaRepository {
    pub fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn snapshot(&self) -> Vec<Row> {
        self.rows
            .lock()
            .expect("artifact meta")
            .values()
            .cloned()
            .collect()
    }
}

#[async_trait]
impl ArtifactCacheMeta for InMemoryArtifactMetaRepository {
    async fn record_artifact(&self, rec: ArtifactMetaRecord<'_>) -> Result<(), CoreError> {
        let now = Utc::now();
        self.rows.lock().expect("artifact meta").insert(
            rec.key.to_owned(),
            Row {
                meta: ArtifactMeta {
                    artifact_key: rec.key.to_owned(),
                    registry: rec.registry.to_owned(),
                    package_name: rec.package_name.to_owned(),
                    version: rec.version.to_owned(),
                    size_bytes: rec.size,
                    cached_at: now,
                    last_accessed_at: now,
                },
                checksum: rec.checksum.map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn get_artifact_checksum(&self, key: &str) -> Result<Option<String>, CoreError> {
        Ok(self
            .rows
            .lock()
            .expect("artifact meta")
            .get(key)
            .and_then(|r| r.checksum.clone()))
    }

    async fn touch_artifact(&self, key: &str) -> Result<(), CoreError> {
        if let Some(r) = self.rows.lock().expect("artifact meta").get_mut(key) {
            r.meta.last_accessed_at = Utc::now();
        }
        Ok(())
    }

    async fn is_artifact_expired(
        &self,
        key: &str,
        older_than: DateTime<Utc>,
    ) -> Result<bool, CoreError> {
        // An unknown key is *expired*, not fresh — the port's contract, and
        // what the Postgres store answers (`NOT EXISTS(… cached_at >= $2)`).
        // Answering `false` here would report a blob with no meta row — one
        // written by a path that never called `record_artifact`, or whose row
        // was dropped while the bytes survived — as fresh forever, so
        // `artifact_ttl` would never expire it.
        Ok(!self
            .rows
            .lock()
            .expect("artifact meta")
            .get(key)
            .is_some_and(|r| r.meta.cached_at >= older_than))
    }

    async fn delete_artifact_meta(&self, key: &str) -> Result<(), CoreError> {
        self.rows.lock().expect("artifact meta").remove(key);
        Ok(())
    }

    async fn list_registry_artifacts(
        &self,
        registry: &str,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        ArtifactInventory::list_artifacts(self, registry).await
    }

    async fn list_package_artifacts(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self
            .snapshot()
            .into_iter()
            .filter(|r| r.meta.registry == registry && r.meta.package_name == package)
            .map(|r| r.meta)
            .collect())
    }
}

#[async_trait]
impl ArtifactInventory for InMemoryArtifactMetaRepository {
    async fn list_artifacts(&self, registry: &str) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self
            .snapshot()
            .into_iter()
            .filter(|r| registry.is_empty() || r.meta.registry == registry)
            .map(|r| r.meta)
            .collect())
    }

    async fn list_artifacts_by_package(&self) -> Result<Vec<ArtifactMeta>, CoreError> {
        let mut rows: Vec<ArtifactMeta> = self.snapshot().into_iter().map(|r| r.meta).collect();
        rows.sort_by(|a, b| {
            (a.registry.as_str(), a.package_name.as_str(), b.cached_at).cmp(&(
                b.registry.as_str(),
                b.package_name.as_str(),
                a.cached_at,
            ))
        });
        Ok(rows)
    }

    async fn list_expired_by_ttl(
        &self,
        registry: &str,
        older_than: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self
            .list_artifacts(registry)
            .await?
            .into_iter()
            .filter(|m| m.cached_at < older_than)
            .collect())
    }

    async fn list_idle(
        &self,
        registry: &str,
        idle_since: DateTime<Utc>,
    ) -> Result<Vec<ArtifactMeta>, CoreError> {
        Ok(self
            .list_artifacts(registry)
            .await?
            .into_iter()
            .filter(|m| m.last_accessed_at < idle_since)
            .collect())
    }

    async fn total_size_bytes(&self, registry: &str) -> Result<u64, CoreError> {
        Ok(self
            .list_artifacts(registry)
            .await?
            .iter()
            .filter_map(|m| m.size_bytes)
            .sum())
    }

    async fn list_lru(&self, registry: &str, limit: i64) -> Result<Vec<ArtifactMeta>, CoreError> {
        let mut rows = self.list_artifacts(registry).await?;
        rows.sort_by_key(|m| m.last_accessed_at);
        rows.truncate(limit.max(0) as usize);
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn all_methods_return_defaults() {
        let repo = NoopArtifactMetaRepository::arc();

        assert!(repo
            .record_artifact(ArtifactMetaRecord {
                key: "k",
                registry: "cargo",
                package_name: "tokio",
                version: "1.0",
                size: Some(100),
                checksum: None,
            })
            .await
            .is_ok());
        assert!(repo.get_artifact_checksum("k").await.unwrap().is_none());
        assert!(repo.touch_artifact("k").await.is_ok());
        assert!(repo.list_artifacts("cargo").await.unwrap().is_empty());
        assert!(repo.list_artifacts_by_package().await.unwrap().is_empty());
        assert!(repo.delete_artifact_meta("k").await.is_ok());
        assert!(!repo.is_artifact_expired("k", Utc::now()).await.unwrap());
        assert!(repo
            .list_expired_by_ttl("cargo", Utc::now())
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .list_idle("cargo", Utc::now())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(repo.total_size_bytes("cargo").await.unwrap(), 0);
        assert!(repo.list_lru("cargo", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_remembering_store_answers_the_held_set_per_package() {
        let repo = InMemoryArtifactMetaRepository::arc();
        for (key, pkg, v) in [
            ("artifact:npm/left-pad/1.3.0", "left-pad", "1.3.0"),
            ("artifact:npm/left-pad/1.2.0", "left-pad", "1.2.0"),
            ("artifact:npm/lodash/4.0.0", "lodash", "4.0.0"),
        ] {
            repo.record_artifact(ArtifactMetaRecord {
                key,
                registry: "npm",
                package_name: pkg,
                version: v,
                size: Some(10),
                checksum: Some("ab"),
            })
            .await
            .unwrap();
        }
        let held = repo
            .list_package_artifacts("npm", "left-pad")
            .await
            .unwrap();
        assert_eq!(held.len(), 2);
        assert!(held.iter().all(|m| m.package_name == "left-pad"));
        assert_eq!(repo.total_size_bytes("npm").await.unwrap(), 30);
        assert_eq!(
            repo.get_artifact_checksum("artifact:npm/lodash/4.0.0")
                .await
                .unwrap()
                .as_deref(),
            Some("ab")
        );
        repo.delete_artifact_meta("artifact:npm/lodash/4.0.0")
            .await
            .unwrap();
        assert!(repo
            .list_package_artifacts("npm", "lodash")
            .await
            .unwrap()
            .is_empty());
    }
}
