//! [`ClusterStatePort`] in one process: what two services sharing one instance
//! of it see as "the database the replicas share", which is how the tests drive
//! two replicas without Postgres.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use batlehub_core::{
    error::CoreError,
    ports::{ClusterStatePort, ReplicaConfig},
};

#[derive(Default)]
pub struct InMemoryClusterState {
    generations: Mutex<HashMap<String, i64>>,
    reports: Mutex<HashMap<String, ReplicaConfig>>,
}

impl InMemoryClusterState {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ClusterStatePort for InMemoryClusterState {
    async fn bump(&self, topic: &str) -> Result<(), CoreError> {
        *self
            .generations
            .lock()
            .await
            .entry(topic.to_owned())
            .or_default() += 1;
        Ok(())
    }

    async fn heartbeat(
        &self,
        replica: &str,
        fingerprint: &str,
        prefix: &str,
    ) -> Result<Vec<(String, i64)>, CoreError> {
        self.reports.lock().await.insert(
            replica.to_owned(),
            ReplicaConfig {
                replica: replica.to_owned(),
                fingerprint: fingerprint.to_owned(),
                reported_at: Utc::now(),
            },
        );
        Ok(self
            .generations
            .lock()
            .await
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), *v))
            .collect())
    }

    async fn config_reports(&self, since: DateTime<Utc>) -> Result<Vec<ReplicaConfig>, CoreError> {
        let mut reports: Vec<_> = self
            .reports
            .lock()
            .await
            .values()
            .filter(|r| r.reported_at >= since)
            .cloned()
            .collect();
        reports.sort_by(|a, b| a.replica.cmp(&b.replica));
        Ok(reports)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use batlehub_core::entities::ExploreEntry;
    use batlehub_core::services::ExploreCache;

    use super::*;

    fn entry(registry: &str) -> Vec<ExploreEntry> {
        vec![ExploreEntry {
            registry: registry.into(),
            name: "lodash".into(),
            ..Default::default()
        }]
    }

    /// Two replicas, one shared state: an invalidation on one drops the other's
    /// listing of that registry at its next heartbeat, and leaves another
    /// registry's.
    #[tokio::test]
    async fn an_invalidation_on_one_replica_reaches_the_other_at_its_next_heartbeat() {
        use batlehub_core::services::explore_cache::TOPIC_PREFIX;
        let shared: Arc<dyn ClusterStatePort> = Arc::new(InMemoryClusterState::new());
        let (a, b) = (ExploreCache::new(), ExploreCache::new());
        a.share_with(Arc::clone(&shared));
        b.share_with(Arc::clone(&shared));
        let beat = || async { shared.heartbeat("b", "fp", TOPIC_PREFIX).await.unwrap() };
        b.apply_generations(beat().await).await; // the first read only records

        b.set_packages("npm", entry("npm"), 1, vec!["npm".into()])
            .await;
        b.set_packages("pypi", entry("pypi"), 1, vec!["pypi".into()])
            .await;
        a.invalidate(Some("npm")).await;
        assert!(
            b.get_packages("npm").await.is_some(),
            "not before the heartbeat"
        );

        b.apply_generations(beat().await).await;
        assert!(b.get_packages("npm").await.is_none());
        assert!(b.get_packages("pypi").await.is_some());

        // A whole-cache invalidation is its own topic.
        a.invalidate(None).await;
        b.apply_generations(beat().await).await;
        assert!(b.get_packages("pypi").await.is_none());
    }

    #[tokio::test]
    async fn a_replica_s_latest_report_replaces_its_previous_one() {
        let state = InMemoryClusterState::new();
        let since = chrono::Utc::now() - chrono::Duration::seconds(1);
        state.heartbeat("b", "x", "explore:").await.unwrap();
        state.heartbeat("a", "x", "explore:").await.unwrap();
        state.heartbeat("a", "y", "explore:").await.unwrap();
        let reports = state.config_reports(since).await.unwrap();
        let got: Vec<_> = reports
            .iter()
            .map(|r| (r.replica.as_str(), r.fingerprint.as_str()))
            .collect();
        assert_eq!(got, [("a", "y"), ("b", "x")]);
        let later = chrono::Utc::now() + chrono::Duration::seconds(1);
        assert!(state.config_reports(later).await.unwrap().is_empty());
    }
}
