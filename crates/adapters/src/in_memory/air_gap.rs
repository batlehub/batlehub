//! In-memory [`MissRecorder`] for tests and for a deployment with no database.
//!
//! An air-gapped instance without a database still refuses to dial and still
//! answers `503`; what it loses is the record surviving a restart, which is
//! the same trade every other in-memory store here makes.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use batlehub_core::{
    entities::{BundleImport, ContentMiss, MissFilter, RecordedMiss, MAX_MISSES_PER_REGISTRY},
    error::CoreError,
    ports::{BundleHistory, MissRecorder},
};

#[derive(Default)]
pub struct InMemoryMissRecorder {
    rows: RwLock<HashMap<(String, String), RecordedMiss>>,
}

impl InMemoryMissRecorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

fn matches(row: &RecordedMiss, filter: &MissFilter) -> bool {
    filter.registry.as_deref().is_none_or(|r| r == row.registry)
        && filter.kind.is_none_or(|k| k == row.kind)
}

#[async_trait]
impl MissRecorder for InMemoryMissRecorder {
    async fn record(&self, miss: &ContentMiss, now: DateTime<Utc>) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        let key = (miss.registry.clone(), miss.storage_key.clone());
        match rows.get_mut(&key) {
            Some(row) => {
                row.last_seen = now;
                row.count += 1;
                if miss.requested_version.is_some() {
                    row.requested_version = miss.requested_version.clone();
                }
                row.held_versions = miss.held_versions.clone();
                return Ok(());
            }
            None => {
                // The cap, before the insert: a caller who can ask for a
                // package can write a row, so the oldest-seen row of this
                // registry makes way rather than the table growing without
                // bound.
                let held = rows
                    .values()
                    .filter(|r| r.registry == miss.registry)
                    .count() as u64;
                if held >= MAX_MISSES_PER_REGISTRY {
                    if let Some(oldest) = rows
                        .values()
                        .filter(|r| r.registry == miss.registry)
                        .min_by_key(|r| r.last_seen)
                        .map(|r| (r.registry.clone(), r.storage_key.clone()))
                    {
                        rows.remove(&oldest);
                    }
                }
                rows.insert(
                    key,
                    RecordedMiss {
                        registry: miss.registry.clone(),
                        storage_key: miss.storage_key.clone(),
                        kind: miss.kind,
                        coordinate: miss.coordinate.clone(),
                        first_seen: now,
                        last_seen: now,
                        count: 1,
                        requested_version: miss.requested_version.clone(),
                        held_versions: miss.held_versions.clone(),
                    },
                );
            }
        }
        Ok(())
    }

    async fn list(&self, filter: &MissFilter) -> Result<Vec<RecordedMiss>, CoreError> {
        let rows = self.rows.read().await;
        let mut out: Vec<RecordedMiss> = rows
            .values()
            .filter(|r| matches(r, filter))
            .cloned()
            .collect();
        // Most-asked first: the order an operator builds the next bundle in.
        out.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(b.last_seen.cmp(&a.last_seen))
                .then(a.storage_key.cmp(&b.storage_key))
        });
        let limit = if filter.limit == 0 {
            usize::MAX
        } else {
            filter.limit as usize
        };
        Ok(out
            .into_iter()
            .skip(filter.offset as usize)
            .take(limit)
            .collect())
    }

    async fn count(&self, filter: &MissFilter) -> Result<u64, CoreError> {
        Ok(self
            .rows
            .read()
            .await
            .values()
            .filter(|r| matches(r, filter))
            .count() as u64)
    }

    async fn purge(&self, before: DateTime<Utc>, registry: Option<&str>) -> Result<u64, CoreError> {
        let mut rows = self.rows.write().await;
        let before_len = rows.len();
        rows.retain(|_, r| {
            let in_scope = registry.is_none_or(|reg| reg == r.registry);
            !(in_scope && r.last_seen < before)
        });
        Ok((before_len - rows.len()) as u64)
    }
}

/// In-memory [`BundleHistory`].
#[derive(Default)]
pub struct InMemoryBundleHistory {
    rows: RwLock<Vec<BundleImport>>,
}

impl InMemoryBundleHistory {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl BundleHistory for InMemoryBundleHistory {
    async fn record(&self, import: &BundleImport) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        rows.retain(|r| r.bundle_id != import.bundle_id);
        rows.push(import.clone());
        Ok(())
    }

