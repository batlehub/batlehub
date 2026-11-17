//! The status rules of the security console's overview (RFC 0050 §4.2).
//!
//! Pure functions over what the sources returned, so each rule is tested at
//! its boundary here; `crates/web`'s `security_console.rs` reads the sources,
//! filters them per caller and applies these.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::entities::{ReasonCode, Verdict, VerdictState};
use crate::ports::{ScannerStatus, WorkerHeartbeat};

/// One line of the status strip. `Unknown` is a source that failed or timed
/// out: never drawn as `Ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LineState {
    Ok,
    Attention,
    Unknown,
}

impl LineState {
    pub fn from_attention(attention: bool) -> Self {
        if attention {
            Self::Attention
        } else {
            Self::Ok
        }
    }
}

/// A scanner's own state on the *Scanners & worker* page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScannerState {
    Ok,
    Attention,
    NeverRan,
}

/// The reason codes counted as malware on the *Holds* line.
pub const MALWARE_CODES: [ReasonCode; 3] = [
    ReasonCode::KnownMalicious,
    ReasonCode::MalwareSignal,
    ReasonCode::SocVerdict,
];

/// How long a `SCAN_PENDING` hold may last before it needs attention.
pub const PENDING_ATTENTION: Duration = Duration::hours(1);

/// A heartbeat younger than this is a live worker (§4.2): the worker beats
/// every 30 s from its own ticker, whatever its pass is doing, so this is
/// four beats of slack.
pub const LIVE_WORKER: Duration = Duration::seconds(120);

/// A job deferred this long needs attention.
pub const DEFERRED_ATTENTION: Duration = Duration::hours(24);

pub fn is_malware(v: &Verdict) -> bool {
    v.reason_codes.iter().any(|c| MALWARE_CODES.contains(c))
}

/// A hold that lifts on its own: quarantined only for its age or a pending
/// scan.
pub fn is_waiting(v: &Verdict) -> bool {
    v.state == VerdictState::Quarantined
        && !v.reason_codes.is_empty()
        && v.reason_codes
            .iter()
            .all(|c| matches!(c, ReasonCode::MinAgeNotMet | ReasonCode::ScanPending))
}

/// Whether one held version makes the *Holds* line `ATTENTION`: denied, or
/// quarantined for anything but `MIN_AGE_NOT_MET`, a `SCAN_PENDING` counting
/// only once it is older than an hour.
pub fn hold_needs_attention(v: &Verdict, now: DateTime<Utc>) -> bool {
    match v.state {
        VerdictState::Denied => true,
        VerdictState::Quarantined => v.reason_codes.iter().any(|c| match c {
            ReasonCode::MinAgeNotMet => false,
            ReasonCode::ScanPending => now - v.evaluated_at > PENDING_ATTENTION,
            _ => true,
        }),
        _ => false,
    }
}

pub fn pending_over_hour(v: &Verdict, now: DateTime<Utc>) -> bool {
    v.state == VerdictState::Quarantined
        && v.reason_codes.contains(&ReasonCode::ScanPending)
        && now - v.evaluated_at > PENDING_ATTENTION
}

/// Quarantined for a reason that does not lift on its own: anything but
/// `MIN_AGE_NOT_MET` and `SCAN_PENDING` (the latter counts once over the hour,
/// through [`pending_over_hour`]).
pub fn is_blocking_quarantine(v: &Verdict) -> bool {
    v.state == VerdictState::Quarantined
        && v.reason_codes
            .iter()
            .any(|c| !matches!(c, ReasonCode::MinAgeNotMet | ReasonCode::ScanPending))
}

/// [`hold_needs_attention`] over a registry set's exact counts, as the store
/// answers them (`VerdictRepository::hold_counts`).
pub fn holds_need_attention(
    denied: u64,
    quarantined_blocking: u64,
    pending_over_hour: u64,
) -> bool {
    denied > 0 || quarantined_blocking > 0 || pending_over_hour > 0
}

pub fn worker_is_live(w: &WorkerHeartbeat, now: DateTime<Utc>) -> bool {
    now - w.last_seen < LIVE_WORKER
}

/// The protected registries no live worker covers (an empty `registries`
/// on a heartbeat covers every registry).
pub fn uncovered<'a>(
    protected: impl IntoIterator<Item = &'a String>,
    workers: &[WorkerHeartbeat],
    now: DateTime<Utc>,
) -> Vec<String> {
    let live: Vec<&WorkerHeartbeat> = workers.iter().filter(|w| worker_is_live(w, now)).collect();
    protected
        .into_iter()
        .filter(|r| {
            !live
                .iter()
                .any(|w| w.registries.is_empty() || w.registries.contains(r))
        })
        .cloned()
        .collect()
}

/// A configured scanner's state: no row is *never ran*; an error newer than
/// the last success, or an open breaker, is attention.
pub fn scanner_state(row: Option<&ScannerStatus>, now: DateTime<Utc>) -> ScannerState {
    let Some(row) = row else {
        return ScannerState::NeverRan;
    };
    let failing = match (row.last_error_at, row.last_ok_at) {
        (Some(err), Some(ok)) => err > ok,
        (Some(_), None) => true,
        _ => false,
    };
    let breaker_open = row.unavailable_until.is_some_and(|t| t > now);
    if failing || breaker_open {
        ScannerState::Attention
    } else {
        ScannerState::Ok
    }
}

