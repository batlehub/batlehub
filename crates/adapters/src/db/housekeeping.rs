//! Retention for the operational tables nothing else prunes.
//!
//! Each row of [`SWEEPS`] is a table that only ever grew: rows past the cutoff
//! are never read again (every reader filters them out), so deleting them
//! changes no answer. Audit rows (`access_events`, `config_changes`) are not
//! here on purpose: their retention is RFC 0036 §6.3's, and absent
//! configuration keeps them.

use sqlx::PgPool;

use super::DbResultExt;
use batlehub_core::error::CoreError;

/// Rows deleted per statement, so one sweep never holds a long lock or writes
/// one huge WAL burst.
const BATCH: i64 = 5_000;

/// `(table, condition)`. The condition is trusted SQL — constants only.
const SWEEPS: &[(&str, &str)] = &[
    // `get` already ignores an expired entry.
    ("metadata_cache", "expires_at < NOW()"),
    // Every reader asks for `completed_at IS NULL`.
    ("scan_jobs", "completed_at < NOW() - INTERVAL '7 days'"),
    // The console lists the newest hundred.
    (
        "inbound_webhook_events",
        "received_at < NOW() - INTERVAL '30 days'",
    ),
    // Liveness counts heartbeats from the last minutes.
    ("worker_heartbeats", "last_seen < NOW() - INTERVAL '7 days'"),
    // Both readers ask for `unblock_at > now`.
    (
        "ip_blocks",
        "unblock_at < EXTRACT(EPOCH FROM NOW())::BIGINT",
    ),
    // Pruned per IP only when that IP misbehaves again; one that never comes
    // back left its rows forever.
    // 30 days is also the ceiling config validation puts on
    // `[ip_blocking].violation_window_secs`; move both together.
    (
        "ip_violation_counters",
        "window_start < EXTRACT(EPOCH FROM NOW() - INTERVAL '30 days')::BIGINT",
    ),
];

/// Run every sweep once; returns the rows deleted per table.
///
/// Safe to run on every replica at once: a delete is idempotent, and two
/// replicas racing on one batch only make one of them delete fewer rows.
pub async fn sweep(pool: &PgPool) -> Result<Vec<(&'static str, u64)>, CoreError> {
    let mut deleted = Vec::with_capacity(SWEEPS.len());
    for (table, cond) in SWEEPS {
        let sql = format!(
            "DELETE FROM {table} WHERE ctid IN \
             (SELECT ctid FROM {table} WHERE {cond} LIMIT {BATCH})"
        );
        let mut total = 0;
        loop {
            // Table and condition are the constants above.
            let n = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .execute(pool)
                .await
                .db_err()?
                .rows_affected();
            total += n;
            if n < BATCH as u64 {
                break;
            }
        }
        deleted.push((*table, total));
    }
    Ok(deleted)
}
