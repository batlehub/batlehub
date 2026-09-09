//! The in-memory `LocalRegistryBackend` and no-op storage the service tests
//! publish against.
//!
//! Shared rather than copied: `release_import` publishes through the very same
//! `LocalRegistryService`, and a second hand-written fake of a 25-method trait
//! is a second chance for the two suites to disagree about what a backend does.

#![cfg(test)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;

use crate::entities::PublishedPackage;
use crate::error::CoreError;
use crate::ports::{StorageBackend, StorageMeta, StoredArtifact};

// ── Minimal mock backend ──────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct InMemBackend {
    pub(crate) versions: Mutex<Vec<PublishedPackage>>,
}

impl InMemBackend {
    pub(crate) fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub(crate) fn seed(&self, pkg: PublishedPackage) {
        self.versions.lock().unwrap().push(pkg);
    }
}

#[async_trait]
impl crate::ports::LocalRegistryBackend for InMemBackend {
    async fn publish(&self, pkg: PublishedPackage) -> Result<(), CoreError> {
        self.versions.lock().unwrap().push(pkg);
        Ok(())
    }
    /// The port's default returns an empty vec, which made every
    /// whole-registry document in this module build from no names at all — a
    /// green assertion about a document that could not have had anything in it.
    async fn list_package_names(&self, registry: &str) -> Result<Vec<String>, CoreError> {
        let mut names: Vec<String> = self
            .versions
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.registry == registry)
            .map(|p| p.name.clone())
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }
    async fn yank(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn unyank(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn deprecate(&self, _: &str, _: &str, _: &str, _: Option<&str>) -> Result<(), CoreError> {
        Ok(())
    }
    async fn undeprecate(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn set_channel(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        channel: &str,
    ) -> Result<bool, CoreError> {
        let mut v = self.versions.lock().unwrap();
        match v
            .iter_mut()
            .find(|p| p.registry == registry && p.name == name && p.version == version)
        {
            Some(p) => {
                let current = p
                    .index_metadata
                    .get("channel")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default();
                if current == channel {
                    return Ok(false);
                }
                if let Some(obj) = p.index_metadata.as_object_mut() {
                    obj.insert("channel".to_owned(), serde_json::json!(channel));
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn unlist(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn relist(&self, _: &str, _: &str, _: &str) -> Result<(), CoreError> {
        Ok(())
    }
    async fn get_versions(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        Ok(self
            .versions
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.registry == registry && p.name == name)
            .cloned()
            .collect())
    }
    async fn exists(&self, registry: &str, name: &str) -> Result<bool, CoreError> {
        Ok(self
            .versions
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.registry == registry && p.name == name))
    }
}

/// In-memory storage that actually round-trips bytes, for download-path tests
/// (re-serve checksum + signature verification).
#[derive(Default)]
pub(crate) struct MemStore {
    data: Mutex<HashMap<String, Bytes>>,
}
impl MemStore {
    pub(crate) fn arc() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub(crate) fn put(&self, key: &str, bytes: Bytes) {
        self.data.lock().unwrap().insert(key.to_owned(), bytes);
    }
}
#[async_trait]
impl StorageBackend for MemStore {
    async fn store(&self, key: &str, data: Bytes, _: StorageMeta) -> Result<(), CoreError> {
        self.data.lock().unwrap().insert(key.to_owned(), data);
        Ok(())
    }
    async fn retrieve(&self, key: &str) -> Result<Option<StoredArtifact>, CoreError> {
        Ok(self.data.lock().unwrap().get(key).cloned().map(|bytes| {
            let s: crate::ports::ByteStream =
                Box::pin(futures::stream::once(async move { Ok(bytes) }));
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

pub(crate) struct NoopStorage;

#[async_trait]
impl StorageBackend for NoopStorage {
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
        Ok(false)
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
