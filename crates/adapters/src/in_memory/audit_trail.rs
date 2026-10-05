//! In-memory [`AuditTrailStore`] over [`InMemoryPackageRepository`]'s events,
//! and the tests of `services::audit_trail` that drive it.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use batlehub_core::entities::audit_seal::rows_digest;
use batlehub_core::entities::{AccessAction, AccessEvent, SealRecord};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{
    AuditTrailStore, CandidateQuery, ErasedElsewhere, LeaderLock, RowOp, TrailBatch,
};

use super::InMemoryPackageRepository;

/// One process, by construction: always the leader.
#[derive(Debug, Default)]
pub struct AlwaysLeader;

#[async_trait]
impl LeaderLock for AlwaysLeader {
    async fn try_lead(&self) -> Result<bool, CoreError> {
        Ok(true)
    }
}

fn is_access(e: &AccessEvent) -> bool {
    e.action.is_access_class()
}

fn matches(q: &CandidateQuery, e: &AccessEvent) -> bool {
    match q {
        CandidateQuery::Pseudonymise { before } => {
            is_access(e)
                && e.timestamp < *before
                && (e.user_agent.is_some()
                    || e.ip_address.as_deref().is_some_and(|ip| !ip.contains('/')))
        }
        CandidateQuery::Expire {
            access_before,
            security_before,
        } => {
            let cutoff = if is_access(e) {
                access_before
            } else {
                security_before
            };
            cutoff.is_some_and(|c| e.timestamp < c)
        }
        CandidateQuery::Subject { user_id } => {
            e.user_id.as_deref() == Some(user_id.as_str())
                || (e.action == AccessAction::GdprExport
                    && e.detail.as_deref() == Some(format!("subject={user_id}").as_str()))
        }
    }
}

fn window(events: &[AccessEvent], start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<AccessEvent> {
    let mut rows: Vec<AccessEvent> = events
        .iter()
        .filter(|e| e.timestamp >= start && e.timestamp < end)
        .cloned()
        .collect();
    rows.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));
    rows
}

#[async_trait]
impl AuditTrailStore for InMemoryPackageRepository {
    async fn seal_records(&self) -> Result<Vec<SealRecord>, CoreError> {
        Ok(self.seals.read().await.clone())
    }

    async fn rows_in(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        Ok(window(&self.events.read().await, start, end))
    }

    async fn candidates(
        &self,
        query: &CandidateQuery,
        created_before: Option<DateTime<Utc>>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        let mut rows: Vec<AccessEvent> = self
            .events
            .read()
            .await
            .iter()
            .filter(|e| created_before.is_none_or(|b| e.timestamp < b) && matches(query, e))
            .cloned()
            .collect();
        rows.sort_by_key(|e| e.timestamp);
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn apply(&self, batch: TrailBatch) -> Result<(), CoreError> {
        // Both locks for the whole batch: the in-memory transaction.
        let mut seals = self.seals.write().await;
        let mut events = self.events.write().await;
        let head = seals.last().map_or(0, |r| r.seq);
        if batch.expect_head.is_some_and(|want| want != head) {
            return Err(CoreError::Conflict(format!(
                "the seal chain moved: head is {head}, the batch was built on {:?}",
                batch.expect_head
            )));
        }
        let mut staged = events.clone();
        for op in &batch.ops {
            match op {
                RowOp::Delete(id) => staged.retain(|e| e.id != *id),
                RowOp::Replace(new) => {
                    if let Some(e) = staged.iter_mut().find(|e| e.id == new.id) {
                        e.user_id = new.user_id.clone();
                        e.ip_address = new.ip_address.clone();
                        e.user_agent = new.user_agent.clone();
                        e.detail = new.detail.clone();
                    }
                }
            }
        }
        if let Some(record) = &batch.record {
            let rows = window(&staged, record.window_start, record.window_end);
            if rows_digest(&rows) != record.rows_digest {
                return Err(CoreError::Conflict(
                    "the window's rows changed since the record was built".into(),
                ));
            }
            seals.push(record.clone());
        }
        *events = staged;
        Ok(())
    }

    async fn open_decisions(&self, _user_id: &str) -> Result<Vec<String>, CoreError> {
        Ok(Vec::new())
    }

    async fn erase_elsewhere(
        &self,
        _user_id: &str,
        _pseudonym: &str,
    ) -> Result<ErasedElsewhere, CoreError> {
        Ok(ErasedElsewhere::default())
    }

    async fn subject_export(&self, user_id: &str) -> Result<serde_json::Value, CoreError> {
        let rows: Vec<AccessEvent> = self
            .events
            .read()
            .await
            .iter()
            .filter(|e| e.user_id.as_deref() == Some(user_id))
            .cloned()
            .collect();
        Ok(serde_json::json!({ "user_id": user_id, "access_events": rows }))
    }
}

#[cfg(test)]
mod tests;