    async fn list(&self, limit: u64) -> Result<Vec<BundleImport>, CoreError> {
        let rows = self.rows.read().await;
        let mut out: Vec<BundleImport> = rows.clone();
        out.sort_by_key(|r| std::cmp::Reverse(r.imported_at));
        out.truncate(if limit == 0 {
            usize::MAX
        } else {
            limit as usize
        });
        Ok(out)
    }

    async fn seen(&self, bundle_id: &str) -> Result<bool, CoreError> {
        Ok(self
            .rows
            .read()
            .await
            .iter()
            .any(|r| r.bundle_id == bundle_id))
    }
}

#[cfg(test)]
mod tests {
    use batlehub_core::entities::MissKind;

    use super::*;

    fn miss(registry: &str, key: &str) -> ContentMiss {
        ContentMiss {
            registry: registry.to_owned(),
            storage_key: key.to_owned(),
            kind: MissKind::Artifact,
            coordinate: Some(key.to_owned()),
            requested_version: None,
            held_versions: Vec::new(),
        }
    }

    /// RFC 0008-bis §4.4: the version the client asked for survives a later
    /// request that named none, and the held set is as of the last request.
    #[tokio::test]
    async fn the_requested_version_is_kept_and_the_held_set_is_the_latest() {
        let r = InMemoryMissRecorder::new();
        let now = Utc::now();
        let mut first = miss("npm", "left-pad (versions)");
        first.requested_version = Some("1.2.0".into());
        first.held_versions = vec!["1.3.0".into()];
        r.record(&first, now).await.unwrap();
        let mut second = miss("npm", "left-pad (versions)");
        second.held_versions = vec!["1.3.0".into(), "1.3.1".into()];
        r.record(&second, now + chrono::Duration::minutes(1))
            .await
            .unwrap();
        let rows = r.list(&MissFilter::default()).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requested_version.as_deref(), Some("1.2.0"));
        assert_eq!(rows[0].held_versions, vec!["1.3.0", "1.3.1"]);
    }

    #[tokio::test]
    async fn a_repeat_bumps_the_counter_rather_than_adding_a_row() {
        let r = InMemoryMissRecorder::new();
        let now = Utc::now();
        r.record(&miss("npm", "a"), now).await.unwrap();
        r.record(&miss("npm", "a"), now + chrono::Duration::minutes(1))
            .await
            .unwrap();
        let rows = r.list(&MissFilter::default()).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].count, 2);
        assert_eq!(rows[0].first_seen, now);
        assert!(rows[0].last_seen > rows[0].first_seen);
    }

    #[tokio::test]
    async fn the_list_is_most_asked_first_and_filters_by_registry() {
        let r = InMemoryMissRecorder::new();
        let now = Utc::now();
        r.record(&miss("npm", "quiet"), now).await.unwrap();
        for _ in 0..3 {
            r.record(&miss("npm", "loud"), now).await.unwrap();
        }
        r.record(&miss("cargo", "other"), now).await.unwrap();
        let rows = r.list(&MissFilter::default()).await.unwrap();
        assert_eq!(rows[0].storage_key, "loud");
        assert_eq!(rows.len(), 3);
        let npm = r
            .list(&MissFilter {
                registry: Some("npm".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(npm.len(), 2);
        assert_eq!(r.count(&MissFilter::default()).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn purging_forgets_only_what_is_older_and_in_scope() {
        let r = InMemoryMissRecorder::new();
        let old = Utc::now() - chrono::Duration::days(200);
        let now = Utc::now();
        r.record(&miss("npm", "ancient"), old).await.unwrap();
        r.record(&miss("npm", "fresh"), now).await.unwrap();
        r.record(&miss("cargo", "ancient"), old).await.unwrap();
        let cutoff = Utc::now() - chrono::Duration::days(90);
        assert_eq!(r.purge(cutoff, Some("npm")).await.unwrap(), 1);
        let rows = r.list(&MissFilter::default()).await.unwrap();
        assert_eq!(rows.len(), 2, "cargo's ancient row was out of scope");
        assert_eq!(r.purge(cutoff, None).await.unwrap(), 1);
    }
}
