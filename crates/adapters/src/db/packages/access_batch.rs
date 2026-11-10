//! Access events written in batches rather than one statement per request
//! (TD-0004 §2).
//!
//! [`AccessBatcher::send`] hands an event to a task that writes everything
//! queued in the last [`WINDOW`] (or [`MAX_BATCH`] events, whichever comes
//! first) as one multi-row `INSERT`, together with the first-sight
//! `package_statuses` rows. A request no longer holds a pool connection for its
//! audit row.
//!
//! No event is dropped on that path: a full queue writes the event inline as
//! before, a failed batch is retried one row at a time, and
//! [`AccessBatcher::flush`] (called by the server once it has stopped taking
//! requests) returns only when everything queued is written. `created_at` is
//! the event's own timestamp, so a row lands in the seal window it belongs to
//! (RFC 0036 §5.3 seals a window one window late).

use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};

use super::{action_to_str, crud, role_to_str, AccessAction, AccessEvent, AccessResult, Uuid};

/// How long an event may wait for the next batch.
const WINDOW: Duration = Duration::from_millis(100);
/// Rows per statement; 18 binds each stays far under Postgres's 65 535.
const MAX_BATCH: usize = 1000;
/// Events queued before a sender writes inline instead.
const QUEUE: usize = 10_000;

enum Msg {
    Event(Box<AccessEvent>),
    Flush(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct AccessBatcher {
    tx: mpsc::Sender<Msg>,
}

impl AccessBatcher {
    pub fn spawn(pool: PgPool) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        tokio::spawn(run(pool, rx));
        Self { tx }
    }

    /// Queue `event`; `Err` gives it back when the queue is full or closed.
    pub fn send(&self, event: AccessEvent) -> Result<(), Box<AccessEvent>> {
        self.tx
            .try_send(Msg::Event(Box::new(event)))
            .map_err(|e| match e.into_inner() {
                Msg::Event(ev) => ev,
                Msg::Flush(_) => unreachable!("only events are sent here"),
            })
    }

    /// Return once every event queued before this call is written.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.tx.send(Msg::Flush(done)).await.is_ok() {
            let _ = wait.await;
        }
    }
}

async fn run(pool: PgPool, mut rx: mpsc::Receiver<Msg>) {
    let mut batch: Vec<AccessEvent> = Vec::new();
    while let Some(first) = rx.recv().await {
        let mut flushes = Vec::new();
        let take = |m: Msg, batch: &mut Vec<AccessEvent>, flushes: &mut Vec<_>| match m {
            Msg::Event(e) => batch.push(*e),
            Msg::Flush(done) => flushes.push(done),
        };
        take(first, &mut batch, &mut flushes);
        let deadline = tokio::time::Instant::now() + WINDOW;
        while flushes.is_empty() && batch.len() < MAX_BATCH {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(m)) => take(m, &mut batch, &mut flushes),
                Ok(None) | Err(_) => break,
            }
        }
        write(&pool, &batch).await;
        batch.clear();
        for done in flushes {
            let _ = done.send(());
        }
    }
}

async fn write(pool: &PgPool, batch: &[AccessEvent]) {
    if batch.is_empty() {
        return;
    }
    metrics::histogram!("batlehub_access_event_batch_rows").record(batch.len() as f64);
    match insert_batch(pool, batch).await {
        Ok(()) => {
            for e in batch {
                batlehub_core::services::audit_stream::emit(e, true);
            }
        }
        // The database is unreachable, not one row bad: retrying each row
        // would wait `acquire_timeout` per row (up to 1000 × 30 s) while
        // requests flush behind it (review of #311).
        Err(err) if is_connection_error(&err) => {
            tracing::warn!(error = %err, rows = batch.len(), "access event batch failed: database unreachable, rows dropped");
            metrics::counter!("batlehub_access_events_dropped_total").increment(batch.len() as u64);
            for e in batch {
                batlehub_core::services::audit_stream::emit(e, false);
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, rows = batch.len(), "access event batch failed, writing its rows one at a time");
            for e in batch {
                let r = crud::record_access_impl(pool, e).await;
                if let Err(err) = &r {
                    tracing::warn!(error = %err, "recording an access event failed");
                }
                batlehub_core::services::audit_stream::emit(e, r.is_ok());
            }
        }
    }
}

/// An error about reaching the database, which a per-row retry cannot fix.
fn is_connection_error(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
    )
}

