//! Where one version stands in scanning (RFC 0042 §4.2), from rows already
//! read. Pure: the reads and the visibility tier are the web layer's.

use chrono::{DateTime, Utc};

use crate::entities::{
    ReasonCode, ScanDetail, ScanJob, ScanState, ScanStatus, Verdict, EXHAUSTED_PREFIX,
};

/// The §4.2 table, first match wins.
///
/// `open` is the coordinate's open job, `last_closed` its most recently
/// closed one. The caller reads jobs **before** the verdict: the worker
/// writes the verdict and then closes the job, so a job read as closed always
/// comes with its fresh verdict (§5.1).
pub fn derive(
    security_on: bool,
    open: Option<&ScanJob>,
    last_closed: Option<&ScanJob>,
    verdict: Option<&Verdict>,
    now: DateTime<Utc>,
) -> Option<ScanStatus> {
    // Rule 1: no `[registries.security]`, no scanning, no status — not
    // `not_scanned`, which would say something is missing.
    if !security_on {
        return None;
    }
    let mut detail = ScanDetail {
        last_scanned_at: verdict.and_then(|v| v.last_scanned_at),
        attempts: open.or(last_closed).map(|j| j.attempts),
        queued_since: open.map(|j| j.created_at),
        deferred_since: open.and_then(|j| j.deferred_since),
        last_error: None,
    };
    if let Some(job) = open {
        // The `deferred since ` anchor is carried as `deferred_since`, never
        // as an error.
        if job.deferred_since.is_none() {
            detail.last_error = non_empty(job.last_error.as_deref());
        }
        // Rule 2 needs a worker: a deferred job has a future `leased_until`
        // and no `leased_by`. Rule 3 is everything else that is open.
        let leased = job.leased_by.is_some() && job.leased_until.is_some_and(|u| u > now);
        let state = if leased {
            ScanState::Scanning
        } else {
            ScanState::Queued
        };
        return Some(ScanStatus {
            state,
            verdict: None,
            detail,
        });
    }
    // Rule 4.
    let Some(verdict) = verdict else {
        return Some(ScanStatus {
            state: ScanState::NotScanned,
            verdict: None,
            detail,
        });
    };
    // Rule 5: the latest closed job only, so an old failure does not outlive
    // a later success; or the verdict's own `SCANNER_ERROR`.
    let exhausted = last_closed
        .and_then(|j| j.last_error.as_deref())
        .and_then(|e| e.strip_prefix(EXHAUSTED_PREFIX));
    if exhausted.is_some() || verdict.reason_codes.contains(&ReasonCode::ScannerError) {
        detail.last_error = non_empty(exhausted);
        return Some(ScanStatus {
            state: ScanState::Failed,
            verdict: None,
            detail,
        });
    }
    // Rule 6: a verdict written before its job (the `first_seen` race).
    if verdict.reason_codes.contains(&ReasonCode::ScanPending) {
        return Some(ScanStatus {
            state: ScanState::Queued,
            verdict: None,
            detail,
        });
    }
    // Rule 7.
    Some(ScanStatus {
        state: ScanState::Scanned,
        verdict: Some(verdict.state),
        detail,
    })
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// When a version is next looked at again (RFC 0042 §4.2 *Next rescan*).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct NextRescan {
    /// `last_scanned_at + rescan.interval_secs`; a version never scanned is
    /// due from its verdict's `evaluated_at`. Absent without an interval. In
    /// the past means due: the scheduler ticks every minute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict_interval_secs: Option<u64>,
    /// The registry-wide SBOM re-check: its last run plus
    /// `[vulnerability_scan] interval_secs`. Absent when the re-check is off
    /// or has never covered this registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sbom_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sbom_interval_secs: Option<u64>,
}

