//! The audit stream (RFC 0036 §6.2): every audit row, as one JSON log line a
//! SIEM can consume.
//!
//! **The stream is the table, not a second opinion.** A line is emitted where
//! the row is written — both `PackageRepository::record_access`
//! implementations call [`emit`] — with the same fields, so a Sigma rule and an
//! export can never disagree about what happened. A failed database write still
//! emits, with `batlehub.audit.persisted = false`.
//!
//! Off unless `[logging] format = "json"` turned it on: the text format stays
//! byte-identical to what it was before the stream existed, and a download per
//! line in a human-read log is noise nobody asked for.
//!
//! Field names are ECS. Absent values are empty strings rather than missing
//! keys — a `tracing` event's field set is fixed at the call site.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::entities::{AccessEvent, AccessResult};

/// The `tracing` target every audit line is emitted on.
pub const TARGET: &str = "batlehub::audit";
/// The ECS `event.dataset` every audit line carries — the selector a collector
/// and every Sigma `logsource` key on.
pub const DATASET: &str = "batlehub.audit";

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn the stream on. Called once, by the server's tracing setup, when
/// `[logging] format = "json"`.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Emit one audit row as a stream line. `persisted` is whether the database
/// write that this line mirrors succeeded.
pub fn emit(event: &AccessEvent, persisted: bool) {
    if !is_enabled() {
        return;
    }
    let (outcome, reason) = outcome_and_reason(&event.result);
    let pkg = event.package_id.as_ref();
    // `event!` with an explicit level: `info!(target: …, a.b = …)` is
    // ambiguous to the macro once the first field is a dotted path.
    tracing::event!(
        target: TARGET,
        tracing::Level::INFO,
        batlehub.audit.persisted = persisted,
        "event.dataset" = DATASET,
        "event.kind" = "event",
        "event.category" = event.action.ecs_category(),
        "event.action" = event.action.as_str(),
        "event.outcome" = outcome,
        "event.reason" = reason,
        "event.id" = %event.id,
        "user.id" = event.user_id.as_deref().unwrap_or(""),
        "user.roles" = %event.user_role,
        "source.ip" = event.ip_address.as_deref().unwrap_or(""),
        "user_agent.original" = event.user_agent.as_deref().unwrap_or(""),
        "batlehub.registry" = pkg.map(|p| p.registry.as_str()).unwrap_or(""),
        "package.name" = pkg.map(|p| p.name.as_str()).unwrap_or(""),
        "package.version" = pkg.map(|p| p.version.as_str()).unwrap_or(""),
        "batlehub.audit.throttled_count" = event.throttled_count.unwrap_or(0),
        "batlehub.audit.detail" = event.detail.as_deref().unwrap_or(""),
        "audit"
    );
}

/// A write outside `record_access` that still belongs in the stream — the
/// `config_changes` insert (RFC 0036 §6.2). Same target, same dataset.
pub fn emit_config_change(action: &str, actor: &str, outcome: &str, persisted: bool) {
    if !is_enabled() {
        return;
    }
    // `event!` with an explicit level: `info!(target: …, a.b = …)` is
    // ambiguous to the macro once the first field is a dotted path.
    tracing::event!(
        target: TARGET,
        tracing::Level::INFO,
        batlehub.audit.persisted = persisted,
        "event.dataset" = DATASET,
        "event.kind" = "event",
        "event.category" = "configuration",
        "event.action" = action,
        "event.outcome" = outcome,
        "user.id" = actor,
        "audit"
    );
}

/// A chain record of RFC 0036 §5.3, on the stream: the copy of the chain's
/// head that leaves the host, which is what makes a truncated tail detectable.
/// `batlehub.audit.seal.seq` arriving twice with two digests is the rewrite
/// `audit_chain_gap.yml` fires on.
pub fn emit_seal(record: &crate::entities::SealRecord) {
    if !is_enabled() {
        return;
    }
    // See `emit` for why `event!` and an identifier path lead.
    tracing::event!(
        target: TARGET,
        tracing::Level::INFO,
        batlehub.audit.persisted = true,
        "event.dataset" = DATASET,
        "event.kind" = "event",
        "event.category" = "configuration",
        "event.action" = "audit_seal",
        "event.outcome" = "allowed",
        "batlehub.audit.seal.seq" = record.seq,
        "batlehub.audit.seal.kind" = record.kind.as_str(),
        "batlehub.audit.seal.window_start" = %record.window_start.to_rfc3339(),
        "batlehub.audit.seal.window_end" = %record.window_end.to_rfc3339(),
        "batlehub.audit.seal.row_count" = record.row_count,
        "batlehub.audit.seal.digest" = record.digest.as_str(),
        "batlehub.audit.seal.prev_digest" = record.prev_digest.as_str(),
        "audit"
    );
}

fn outcome_and_reason(result: &AccessResult) -> (&'static str, &str) {
    match result {
        AccessResult::Allowed => ("allowed", ""),
        AccessResult::Denied { reason } => ("denied", reason),
        AccessResult::ProxyError { reason } => ("error", reason),
    }
}