/// The same two inserts as `record_access_impl`, for many events at once.
async fn insert_batch(pool: &PgPool, batch: &[AccessEvent]) -> Result<(), sqlx::Error> {
    let n = batch.len();
    let mut id = Vec::with_capacity(n);
    let mut user_id = Vec::with_capacity(n);
    let mut role = Vec::with_capacity(n);
    let mut registry = Vec::with_capacity(n);
    let mut name = Vec::with_capacity(n);
    let mut version = Vec::with_capacity(n);
    let mut artifact = Vec::with_capacity(n);
    let mut action = Vec::with_capacity(n);
    let mut outcome = Vec::with_capacity(n);
    let mut deny = Vec::with_capacity(n);
    let mut at = Vec::with_capacity(n);
    let mut ip = Vec::with_capacity(n);
    let mut ua = Vec::with_capacity(n);
    let mut throttled = Vec::with_capacity(n);
    let mut detail = Vec::with_capacity(n);
    let mut reason_code = Vec::with_capacity(n);
    let mut status_id = Vec::with_capacity(n);
    let mut creates = Vec::with_capacity(n);
    for e in batch {
        let (o, d) = match &e.result {
            AccessResult::Allowed => ("allowed", None),
            AccessResult::Denied { reason } => ("denied", Some(reason.clone())),
            AccessResult::ProxyError { reason } => ("error", Some(reason.clone())),
        };
        let p = e.package_id.as_ref();
        id.push(e.id);
        user_id.push(e.user_id.clone());
        role.push(role_to_str(&e.user_role));
        registry.push(p.map(|p| p.registry.clone()));
        name.push(p.map(|p| p.name.clone()));
        version.push(p.map(|p| p.version.clone()));
        artifact.push(p.and_then(|p| p.artifact.clone()));
        action.push(action_to_str(&e.action));
        outcome.push(o);
        deny.push(d);
        at.push(e.timestamp);
        ip.push(e.ip_address.clone());
        ua.push(e.user_agent.clone());
        throttled.push(e.throttled_count.map(|n| n as i32));
        detail.push(e.detail.clone());
        reason_code.push(e.reason_code.clone());
        status_id.push(Uuid::new_v4());
        // Same rule as `record_access_impl`.
        creates.push(
            p.is_some()
                && matches!(e.result, AccessResult::Allowed)
                && matches!(
                    e.action,
                    AccessAction::Download
                        | AccessAction::ViewMetadata
                        | AccessAction::Block
                        | AccessAction::Unblock
                ),
        );
    }
    let query = sqlx::query(
        r#"
        WITH ev AS (
            SELECT * FROM UNNEST(
                $1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[],
                $7::text[], $8::text[], $9::text[], $10::text[], $11::timestamptz[],
                $12::text[], $13::text[], $14::int[], $15::text[], $16::text[],
                $17::uuid[], $18::bool[]
            ) AS t(id, user_id, user_role, registry, package_name, package_version,
                   package_artifact, action, outcome, deny_reason, created_at,
                   ip_address, user_agent, throttled_count, detail, reason_code,
                   status_id, creates)
        ), event AS (
            INSERT INTO access_events
                (id, user_id, user_role, registry, package_name, package_version,
                 package_artifact, action, outcome, deny_reason, created_at,
                 ip_address, user_agent, throttled_count, detail, reason_code)
            SELECT id, user_id, user_role, registry, package_name, package_version,
                   package_artifact, action, outcome, deny_reason, created_at,
                   ip_address, user_agent, throttled_count, detail, reason_code
            FROM ev
        )
        INSERT INTO package_statuses
            (id, registry, package_name, package_version, package_artifact,
             status, updated_at)
        SELECT status_id, registry, package_name, package_version, package_artifact,
               'available', NOW()
        FROM ev WHERE creates
        ON CONFLICT (registry, package_name, package_version, COALESCE(package_artifact, ''))
        DO NOTHING
        "#,
    )
    .bind(id)
    .bind(user_id)
    .bind(role)
    .bind(registry)
    .bind(name)
    .bind(version)
    .bind(artifact)
    .bind(action)
    .bind(outcome)
    .bind(deny)
    .bind(at)
    .bind(ip)
    .bind(ua)
    .bind(throttled)
    .bind(detail)
    .bind(reason_code)
    .bind(status_id)
    .bind(creates)
    .execute(pool);
    crate::db::timed_query("record_access_batch", query).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_connection_error;

    #[test]
    fn only_reaching_the_database_skips_the_per_row_retry() {
        assert!(is_connection_error(&sqlx::Error::PoolTimedOut));
        assert!(is_connection_error(&sqlx::Error::PoolClosed));
        assert!(is_connection_error(&sqlx::Error::Io(
            std::io::Error::other("reset")
        )));
        // A bad row is retried alone, so the rest of the batch still lands.
        assert!(!is_connection_error(&sqlx::Error::RowNotFound));
        assert!(!is_connection_error(&sqlx::Error::ColumnNotFound(
            "x".into()
        )));
    }
}