/// A registry serving `proxy` or `hybrid` traffic with no `[security]`
/// profile and no `cve_gate` makes the *Coverage* line `ATTENTION`.
pub fn coverage_needs_attention(serves_upstream: bool, protected: bool, cve_gate: bool) -> bool {
    serves_upstream && !protected && !cve_gate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::PackageId;

    fn verdict(state: VerdictState, codes: &[ReasonCode], age: Duration) -> Verdict {
        Verdict {
            package: PackageId::new("r", "p", "1"),
            state,
            reason_codes: codes.to_vec(),
            findings: Vec::new(),
            policy_ref: "r/default".into(),
            available_at: None,
            evaluated_at: Utc::now() - age,
            last_scanned_at: None,
            scanners_done: Vec::new(),
        }
    }

    #[test]
    fn holds_rule_at_its_boundaries() {
        let now = Utc::now();
        let q = VerdictState::Quarantined;
        assert!(hold_needs_attention(
            &verdict(VerdictState::Denied, &[], Duration::zero()),
            now
        ));
        assert!(!hold_needs_attention(
            &verdict(q, &[ReasonCode::MinAgeNotMet], Duration::days(3)),
            now
        ));
        let young = verdict(q, &[ReasonCode::ScanPending], Duration::minutes(59));
        let old = verdict(q, &[ReasonCode::ScanPending], Duration::minutes(61));
        assert!(!hold_needs_attention(&young, now));
        assert!(hold_needs_attention(&old, now));
        assert!(pending_over_hour(&old, now) && !pending_over_hour(&young, now));
        assert!(hold_needs_attention(
            &verdict(q, &[ReasonCode::SocVerdict], Duration::zero()),
            now
        ));
        assert!(!hold_needs_attention(
            &verdict(
                VerdictState::Warned,
                &[ReasonCode::Vulnerability],
                Duration::zero()
            ),
            now
        ));
        assert!(
            is_waiting(&young)
                && !is_waiting(&verdict(q, &[ReasonCode::Vulnerability], Duration::zero()))
        );
        assert!(is_malware(&verdict(
            q,
            &[ReasonCode::MalwareSignal],
            Duration::zero()
        )));
    }

    /// The count form agrees with the per-verdict rule on every case of it.
    #[test]
    fn the_counted_holds_rule_is_the_per_verdict_one() {
        let now = Utc::now();
        let q = VerdictState::Quarantined;
        for v in [
            verdict(VerdictState::Denied, &[], Duration::zero()),
            verdict(q, &[ReasonCode::MinAgeNotMet], Duration::days(3)),
            verdict(q, &[ReasonCode::ScanPending], Duration::minutes(59)),
            verdict(q, &[ReasonCode::ScanPending], Duration::minutes(61)),
            verdict(q, &[ReasonCode::SocVerdict], Duration::zero()),
            verdict(q, &[], Duration::zero()),
            verdict(
                q,
                &[ReasonCode::MinAgeNotMet, ReasonCode::Vulnerability],
                Duration::zero(),
            ),
        ] {
            let counted = holds_need_attention(
                u64::from(v.state == VerdictState::Denied),
                u64::from(is_blocking_quarantine(&v)),
                u64::from(pending_over_hour(&v, now)),
            );
            assert_eq!(counted, hold_needs_attention(&v, now), "{v:?}");
        }
    }

    #[test]
    fn a_worker_covers_only_what_it_lists_and_only_while_live() {
        let now = Utc::now();
        let beat = |regs: &[&str], age: i64| WorkerHeartbeat {
            worker_id: "w".into(),
            last_seen: now - Duration::seconds(age),
            registries: regs.iter().map(|s| s.to_string()).collect(),
        };
        let protected = vec!["npm".to_owned(), "pypi".to_owned()];
        assert_eq!(
            uncovered(&protected, &[beat(&["npm"], 10)], now),
            vec!["pypi"]
        );
        assert!(uncovered(&protected, &[beat(&[], 119)], now).is_empty());
        assert_eq!(uncovered(&protected, &[beat(&[], 121)], now).len(), 2);
    }

    #[test]
    fn scanner_states() {
        let now = Utc::now();
        let row = |ok: Option<i64>, err: Option<i64>, until: Option<i64>| ScannerStatus {
            scanner: "osv".into(),
            last_ok_at: ok.map(|s| now - Duration::seconds(s)),
            last_error_at: err.map(|s| now - Duration::seconds(s)),
            last_error_class: err.map(|_| "timeout".into()),
            unavailable_until: until.map(|s| now + Duration::seconds(s)),
        };
        assert_eq!(scanner_state(None, now), ScannerState::NeverRan);
        assert_eq!(
            scanner_state(Some(&row(Some(10), Some(60), None)), now),
            ScannerState::Ok
        );
        assert_eq!(
            scanner_state(Some(&row(Some(60), Some(10), None)), now),
            ScannerState::Attention
        );
        assert_eq!(
            scanner_state(Some(&row(None, Some(10), None)), now),
            ScannerState::Attention
        );
        assert_eq!(
            scanner_state(Some(&row(Some(1), None, Some(30))), now),
            ScannerState::Attention
        );
        assert_eq!(
            scanner_state(Some(&row(Some(1), None, Some(-30))), now),
            ScannerState::Ok
        );
    }

    #[test]
    fn coverage_rule() {
        assert!(coverage_needs_attention(true, false, false));
        assert!(!coverage_needs_attention(true, false, true));
        assert!(!coverage_needs_attention(true, true, false));
        assert!(!coverage_needs_attention(false, false, false));
    }
}
