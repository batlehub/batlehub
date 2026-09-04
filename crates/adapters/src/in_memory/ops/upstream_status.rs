//! `upstream_status` in memory, for the web integration tests and the
//! server's in-memory mode.

use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use batlehub_core::{
    entities::{
        truncate_error, MissObservation, UpstreamKey, UpstreamState, UpstreamStatus,
        UpstreamStatusFilter,
    },
    error::CoreError,
    ports::UpstreamStatusPort,
};

type Key = (String, String, Option<String>);

#[derive(Default)]
pub struct InMemoryUpstreamStatusStore {
    rows: RwLock<BTreeMap<Key, UpstreamStatus>>,
}

impl InMemoryUpstreamStatusStore {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }
}

fn key_of(key: &UpstreamKey<'_>) -> Key {
    (
        key.registry.to_owned(),
        key.package_name.to_owned(),
        key.version.map(str::to_owned),
    )
}

#[async_trait]
impl UpstreamStatusPort for InMemoryUpstreamStatusStore {
    async fn record_miss(&self, obs: MissObservation<'_>) -> Result<UpstreamStatus, CoreError> {
        let mut rows = self.rows.write().await;
        let row = rows
            .entry(key_of(&obs.key))
            .and_modify(|r| {
                r.consecutive_misses += 1;
                r.last_checked_at = obs.at;
                if let Some(e) = obs.error {
                    r.last_error = Some(truncate_error(e));
                }
            })
            .or_insert_with(|| UpstreamStatus {
                registry: obs.key.registry.to_owned(),
                package_name: obs.key.package_name.to_owned(),
                version: obs.key.version.map(str::to_owned),
                state: UpstreamState::Missing,
                first_missed_at: obs.at,
                last_checked_at: obs.at,
                confirmed_at: None,
                consecutive_misses: 1,
                last_error: obs.error.map(truncate_error),
            });
        Ok(row.clone())
    }

    async fn confirm(&self, key: &UpstreamKey<'_>, at: DateTime<Utc>) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        let row = rows.get_mut(&key_of(key)).ok_or_else(|| {
            CoreError::NotFound(format!(
                "no upstream_status row for {}/{}",
                key.registry, key.package_name
            ))
        })?;
        row.state = UpstreamState::Disappeared;
        row.confirmed_at.get_or_insert(at);
        Ok(())
    }

    async fn clear(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError> {
        Ok(self.rows.write().await.remove(&key_of(key)))
    }

    async fn get(&self, key: &UpstreamKey<'_>) -> Result<Option<UpstreamStatus>, CoreError> {
        Ok(self.rows.read().await.get(&key_of(key)).cloned())
    }

    async fn list(&self, filter: UpstreamStatusFilter) -> Result<Vec<UpstreamStatus>, CoreError> {
        let mut out: Vec<UpstreamStatus> = self
            .rows
            .read()
            .await
            .values()
            .filter(|r| filter.registry.as_ref().is_none_or(|g| *g == r.registry))
            .filter(|r| filter.state.is_none_or(|s| s == r.state))
            .cloned()
            .collect();
        out.sort_by_key(|r| r.first_missed_at);
        let out: Vec<UpstreamStatus> = out.into_iter().skip(filter.offset).collect();
        Ok(if filter.limit == 0 {
            out
        } else {
            out.into_iter().take(filter.limit).collect()
        })
    }

    async fn count(&self, filter: UpstreamStatusFilter) -> Result<u64, CoreError> {
        Ok(self
            .rows
            .read()
            .await
            .values()
            .filter(|r| filter.registry.as_ref().is_none_or(|g| *g == r.registry))
            .filter(|r| filter.state.is_none_or(|s| s == r.state))
            .count() as u64)
    }

    async fn disappeared_keys(&self, registry: &str) -> Result<HashSet<String>, CoreError> {
        Ok(self
            .rows
            .read()
            .await
            .values()
            .filter(|r| r.registry == registry && r.state == UpstreamState::Disappeared)
            .map(UpstreamStatus::coordinate)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn misses_accumulate_and_a_clear_reports_what_was_there() {
        let store = InMemoryUpstreamStatusStore::new();
        let key = UpstreamKey::version("r", "p", "1.0.0");
        let now = Utc::now();
        let first = store
            .record_miss(MissObservation {
                key,
                at: now,
                error: Some("404"),
            })
            .await
            .unwrap();
        assert_eq!(first.consecutive_misses, 1);
        let second = store
            .record_miss(MissObservation {
                key,
                at: now,
                error: None,
            })
            .await
            .unwrap();
        assert_eq!(second.consecutive_misses, 2);
        assert_eq!(second.last_error.as_deref(), Some("404"));
        store.confirm(&key, now).await.unwrap();
        assert_eq!(
            store.disappeared_keys("r").await.unwrap(),
            HashSet::from(["p@1.0.0".to_owned()])
        );
        let gone = store.clear(&key).await.unwrap().unwrap();
        assert_eq!(gone.state, UpstreamState::Disappeared);
        assert!(store.clear(&key).await.unwrap().is_none());
        assert!(store.confirm(&key, now).await.is_err());
    }
}
