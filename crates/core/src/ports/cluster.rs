//! State every replica of one instance shares, so a change made on one reaches
//! the others.
//!
//! Each process keeps its own copy of what it serves — the catalogue's cached
//! listings, the configuration in force — and a request changes the copy of
//! the replica that handled it. This port is how the others find out — a
//! counter per topic for what only needs dropping — and how an admin finds out
//! that they no longer serve the same configuration. Readers **poll** it; nothing is
//! pushed, so a replica that was restarting or partitioned catches up on its
//! next read instead of missing a notification.

use crate::error::CoreError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// What one replica last said about the configuration it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaConfig {
    /// The replica's name — its pod or host name.
    pub replica: String,
    /// A digest of the configuration in force there. Two replicas serving the
    /// same configuration report the same one.
    pub fingerprint: String,
    pub reported_at: DateTime<Utc>,
}

#[async_trait]
pub trait ClusterStatePort: Send + Sync {
    /// Advance `topic`'s counter.
    async fn bump(&self, topic: &str) -> Result<(), CoreError>;

    /// One replica's periodic round trip, in a single statement: record the
    /// fingerprint of the configuration `replica` serves, and return every
    /// topic starting with `prefix` with its counter.
    ///
    /// One call rather than two because it runs on every replica every few
    /// seconds, idle or not, and an idle server's statement count is budgeted.
    async fn heartbeat(
        &self,
        replica: &str,
        fingerprint: &str,
        prefix: &str,
    ) -> Result<Vec<(String, i64)>, CoreError>;

    /// Every replica that reported since `since`, with what it reported last.
    async fn config_reports(&self, since: DateTime<Utc>) -> Result<Vec<ReplicaConfig>, CoreError>;
}
