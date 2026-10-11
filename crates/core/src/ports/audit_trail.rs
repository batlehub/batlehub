//! The audit trail's lifecycle store (RFC 0036 §6.3–6.4): read a window's rows,
//! find rows a lifecycle change applies to, and apply a batch of row changes
//! together with the chain record that describes them — atomically.
//!
//! Deliberately low level. *What* to rewrite, the digests and the signatures
//! are decided in `services::audit_trail`, once, for every store; a store only
//! moves rows.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::entities::{AccessEvent, SealRecord};
use crate::error::CoreError;

/// Which rows a lifecycle change is looking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateQuery {
    /// Access-class rows older than `before` that still carry a full IP or a
    /// user agent.
    Pseudonymise { before: DateTime<Utc> },
    /// Access-class rows older than `access_before`, and every other row older
    /// than `security_before`. `None` leaves that class alone.
    Expire {
        access_before: Option<DateTime<Utc>>,
        security_before: Option<DateTime<Utc>>,
    },
    /// Rows naming `user_id`: as the actor, or as the subject of a
    /// `gdpr_export` (`detail = "subject=<user_id>"`).
    Subject { user_id: String },
}

/// One change to one row.
#[derive(Debug, Clone, PartialEq)]
pub enum RowOp {
    /// Replace the row's mutable columns — `user_id`, `ip_address`,
    /// `user_agent`, `detail` — with this event's. Matched on `id`.
    Replace(Box<AccessEvent>),
    Delete(Uuid),
}

/// What [`AuditTrailStore::apply`] commits in one transaction.
#[derive(Debug, Clone, Default)]
pub struct TrailBatch {
    pub ops: Vec<RowOp>,
    /// The chain record describing the change, when the rows are in a sealed
    /// window. Appended only if the chain's head is still `expect_head`.
    pub record: Option<SealRecord>,
    /// The `seq` of the head the record was built on; `None` for an empty chain.
    pub expect_head: Option<i64>,
}

/// What `gdpr erase` touched outside `access_events`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErasedElsewhere {
    /// Personal access tokens renamed — and revoked.
    pub tokens: u64,
    /// `user_blocks` and `package_statuses.blocked_by` rows renamed.
    pub blocks: u64,
}

#[async_trait]
pub trait AuditTrailStore: Send + Sync {
    /// Every chain record, by `seq`.
    async fn seal_records(&self) -> Result<Vec<SealRecord>, CoreError>;

    /// The rows of `[start, end)`, ordered by `created_at` then `id` — the
    /// order a window's digest is computed in.
    async fn rows_in(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<AccessEvent>, CoreError>;

    /// Up to `limit` rows matching `query` and created before `created_before`
    /// (when given), oldest first.
    async fn candidates(
        &self,
        query: &CandidateQuery,
        created_before: Option<DateTime<Utc>>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError>;

    /// Apply `batch.ops` and append `batch.record`, all or nothing.
    /// `CoreError::Conflict` when the chain moved since `expect_head`.
    async fn apply(&self, batch: TrailBatch) -> Result<(), CoreError>;

    /// The open decisions an erasure would strip the author or subject from:
    /// an active account block on the user, a package block they placed.
    async fn open_decisions(&self, user_id: &str) -> Result<Vec<String>, CoreError>;

    /// Rename the subject in token and block rows, revoking their tokens.
    async fn erase_elsewhere(
        &self,
        user_id: &str,
        pseudonym: &str,
    ) -> Result<ErasedElsewhere, CoreError>;

    /// Everything the instance holds about the subject, for an access request:
    /// audit rows, tokens (never their hashes), blocks, and the rows erasure
    /// exempts — publications, ownership, grants.
    async fn subject_export(&self, user_id: &str) -> Result<serde_json::Value, CoreError>;
}

/// One process at a time: the lifecycle and the sealer run on whichever
/// process holds this (RFC 0036 §6.3).
#[async_trait]
pub trait LeaderLock: Send + Sync {
    async fn try_lead(&self) -> Result<bool, CoreError>;
}