/// Both clocks of [`NextRescan`], from plain values: core cannot read the
/// `[vulnerability_scan]` config, so its interval arrives as a `Duration`.
pub fn next_rescan(
    verdict: &Verdict,
    rescan_interval: Option<std::time::Duration>,
    sbom_last_run: Option<DateTime<Utc>>,
    sbom_interval: Option<std::time::Duration>,
) -> NextRescan {
    let after = |at: DateTime<Utc>, d: std::time::Duration| {
        chrono::Duration::from_std(d)
            .ok()
            .and_then(|d| at.checked_add_signed(d))
    };
    let verdict_at = rescan_interval
        .and_then(|i| after(verdict.last_scanned_at.unwrap_or(verdict.evaluated_at), i));
    let sbom_interval = sbom_interval.filter(|_| sbom_last_run.is_some());
    NextRescan {
        verdict_at,
        verdict_interval_secs: rescan_interval.map(|d| d.as_secs()),
        sbom_at: sbom_last_run
            .zip(sbom_interval)
            .and_then(|(at, i)| after(at, i)),
        sbom_interval_secs: sbom_interval.map(|d| d.as_secs()),
    }
}

/// One piece of advice on the registry page (RFC 0042 §4.2), a closed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionCode {
    /// The upstream is degraded and stale metadata is not served.
    DegradedWithoutStale,
    /// Security is on and nothing rescans a version on a clock.
    SecurityWithoutRescan,
    /// Versions whose last scan job ran out of attempts.
    ScanFailures,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct Suggestion {
    pub code: SuggestionCode,
    /// How many versions, for `scan_failures`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

/// §4.2's suggestions, from plain values. Text and a link; never a write.
pub fn suggestions(
    degraded: bool,
    serve_stale: bool,
    security_on: bool,
    rescan_interval: Option<std::time::Duration>,
    failed: u64,
) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let mut push = |code, count| out.push(Suggestion { code, count });
    if degraded && !serve_stale {
        push(SuggestionCode::DegradedWithoutStale, None);
    }
    if security_on && rescan_interval.is_none() {
        push(SuggestionCode::SecurityWithoutRescan, None);
    }
    if failed > 0 {
        push(SuggestionCode::ScanFailures, Some(failed));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{PackageId, ScanTrigger, VerdictState};
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn job() -> ScanJob {
        ScanJob {
            id: uuid::Uuid::nil(),
            package: PackageId::new("r", "p", "1.0.0"),
            published_at: None,
            artifact_sha256: None,
            trigger: ScanTrigger::FirstSeen,
            attempts: 1,
            leased_until: None,
            created_at: now() - Duration::minutes(5),
            deferred_since: None,
            leased_by: None,
            last_error: None,
            completed_at: None,
        }
    }

    fn leased(until: DateTime<Utc>) -> ScanJob {
        ScanJob {
            leased_by: Some("worker-1".into()),
            leased_until: Some(until),
            ..job()
        }
    }

    fn exhausted(err: &str) -> ScanJob {
        ScanJob {
            attempts: 3,
            last_error: Some(format!("{EXHAUSTED_PREFIX}{err}")),
            trigger: ScanTrigger::Rescan,
            ..job()
        }
    }

    fn verdict(state: VerdictState, codes: &[ReasonCode]) -> Verdict {
        Verdict {
            package: PackageId::new("r", "p", "1.0.0"),
            state,
            reason_codes: codes.to_vec(),
            findings: vec![],
            policy_ref: "r/default".into(),
            available_at: None,
            evaluated_at: now(),
            last_scanned_at: Some(now() - Duration::hours(2)),
            scanners_done: vec!["osv".into()],
        }
    }

    fn state(s: Option<ScanStatus>) -> (ScanState, Option<VerdictState>) {
        let s = s.expect("a status");
        (s.state, s.verdict)
    }

    #[test]
    fn rule_1_no_security_section_is_absent_whatever_the_rows_say() {
        let v = verdict(VerdictState::Denied, &[]);
        assert_eq!(derive(false, Some(&job()), None, Some(&v), now()), None);
    }

    #[test]
    fn rule_2_a_live_lease_with_a_worker_is_scanning() {
        let j = leased(now() + Duration::seconds(30));
        assert_eq!(
            state(derive(true, Some(&j), None, None, now())),
            (ScanState::Scanning, None)
        );
    }

    #[test]
    fn rule_2_beats_rule_5_a_rescan_of_a_failed_version_is_scanning() {
        let j = leased(now() + Duration::seconds(30));
        let v = verdict(VerdictState::Quarantined, &[ReasonCode::ScannerError]);
        let closed = exhausted("tar: unexpected EOF");
        assert_eq!(
            state(derive(true, Some(&j), Some(&closed), Some(&v), now())),
            (ScanState::Scanning, None)
        );
    }

    #[test]
    fn rule_3_a_deferred_job_is_queued_not_scanning() {
        let j = ScanJob {
            leased_by: None,
            leased_until: Some(now() + Duration::hours(1)),
            deferred_since: Some(now() - Duration::minutes(1)),
            last_error: Some("deferred since 2026-11-10T11:59:00Z".into()),
            ..job()
        };
        let s = derive(true, Some(&j), None, None, now()).unwrap();
        assert_eq!(s.state, ScanState::Queued);
        assert_eq!(s.detail.deferred_since, j.deferred_since);
        // The anchor is a date, not an error.
        assert_eq!(s.detail.last_error, None);
    }

    #[test]
    fn rule_3_a_lease_expired_by_one_second_is_queued() {
        let j = leased(now() - Duration::seconds(1));
        assert_eq!(
            state(derive(true, Some(&j), None, None, now())),
            (ScanState::Queued, None)
        );
    }

    #[test]
    fn rule_3_a_never_leased_job_is_queued_with_its_age() {
        let s = derive(true, Some(&job()), None, None, now()).unwrap();
        assert_eq!(s.state, ScanState::Queued);
        assert_eq!(s.detail.queued_since, Some(job().created_at));
    }

    #[test]
    fn rule_4_no_verdict_row_is_not_scanned() {
        assert_eq!(
            state(derive(true, None, None, None, now())),
            (ScanState::NotScanned, None)
        );
    }

    #[test]
    fn rule_5_an_exhausted_rescan_on_an_allowed_version_is_failed() {
        let v = verdict(VerdictState::Allowed, &[]);
        let closed = exhausted("tar: unexpected EOF");
        let s = derive(true, None, Some(&closed), Some(&v), now()).unwrap();
        assert_eq!((s.state, s.verdict), (ScanState::Failed, None));
        assert_eq!(s.detail.last_error.as_deref(), Some("tar: unexpected EOF"));
        assert_eq!(s.detail.attempts, Some(3));
    }

    #[test]
    fn rule_5_an_exhausted_job_with_no_error_recorded_has_none() {
        let v = verdict(VerdictState::Allowed, &[]);
        let closed = exhausted("");
        let s = derive(true, None, Some(&closed), Some(&v), now()).unwrap();
        assert_eq!(s.state, ScanState::Failed);
        assert_eq!(s.detail.last_error, None);
    }

    #[test]
    fn rule_5_scanner_error_on_the_verdict_is_failed() {
        let v = verdict(VerdictState::Quarantined, &[ReasonCode::ScannerError]);
        assert_eq!(
            state(derive(true, None, None, Some(&v), now())),
            (ScanState::Failed, None)
        );
    }

    #[test]
    fn a_failure_followed_by_a_successful_later_job_is_scanned() {
        // Only the latest closed job is read: a success closes with no prefix.
        let v = verdict(VerdictState::Allowed, &[]);
        let later = ScanJob {
            last_error: Some("tar: unexpected EOF".into()),
            ..job()
        };
        assert_eq!(
            state(derive(true, None, Some(&later), Some(&v), now())),
            (ScanState::Scanned, Some(VerdictState::Allowed))
        );
    }

    #[test]
    fn rule_6_scan_pending_with_no_job_is_queued() {
        let v = verdict(VerdictState::Quarantined, &[ReasonCode::ScanPending]);
        assert_eq!(
            state(derive(true, None, None, Some(&v), now())),
            (ScanState::Queued, None)
        );
    }

    #[test]
    fn rule_7_scanned_carries_the_verdict_state() {
        for st in [
            VerdictState::Allowed,
            VerdictState::Warned,
            VerdictState::Quarantined,
            VerdictState::Denied,
        ] {
            let v = verdict(st, &[]);
            let s = derive(true, None, None, Some(&v), now()).unwrap();
            assert_eq!((s.state, s.verdict), (ScanState::Scanned, Some(st)));
            assert_eq!(s.detail.last_scanned_at, v.last_scanned_at);
        }
    }

    fn scanned(at: Option<DateTime<Utc>>) -> Verdict {
        Verdict {
            package: PackageId::new("r", "p", "1.0.0"),
            state: VerdictState::Allowed,
            reason_codes: vec![],
            findings: vec![],
            policy_ref: "r/default".into(),
            available_at: None,
            evaluated_at: now() - Duration::days(30),
            last_scanned_at: at,
            scanners_done: vec![],
        }
    }

    #[test]
    fn next_rescan_adds_each_interval_to_its_own_clock() {
        let hour = std::time::Duration::from_secs(3600);
        let day = std::time::Duration::from_secs(86_400);
        let n = next_rescan(&scanned(Some(now())), Some(hour), Some(now()), Some(day));
        assert_eq!(n.verdict_at, Some(now() + Duration::hours(1)));
        assert_eq!(n.verdict_interval_secs, Some(3600));
        assert_eq!(n.sbom_at, Some(now() + Duration::days(1)));
        assert_eq!(n.sbom_interval_secs, Some(86_400));
    }

    #[test]
    fn next_rescan_is_absent_without_an_interval_or_an_sbom_run() {
        let n = next_rescan(
            &scanned(Some(now())),
            None,
            None,
            Some(std::time::Duration::from_secs(60)),
        );
        assert_eq!(
            n,
            NextRescan {
                verdict_at: None,
                verdict_interval_secs: None,
                sbom_at: None,
                sbom_interval_secs: None
            }
        );
        let n = next_rescan(&scanned(Some(now())), None, Some(now()), None);
        assert_eq!(n.sbom_at, None, "SBOM re-check disabled");
    }

    #[test]
    fn a_never_scanned_version_is_due_from_its_evaluation() {
        let hour = std::time::Duration::from_secs(3600);
        let n = next_rescan(&scanned(None), Some(hour), None, None);
        let at = n.verdict_at.unwrap();
        assert_eq!(at, now() - Duration::days(30) + Duration::hours(1));
        assert!(at < now(), "in the past: due");
    }

    #[test]
    fn each_suggestion_fires_at_its_boundary_and_not_before() {
        let hour = Some(std::time::Duration::from_secs(3600));
        assert!(suggestions(false, false, false, None, 0).is_empty());
        assert!(
            suggestions(true, true, true, hour, 0).is_empty(),
            "stale served: no advice"
        );
        let codes = |v: Vec<Suggestion>| v.into_iter().map(|s| s.code).collect::<Vec<_>>();
        assert_eq!(
            codes(suggestions(true, false, false, None, 0)),
            [SuggestionCode::DegradedWithoutStale]
        );
        assert_eq!(
            codes(suggestions(false, false, true, None, 0)),
            [SuggestionCode::SecurityWithoutRescan]
        );
        assert_eq!(
            suggestions(false, false, true, hour, 1),
            [Suggestion {
                code: SuggestionCode::ScanFailures,
                count: Some(1)
            }]
        );
    }
}
