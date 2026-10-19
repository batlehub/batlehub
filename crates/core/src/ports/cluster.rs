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

    /// Every topic starting with `prefix`, with its counter.
    async fn generations(&self, prefix: &str) -> Result<Vec<(String, i64)>, CoreError>;

    /// Record the fingerprint of the configuration `replica` serves, now.
    async fn report_config(&self, replica: &str, fingerprint: &str) -> Result<(), CoreError>;

    /// Every replica that reported since `since`, with what it reported last.
    async fn config_reports(&self, since: DateTime<Utc>) -> Result<Vec<ReplicaConfig>, CoreError>;
}
