//! The state machine (RFC 0014 §4.2, §5.1): the population gate, then per
//! miss record-or-confirm, per success clear-and-report.
//!
//! The invariant it protects: **no single upstream failure mode can confirm
//! a disappearance.** Confirming takes `confirm_after` sweeps that were each
//! individually valid, spanning at least `confirm_min_age`, in which this
//! package was missing while the overwhelming majority of its neighbours
//! were not.

use chrono::{DateTime, Utc};

use super::probe::ProbeOutcome;
use super::{UpstreamAuditPolicy, MIN_PROBED_FOR_RATIO};
use crate::entities::{MissObservation, UpstreamKey, UpstreamState, UpstreamStatus};
use crate::ports::UpstreamStatusPort;

/// A change of state the sweep produced. Silent misses (`missing`, not yet
/// confirmed) are not transitions: nobody is told about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// The row, and the cached versions it covers (all of them for a
    /// package-level row, one for a version row).
    Confirmed(UpstreamStatus, Vec<String>),
    /// The row that was cleared, and the versions it covered.
    Reappeared(UpstreamStatus, Vec<String>),
}

pub(super) struct Applied {
    pub missing: usize,
    pub inconclusive: usize,
    pub capped: usize,
    pub void: bool,
    pub transitions: Vec<Transition>,
    /// Every row in `disappeared` after this sweep, for the pin.
    pub disappeared: Vec<UpstreamStatus>,
}

/// Count what the probes said, before anything is written. Returns the number
/// of conclusive answers, which is the denominator of the outage ratio.
fn tally(outcomes: &[(String, Vec<String>, (ProbeOutcome, bool))], applied: &mut Applied) -> usize {
    let mut conclusive = 0usize;
    for (_, _, (outcome, capped)) in outcomes {
        if *capped {
            applied.capped += 1;
        }
        if matches!(outcome, ProbeOutcome::Inconclusive(_)) {
            applied.inconclusive += 1;
            continue;
        }
        conclusive += 1;
        if outcome.is_missing() {
            applied.missing += 1;
        }
    }
    conclusive
}

/// A package the probe found: the package row and every version row clear.
async fn present(
    status: &dyn UpstreamStatusPort,
    registry: &str,
    name: &str,
    versions: &[String],
    applied: &mut Applied,
) {
    clear(status, registry, name, None, versions, applied).await;
    for v in versions {
        clear(
            status,
            registry,
            name,
            Some(v),
            std::slice::from_ref(v),
            applied,
        )
        .await;
    }
}

/// A package that is still there with some versions gone: its package-level
/// row, if any, clears, and each version is recorded on its own answer.
#[allow(clippy::too_many_arguments)]
async fn missing_versions(
    status: &dyn UpstreamStatusPort,
    registry: &str,
    name: &str,
    versions: &[String],
    missing: &[String],
    policy: &UpstreamAuditPolicy,
    now: DateTime<Utc>,
    applied: &mut Applied,
) {
    clear(status, registry, name, None, versions, applied).await;
    for v in versions {
        if missing.contains(v) {
            miss(
                status,
                registry,
                name,
                Some(v),
                std::slice::from_ref(v),
                policy,
                now,
                None,
                applied,
            )
            .await;
        } else {
            clear(
                status,
                registry,
                name,
                Some(v),
                std::slice::from_ref(v),
                applied,
            )
            .await;
        }
    }
}

/// Apply one registry's probe outcomes to the store.
pub(super) async fn apply(
    status: &dyn UpstreamStatusPort,
    registry: &str,
    outcomes: &[(String, Vec<String>, (ProbeOutcome, bool))],
    policy: &UpstreamAuditPolicy,
    now: DateTime<Utc>,
) -> Applied {
    let mut applied = Applied {
        missing: 0,
        inconclusive: 0,
        capped: 0,
        void: false,
        transitions: Vec::new(),
        disappeared: Vec::new(),
    };
    let conclusive = tally(outcomes, &mut applied);

    // The population gate. Below the floor the ratio says nothing (one
    // package is a quarter of four) and the count and age carry the decision.
    if conclusive >= MIN_PROBED_FOR_RATIO
        && (applied.missing as f64) / (conclusive as f64) > policy.outage_ratio
    {
        applied.void = true;
        return applied;
    }

    for (name, versions, (outcome, _)) in outcomes {
        match outcome {
            ProbeOutcome::Inconclusive(_) => {}
            ProbeOutcome::Present => present(status, registry, name, versions, &mut applied).await,
            ProbeOutcome::MissingPackage => {
                miss(
                    status,
                    registry,
                    name,
                    None,
                    versions,
                    policy,
                    now,
                    None,
                    &mut applied,
                )
                .await;
            }
            ProbeOutcome::MissingVersions(missing) => {
                missing_versions(
                    status,
                    registry,
                    name,
                    versions,
                    missing,
                    policy,
                    now,
                    &mut applied,
                )
                .await;
            }
        }
    }
    applied
}

/// A successful probe clears the row — not "decrements the counter". Two
/// misses six months apart are not two thirds of a disappearance.
async fn clear(
    status: &dyn UpstreamStatusPort,
    registry: &str,
    name: &str,
    version: Option<&str>,
    covered: &[String],
    applied: &mut Applied,
) {
    let key = UpstreamKey {
        registry,
        package_name: name,
        version,
    };
    match status.clear(&key).await {
        Ok(Some(row)) => applied
            .transitions
            .push(Transition::Reappeared(row, covered.to_vec())),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(registry, package = name, error = %e, "upstream audit: clear failed")
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn miss(
    status: &dyn UpstreamStatusPort,
    registry: &str,
    name: &str,
    version: Option<&str>,
    covered: &[String],
    policy: &UpstreamAuditPolicy,
    now: DateTime<Utc>,
    error: Option<&str>,
    applied: &mut Applied,
) {
    let key = UpstreamKey {
        registry,
        package_name: name,
        version,
    };
    let row = match status
        .record_miss(MissObservation {
            key,
            at: now,
            error,
        })
        .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(registry, package = name, error = %e, "upstream audit: record_miss failed");
            return;
        }
    };
    if row.state == UpstreamState::Disappeared {
        applied.disappeared.push(row);
        return;
    }
    let age_ok = now - row.first_missed_at
        >= chrono::Duration::from_std(policy.confirm_min_age).unwrap_or(chrono::Duration::MAX);
    if row.consecutive_misses >= policy.confirm_after && age_ok {
        if let Err(e) = status.confirm(&key, now).await {
            tracing::warn!(registry, package = name, error = %e, "upstream audit: confirm failed");
            return;
        }
        let confirmed = UpstreamStatus {
            state: UpstreamState::Disappeared,
            confirmed_at: Some(now),
            ..row
        };
        applied.disappeared.push(confirmed.clone());
        applied
            .transitions
            .push(Transition::Confirmed(confirmed, covered.to_vec()));
    }
}
