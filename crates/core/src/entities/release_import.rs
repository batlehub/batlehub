//! The history of a configured release import (RFC 0021 §6.5).
//!
//! A run leaves a row so the console can answer the question the scheduler
//! already knew and threw away. `server/src/watcher.rs` names it: *"'the import
//! ran and found nothing new' and 'the import has not run' are the two states an
//! operator needs to tell apart"* — and a log line cannot answer either from a
//! browser.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// One completed run of one configured import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportRun {
    pub id: Uuid,
    /// The registry the versions landed in — what the console groups by.
    pub registry: String,
    /// The source repository. One registry can have several imports configured
    /// into it and they fail independently, so a run names which one it was.
    pub repo: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub imported: u64,
    pub skipped: u64,
    pub errors: u64,
    /// Who asked. `None` is the scheduler: an interval that fired has no
    /// operator behind it, and naming one would be a lie an audit reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
    /// What failed, named rather than counted — the same reason the HTTP report
    /// lists them: "3 errors" is not something an operator can act on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures: Option<String>,
}
