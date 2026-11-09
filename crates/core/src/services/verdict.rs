//! The verdict pipeline's centre (RFC 0018 §5.2, §6.1): a pure evaluation,
//! the age scanner it always runs, and the read-side `current()` that turns
//! "no verdict yet" into a persisted `SCAN_PENDING` hold and a queued job.
//!
//! # Fail-closed by construction
//!
//! `evaluate` is a function of the *full* finding set. A required scanner
//! that has not answered is a `Pending` finding, a scanner that crashed is a
//! `ScannerError` finding, and both are holds — so the absence of a scan is
//! never an allow. That is the reverse of `CveGateRule` and the property the
//! design buys.
//!
//! # What `current()` re-derives
//!
//! Age is a clock: a `MIN_AGE_NOT_MET` written yesterday may have lifted
//! since. So a read never trusts the stored *state*; it keeps the stored
//! content findings (what scanners said) and re-runs the evaluation with a
//! fresh age reading and a fresh pending check. Only a change of state is
//! written back, so a hot read path costs one row read.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::entities::{
    worse_state, Finding, FindingKind, InstallHookMode, PackageId, PackageMetadata, ReasonCode,
    ScanTrigger, ScannerErrorMode, SecurityMode, SecurityPolicy, Severity, Verdict, VerdictState,
};
use crate::error::CoreError;
use crate::ports::{ScanQueue, VerdictRepository};

/// The name the age findings are filed under.
pub const AGE_SCANNER: &str = "age";
/// The name the pending findings are filed under.
pub const PENDING_SCANNER: &str = "pending";

/// The gate name an exemption silences to override a verdict (RFC 0018 §4.1):
/// `"security_verdict"` in `EXEMPTIBLE_GATES`.
pub const VERDICT_EXEMPTION_GATE: &str = "security_verdict";

/// The evaluator version a saved verdict was judged by, written into its
/// free-text `policy_ref` (RFC 0041 decision 45): a change caused only by
/// this evaluator's on-read substitutions is saved once per version, so a
/// stored row converges without being rewritten on every read.
pub const EVALUATOR_TAG: &str = "eval=2";

/// The findings the internal `AgeScanner` produces for `package` under
/// `policy` at `now` (RFC 0018 §5.2): a time-bound `MIN_AGE_NOT_MET` with its
/// `available_at`, or an open-ended `TIMESTAMP_MISSING` when the upstream
/// dated nothing and the policy holds on that.
pub fn age_findings(
    package: &PackageMetadata,
    policy: &SecurityPolicy,
    now: DateTime<Utc>,
) -> Vec<Finding> {
    let Some(published) = package.published_at else {
        if !policy.hold_missing_timestamp {
            return Vec::new();
        }
        return vec![Finding::new(
            AGE_SCANNER,
            FindingKind::Age,
            ReasonCode::TimestampMissing,
            Severity::High,
            "the upstream did not date this version, and hold_missing_timestamp holds on that",
        )];
    };
    let min_age = chrono::Duration::from_std(policy.min_age).unwrap_or_default();
    let available_at = published + min_age;
    if now >= available_at {
        return Vec::new();
    }
    let mut f = Finding::new(
        AGE_SCANNER,
        FindingKind::Age,
        ReasonCode::MinAgeNotMet,
        Severity::High,
        format!(
            "published {}, min age {}s, available in {}s",
            published.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            policy.min_age.as_secs(),
            (available_at - now).num_seconds().max(0)
        ),
    );
    f.available_at = Some(available_at);
    vec![f]
}

/// The `SCAN_PENDING` findings for the required scanners not in `done`.
pub fn pending_findings(policy: &SecurityPolicy, done: &[String]) -> Vec<Finding> {
    policy
        .required_scanners
        .iter()
        .filter(|s| !done.contains(s))
        .map(|s| {
            Finding::new(
                PENDING_SCANNER,
                FindingKind::Pending,
                ReasonCode::ScanPending,
                Severity::High,
                format!("required scanner '{s}' has not answered yet"),
            )
        })
        .collect()
}

/// The severity a finding is judged at: its `judged_as` when `evaluate` set
/// one, else what the scanner said.
fn judged_of(f: &Finding) -> Severity {
    f.judged_as.unwrap_or(f.severity)
}

/// The forge-ref codes carry the operator's own action as their severity
/// (RFC 0019 §4.2), so `unknown_severity` never applies to them.
fn is_forge_ref(code: ReasonCode) -> bool {
    matches!(
        code,
        ReasonCode::MutableRef
            | ReasonCode::TagMoved
            | ReasonCode::AssetReplaced
            | ReasonCode::PinnedRefRequired
            | ReasonCode::RawScript
    )
}

/// The on-read substitutions of RFC 0041 §4.5: a finding citing a `MAL-`
/// record is `KNOWN_MALICIOUS` whatever it was stored as (decision 36), and
/// an unrated one is judged at `unknown_severity`. `judged_as` is cleared
/// first, so a substitution made under an older setting never survives.
fn judge(findings: &mut [Finding], policy: &SecurityPolicy) {
    for f in findings.iter_mut() {
        f.judged_as = None;
        if f.code != ReasonCode::KnownMalicious && !f.code.is_always_denied() && f.names_malicious()
        {
            f.code = ReasonCode::KnownMalicious;
            f.kind = FindingKind::MalwareSignal;
            f.severity = Severity::Critical;
        }
        if f.severity == Severity::Unknown && !is_forge_ref(f.code) {
            let judged = f.severity.judged(policy.unknown_severity);
            if judged != f.severity {
                f.judged_as = Some(judged);
            }
        }
    }
}

/// A stored verdict as a reader should see it: its findings re-judged under
/// `policy` (`judged_as`, a `MAL-` citation read as `KNOWN_MALICIOUS`), its
/// state untouched. `judged_as` is never stored, so a path that answers with
/// the stored row — no metadata at hand to re-derive the whole verdict — goes
/// through this rather than showing an unrated finding with no judgement.
pub fn with_judgement(mut verdict: Verdict, policy: &SecurityPolicy) -> Verdict {
    judge(&mut verdict.findings, policy);
    verdict
}

/// Apply each scanner's escalation block (RFC 0018 §4.2): `count` findings of
/// the named kinds at or above `from`, *from that scanner*, are raised to
/// `to`. Findings are never combined across scanners.
pub fn escalate(findings: &mut [Finding], policy: &SecurityPolicy) {
    for (scanner, esc) in &policy.escalation {
        let hits: Vec<usize> = findings
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                &f.scanner == scanner && esc.kinds.contains(&f.kind) && judged_of(f) >= esc.from
            })
            .map(|(i, _)| i)
            .collect();
        if hits.len() >= esc.count.max(1) {
            for i in hits {
                if judged_of(&findings[i]) < esc.to {
                    findings[i].severity = esc.to;
                    findings[i].judged_as = None;
                }
            }
        }
    }
}

/// A finding at `severity`, judged against the registry's threshold: a refusal
/// in `block` mode, a warning in `warn` mode, and nothing at all below it.
fn threshold(severity: Severity, policy: &SecurityPolicy) -> Option<VerdictState> {
    if severity < policy.max_severity {
        return None;
    }
    Some(match policy.mode {
        SecurityMode::Block => VerdictState::Denied,
        SecurityMode::Warn => VerdictState::Warned,
    })
}

/// `SCANNER_ERROR` / `SCANNER_UNSUPPORTED`, under `policy.scanner_error`.
fn scanner_error_effect(policy: &SecurityPolicy) -> Option<VerdictState> {
    match policy.scanner_error {
        ScannerErrorMode::Quarantine => Some(VerdictState::Quarantined),
        ScannerErrorMode::Warn => Some(VerdictState::Warned),
        ScannerErrorMode::Ignore => None,
    }
}

/// `INSTALL_HOOK`, under `policy.deny_install_hooks`. `Deny` is still judged
/// against the threshold, so a registry that tolerates critical findings
/// tolerates this one too.
fn install_hook_effect(policy: &SecurityPolicy) -> Option<VerdictState> {
    match policy.deny_install_hooks {
        InstallHookMode::Deny => threshold(Severity::Critical, policy),
        InstallHookMode::Warn => Some(VerdictState::Warned),
        InstallHookMode::Ignore => None,
    }
}

/// `PROVENANCE_MISSING` / `PROVENANCE_INVALID`: a refusal only where the
/// registry requires provenance, a warning everywhere else.
fn provenance_effect(policy: &SecurityPolicy) -> Option<VerdictState> {
    if policy.require_provenance {
        threshold(Severity::Critical, policy)
    } else {
        Some(VerdictState::Warned)
    }
}

/// RFC 0019 §4.2 — the forge-ref codes carry the operator's own action in
/// their severity (`deny` → critical, `warn` → low), so they are not judged
/// against `max_severity`: a warned mutable ref must reach the client, and a
/// registry whose threshold is `critical` must not silently swallow it.
/// `mode = "warn"` still downgrades a refusal, as it does for every other code.
fn forge_ref_effect(f: &Finding, policy: &SecurityPolicy) -> Option<VerdictState> {
    Some(match (f.severity, policy.mode) {
        (Severity::Critical, SecurityMode::Block) => VerdictState::Denied,
        _ => VerdictState::Warned,
    })
}

/// What one finding does to the state under `policy`, or `None` when it is
/// recorded but changes nothing (a finding under the threshold, an ignored
/// scanner error).
fn classify(f: &Finding, policy: &SecurityPolicy) -> Option<VerdictState> {
    if f.code.is_always_denied() {
        return Some(VerdictState::Denied);
    }
    match f.code {
        ReasonCode::MinAgeNotMet | ReasonCode::ScanPending | ReasonCode::TimestampMissing => {
            Some(VerdictState::Quarantined)
        }
        ReasonCode::ScannerError | ReasonCode::ScannerUnsupported => scanner_error_effect(policy),
        ReasonCode::InstallHook => install_hook_effect(policy),
        ReasonCode::ProvenanceMissing | ReasonCode::ProvenanceInvalid => provenance_effect(policy),
        ReasonCode::ProvenanceUnverifiable => Some(VerdictState::Warned),
        ReasonCode::AdminOverride => None,
        ReasonCode::MutableRef
        | ReasonCode::TagMoved
        | ReasonCode::AssetReplaced
        | ReasonCode::PinnedRefRequired
        | ReasonCode::RawScript => forge_ref_effect(f, policy),
        _ => threshold(judged_of(f), policy),
    }
}

/// What folding the findings produced: the worst state any of them reached,
/// the codes in first-seen order, and — for a hold — when it lifts and which
/// codes are holding it.
struct Folded {
    state: VerdictState,
    codes: Vec<ReasonCode>,
    available_at: Option<DateTime<Utc>>,
    /// The codes that put the verdict into a hold, to decide whether the
    /// maturity bypass may lift it — each with whether the bypass may lift
    /// *that* finding: an advisory database's error on a version it never
    /// answered for may not (RFC 0041 decision 44).
    hold_codes: Vec<(ReasonCode, bool)>,
}

/// Whether the maturity bypass may lift this held finding.
fn bypassable(f: &Finding, scanners_done: &[String]) -> bool {
    f.code.is_maturity_bypassable()
        && !(f.kind == FindingKind::AdvisoryDbError && !scanners_done.contains(&f.scanner))
}

/// Fold every finding's effect into one state.
fn fold(findings: &[Finding], policy: &SecurityPolicy, scanners_done: &[String]) -> Folded {
    let mut folded = Folded {
        state: VerdictState::Allowed,
        codes: Vec::new(),
        available_at: None,
        hold_codes: Vec::new(),
    };
    for f in findings {
        let Some(effect) = classify(f, policy) else {
            continue;
        };
        if !folded.codes.contains(&f.code) {
            folded.codes.push(f.code);
        }
        if effect == VerdictState::Quarantined {
            folded
                .hold_codes
                .push((f.code, bypassable(f, scanners_done)));
            if let Some(at) = f.available_at {
                folded.available_at = Some(
                    folded
                        .available_at
                        .map_or(at, |cur: DateTime<Utc>| cur.max(at)),
                );
            }
        }
        folded.state = worse_state(folded.state, effect);
    }
    folded
}

/// The maturity bypass (RFC 0018 §4.2 *Precedence*): a hold whose only causes
/// are "nobody looked yet" lifts once the version is older than
/// `mature_age_secs`. Never for a finding, a block, or a version the upstream
/// did not date.
fn maturity_lifts(
    folded: &Folded,
    package: &PackageMetadata,
    policy: &SecurityPolicy,
    now: DateTime<Utc>,
) -> bool {
    if folded.state != VerdictState::Quarantined
        || folded.hold_codes.is_empty()
        || policy.mature_age.is_zero()
    {
        return false;
    }
    if !folded.hold_codes.iter().all(|(_, lifts)| *lifts) {
        return false;
    }
    let Some(published) = package.published_at else {
        return false;
    };
    let mature = chrono::Duration::from_std(policy.mature_age).unwrap_or_default();
    published + mature <= now
}

/// The pure evaluation (RFC 0018 §5.2): `(metadata, findings, policy, now) →
/// Verdict`.
///
/// `scanner_findings` are what scanners said — content findings, scanner
/// errors, a SOC verdict. Age and pending findings are derived here from
/// `package`, `policy` and `scanners_done`, so the same call judges time and
/// content and a held artifact's `available_at` is known without a scan.
pub fn evaluate(
    package: &PackageMetadata,
    scanner_findings: Vec<Finding>,
    scanners_done: Vec<String>,
    policy: &SecurityPolicy,
    now: DateTime<Utc>,
    last_scanned_at: Option<DateTime<Utc>>,
) -> Verdict {
    let mut findings = scanner_findings;
    findings.retain(|f| f.scanner != AGE_SCANNER && f.scanner != PENDING_SCANNER);
    judge(&mut findings, policy);
    escalate(&mut findings, policy);
    findings.extend(age_findings(package, policy, now));
    findings.extend(pending_findings(policy, &scanners_done));

    let mut folded = fold(&findings, policy, &scanners_done);

    // The codes stay when maturity lifts a hold, so the served-unscanned state
    // stays visible; only the state and the lift time move.
    if maturity_lifts(&folded, package, policy, now) {
        folded.state = VerdictState::Warned;
    }
    if folded.state != VerdictState::Quarantined {
        folded.available_at = None;
    }

    Verdict {
        // The version, not the file: every artifact of a release shares it.
        package: coordinate_key(&package.id),
        state: folded.state,
        reason_codes: folded.codes,
        findings,
        policy_ref: format!("{} {EVALUATOR_TAG}", policy.policy_ref),
        available_at: folded.available_at,
        evaluated_at: now,
        last_scanned_at,
        scanners_done,
    }
}

/// Fold an active `security_verdict` exemption into a verdict (RFC 0018
/// §4.2): `denied`/`quarantined` become `warned` with `ADMIN_OVERRIDE` — never
/// `allowed`, so the override stays visible in headers and audit.
pub fn apply_override(mut verdict: Verdict) -> Verdict {
    if verdict.state.is_served() {
        return verdict;
    }
    verdict.state = VerdictState::Warned;
    verdict.available_at = None;
    if !verdict.reason_codes.contains(&ReasonCode::AdminOverride) {
        verdict.reason_codes.push(ReasonCode::AdminOverride);
    }
    verdict
}

/// The read side of the pipeline: the stored verdict re-judged against the
/// clock, or a fresh `SCAN_PENDING` hold and a queued job.
pub struct VerdictService {
    pub verdicts: Arc<dyn VerdictRepository>,
    pub queue: Arc<dyn ScanQueue>,
}

impl VerdictService {
    pub fn new(verdicts: Arc<dyn VerdictRepository>, queue: Arc<dyn ScanQueue>) -> Self {
        Self { verdicts, queue }
    }

    /// The verdict `package` is under right now.
    ///
    /// **Fails closed**: a verdict store that cannot be read is an error, and
    /// the gate turns that into a refusal. A repository blip must not become
    /// an unscanned artifact served.
    pub async fn current(
        &self,
        package: &PackageMetadata,
        policy: &SecurityPolicy,
        now: DateTime<Utc>,
    ) -> Result<Verdict, CoreError> {
        let key = coordinate_key(&package.id);
        let stored = self.verdicts.get(&key).await?;
        let (scanner_findings, done, last_scanned) = match &stored {
            Some(v) => (
                v.findings.clone(),
                v.scanners_done.clone(),
                v.last_scanned_at,
            ),
            None => (Vec::new(), Vec::new(), None),
        };
        let fresh = evaluate(package, scanner_findings, done, policy, now, last_scanned);
        // RFC 0040 §6.1: a pullability answer reads the verdict the download
        // would get and writes nothing — no scan queued, no hold persisted,
        // no transition recorded (T2b). The same `evaluate` either way.
        if crate::services::pullability::evaluate_only() {
            return Ok(fresh);
        }

        if stored.is_none() {
            // First sight: a user is waiting, so the job is `FirstSeen`, and
            // the hold is persisted before anything else can read it as
            // absent.
            let created = self
                .queue
                .enqueue(&key, package.published_at, ScanTrigger::FirstSeen)
                .await?;
            metrics::counter!(
                "batlehub_verdicts_total",
                "registry" => key.registry.clone(),
                "state" => fresh.state.as_str(),
                "trigger" => ScanTrigger::FirstSeen.as_str(),
            )
            .increment(1);
            if created {
                tracing::info!(package = %key, "security: first sight, scan queued");
            }
            self.verdicts.upsert(&fresh).await?;
            return Ok(fresh);
        }

        let previous = stored.expect("checked above");
        if holds_unrecognised(&previous) {
            // Written by a newer release: refused here (always denied), and
            // left for the release that understands it (RFC 0041 decision 33).
            return Ok(fresh);
        }
        let changed = previous.state != fresh.state || previous.reason_codes != fresh.reason_codes;
        if changed
            && previous.policy_ref.contains(EVALUATOR_TAG)
            && substitution_only(package, &previous, policy, now)
        {
            // Already saved once by this evaluator; what differs is only the
            // on-read substitution, which every read re-applies (decision 45).
            return Ok(fresh);
        }
        if changed {
            metrics::counter!(
                "batlehub_verdict_transitions_total",
                "registry" => key.registry.clone(),
                "from" => previous.state.as_str(),
                "to" => fresh.state.as_str(),
            )
            .increment(1);
            tracing::info!(
                package = %key,
                from = %previous.state,
                to = %fresh.state,
                codes = ?fresh.reason_codes,
                "security: verdict transition on read"
            );
            // A write that fails leaves the stored row stale; the next read
            // re-derives the same answer, so nothing is lost but a metric.
            if let Err(e) = self.verdicts.upsert(&fresh).await {
                tracing::warn!(package = %key, error = %e, "security: could not record verdict transition");
            }
        }
        Ok(fresh)
    }

    /// Record what the worker found for `package`: scanner findings and the
    /// scanners that answered, judged now. Findings of kind `SocVerdict`
    /// already stored are kept — the SOC's word outlives a rescan.
    pub async fn record_scan(
        &self,
        package: &PackageMetadata,
        policy: &SecurityPolicy,
        mut findings: Vec<Finding>,
        mut done: Vec<String>,
        now: DateTime<Utc>,
    ) -> Result<(Option<VerdictState>, Verdict), CoreError> {
        let key = coordinate_key(&package.id);
        let stored = self.verdicts.get(&key).await?;
        if let Some(prev) = stored.as_ref().filter(|v| holds_unrecognised(v)) {
            // A newer release wrote this row, SOC findings and all: rewriting
            // it from what this release understands would drop what it does
            // not (RFC 0041 decision 33). The job closes; the row stands.
            tracing::warn!(package = %key, codes = ?prev.reason_codes, "security worker: verdict holds a reason code this release does not know; left untouched");
            return Ok((Some(prev.state), prev.clone()));
        }
        if let Some(prev) = &stored {
            // Whether the `flags` scanner actually answered *this* run. Read
            // before the carry-forward loop below, which appends to `done`.
            let flags_answered = done.iter().any(|s| s == crate::entities::FLAGS_SCANNER);
            // The `flags` scanner re-emits every live pushed flag on each
            // run (RFC 0002 §13), so its previous findings are not kept:
            // keeping them would outlive a revoke. Every other SOC finding
            // — an administrator's word — stays.
            //
            // That reasoning holds only when `flags` *ran*. `run_scanner` adds
            // a scanner to `done` on `Ok` alone, so a `flags` run that errored
            // — the store unreadable, or the job timed out — re-emits nothing,
            // and dropping its stored findings anyway erased a live SOC
            // `hard_block` while the marker inherited below still suppressed
            // `SCAN_PENDING`. What was left was a lone `SCANNER_ERROR`, which
            // is maturity-bypassable, so a denied version was downgraded to
            // `warned` and served. A scanner that did not answer revokes
            // nothing.
            findings.extend(
                prev.findings
                    .iter()
                    .filter(|f| {
                        f.kind == FindingKind::SocVerdict
                            && !(flags_answered && f.scanner == crate::entities::FLAGS_SCANNER)
                    })
                    .cloned(),
            );
            // A scanner whose previous answer is carried forward in
            // `scanners_done` has to have its previous *findings* carried with
            // it. Content findings are otherwise re-derived by the scanner that
            // ran, so nothing is lost when it did — but inheriting the marker
            // alone for a scanner that did *not* run erased what it had found
            // while still suppressing `SCAN_PENDING`. A rescan during an OSV
            // outage dropped a critical vulnerability finding and left only
            // `SCANNER_ERROR`, which is maturity-bypassable, so the version was
            // downgraded to `warned` and served.
            //
            // `flags` needs no exception here: a `flags` run that succeeded is
            // already in `done` and is skipped by the guard below, so anything
            // reaching this point did not run and has nothing to revoke with.
            for s in &prev.scanners_done {
                if done.contains(s) {
                    continue;
                }
                findings.extend(
                    prev.findings
                        .iter()
                        .filter(|f| &f.scanner == s && f.kind != FindingKind::SocVerdict)
                        .cloned(),
                );
                done.push(s.clone());
            }
        }
        let verdict = evaluate(package, findings, done, policy, now, Some(now));
        for f in &verdict.findings {
            metrics::counter!(
                "batlehub_findings_total",
                "registry" => key.registry.clone(),
                "scanner" => f.scanner.clone(),
                "kind" => f.kind.as_str(),
                "severity" => f.severity.as_str(),
            )
            .increment(1);
        }
        self.verdicts.upsert(&verdict).await?;
        let from = stored.map(|v| v.state);
        if from != Some(verdict.state) {
            metrics::counter!(
                "batlehub_verdict_transitions_total",
                "registry" => key.registry.clone(),
                "from" => from.map(|s| s.as_str()).unwrap_or("none"),
                "to" => verdict.state.as_str(),
            )
            .increment(1);
        }
        Ok((from, verdict))
    }
}

/// Whether `previous` differs from today's judgement only by this RFC's
/// on-read substitutions: judged without them, it would stand unchanged.
fn substitution_only(
    package: &PackageMetadata,
    previous: &Verdict,
    policy: &SecurityPolicy,
    now: DateTime<Utc>,
) -> bool {
    let plain_policy = SecurityPolicy {
        unknown_severity: Severity::Unknown,
        ..policy.clone()
    };
    // A stored `MAL-` finding read as `KNOWN_MALICIOUS` is the other
    // substitution: hide the citation from `judge` for the plain judgement.
    let findings = previous
        .findings
        .iter()
        .cloned()
        .map(|mut f| {
            if f.names_malicious() && f.code != ReasonCode::KnownMalicious {
                f.reference = None;
                f.raw = serde_json::Value::Null;
            }
            f
        })
        .collect();
    let plain = evaluate(
        package,
        findings,
        previous.scanners_done.clone(),
        &plain_policy,
        now,
        previous.last_scanned_at,
    );
    plain.state == previous.state && plain.reason_codes == previous.reason_codes
}

/// Whether a stored verdict carries a code this release does not know.
fn holds_unrecognised(v: &Verdict) -> bool {
    v.reason_codes.contains(&ReasonCode::Unrecognised)
        || v.findings
            .iter()
            .any(|f| f.code == ReasonCode::Unrecognised)
}

tokio::task_local! {
    /// The verdict the current request was judged under, left by
    /// `VerdictGateRule` for the response to carry (RFC 0018 §4.2: the
    /// `X-BatleHub-*` headers, `Retry-After`, the native body). A task-local
    /// rather than a field on `RuleDecision`: the sixty sites that build or
    /// match a `Deny` stay as they are, and a rule that is not the gate never
    /// touches it.
    static REQUEST_VERDICT: std::cell::RefCell<Option<Verdict>>;
}

/// Leave `verdict` for the enclosing [`with_request_verdict`], if any. A
/// no-op outside one — the gate is also run by the local read path and the
/// explain oracle, which want only the decision.
pub fn note_request_verdict(verdict: &Verdict) {
    let _ = REQUEST_VERDICT.try_with(|slot| *slot.borrow_mut() = Some(verdict.clone()));
}

/// Add findings the *request* produced to the verdict it carries (RFC 0019
/// phase 2).
///
/// The forge-ref facts — a branch followed, a tag that moved — are about how
/// the client addressed the bytes, not about the bytes: the same commit
/// reached through a tag and through a branch is one stored verdict and two
/// ref kinds. So they are merged into the per-request verdict here and never
/// written to the verdict store, and the worker (which sees a coordinate, not
/// a request) never produces them.
///
/// With a verdict already noted — a `[security]` registry, where
/// `VerdictGateRule` ran first — the findings and their codes are added to
/// it and a served state falls to `warned`, or to `denied` when the ref
/// policy refused. With none noted, one is built only when `create` is set:
/// on a forge registry without `[security]` a refusal is a plain `403` and
/// there is no verdict to invent (RFC 0019 §6.1's honest degradation).
pub fn augment_request_verdict(
    package: &PackageId,
    policy_ref: &str,
    findings: Vec<Finding>,
    denied: bool,
    create: bool,
    now: DateTime<Utc>,
) {
    if findings.is_empty() {
        return;
    }
    let _ = REQUEST_VERDICT.try_with(|slot| {
        let mut slot = slot.borrow_mut();
        let base = match slot.take() {
            Some(v) => Some(v),
            None if create => Some(Verdict {
                package: package.clone(),
                state: VerdictState::Allowed,
                reason_codes: Vec::new(),
                findings: Vec::new(),
                policy_ref: policy_ref.to_owned(),
                available_at: None,
                evaluated_at: now,
                last_scanned_at: None,
                scanners_done: Vec::new(),
            }),
            None => None,
        };
        let Some(mut verdict) = base else { return };
        for f in findings {
            if !verdict.reason_codes.contains(&f.code) {
                verdict.reason_codes.push(f.code);
            }
            verdict.findings.push(f);
        }
        verdict.state = match (denied, verdict.state) {
            (true, _) => VerdictState::Denied,
            // A hold stays a hold: a mutable ref does not release a
            // quarantined version, it is one more thing to say about it.
            (false, VerdictState::Allowed) => VerdictState::Warned,
            (false, other) => other,
        };
        *slot = Some(verdict);
    });
}

/// Run `f` with a slot for the gate to leave its verdict in, and return both.
pub async fn with_request_verdict<F: std::future::Future>(f: F) -> (F::Output, Option<Verdict>) {
    REQUEST_VERDICT
        .scope(std::cell::RefCell::new(None), async move {
            let out = f.await;
            let verdict = REQUEST_VERDICT.with(|slot| slot.borrow_mut().take());
            (out, verdict)
        })
        .await
}

impl VerdictService {
    /// A version this instance just published into a `[security]` registry
    /// (RFC 0018 §4.2 *Local publish*, phase 2): stored, enqueued as
    /// `FirstSeen`, and `quarantined(SCAN_PENDING)` — hidden from listings —
    /// until the worker answers. Dated `now`: the publisher is authenticated,
    /// so there is no upstream date to wait on, and the read path re-judges
    /// the age against the registry's own profile from here.
    pub async fn first_sight_local(
        &self,
        package: &PackageId,
        policy: &SecurityPolicy,
        now: DateTime<Utc>,
    ) -> Result<Verdict, CoreError> {
        let key = coordinate_key(package);
        let mut meta = PackageMetadata::minimal(key.clone(), serde_json::Value::Null);
        meta.published_at = Some(now);
        let created = self
            .queue
            .enqueue(&key, meta.published_at, ScanTrigger::FirstSeen)
            .await?;
        if created {
            tracing::info!(package = %key, "security: local publish, scan queued");
        }
        let verdict = evaluate(&meta, Vec::new(), Vec::new(), policy, now, None);
        self.verdicts.upsert(&verdict).await?;
        metrics::counter!(
            "batlehub_verdicts_total",
            "registry" => key.registry.clone(),
            "state" => verdict.state.as_str(),
            "trigger" => ScanTrigger::FirstSeen.as_str(),
        )
        .increment(1);
        Ok(verdict)
    }
}

/// A verdict is about a *version*, not a file of it: the sub-coordinate
/// (`tarball`, a classifier, a platform file) is dropped, so every file of a
/// release shares one verdict and one scan.
pub fn coordinate_key(id: &PackageId) -> PackageId {
    PackageId::new(&id.registry, &id.name, &id.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{Escalation, PackageId, PackageMetadata};
    use std::time::Duration;

    fn meta(published_ago_secs: Option<i64>, now: DateTime<Utc>) -> PackageMetadata {
        PackageMetadata::minimal(
            PackageId::new("npm-public", "left-pad", "1.3.1"),
            serde_json::Value::Null,
        )
        .with_published(published_ago_secs.map(|s| now - chrono::Duration::seconds(s)))
    }

    trait WithPublished {
        fn with_published(self, at: Option<DateTime<Utc>>) -> Self;
    }
    impl WithPublished for PackageMetadata {
        fn with_published(mut self, at: Option<DateTime<Utc>>) -> Self {
            self.published_at = at;
            self
        }
    }

    fn policy() -> SecurityPolicy {
        let mut p = SecurityPolicy::defaults_for("npm-public");
        p.min_age = Duration::from_secs(3600);
        p.mature_age = Duration::from_secs(86_400);
        p
    }

    fn vuln(severity: Severity) -> Finding {
        Finding::new(
            "osv",
            FindingKind::Vulnerability,
            ReasonCode::Vulnerability,
            severity,
            "GHSA-xxxx",
        )
        .with_reference("GHSA-xxxx")
    }

    fn done() -> Vec<String> {
        vec!["osv".into()]
    }

    #[test]
    fn a_young_version_is_held_by_age_with_an_available_at() {
        let now = Utc::now();
        let v = evaluate(&meta(Some(600), now), vec![], done(), &policy(), now, None);
        assert_eq!(v.state, VerdictState::Quarantined);
        assert_eq!(v.reason_codes, vec![ReasonCode::MinAgeNotMet]);
        let at = v.available_at.unwrap();
        assert_eq!((at - now).num_seconds(), 3000);
        assert_eq!(v.retry_after_secs(now), Some(3000));
    }

    #[test]
    fn an_unscanned_version_is_pending_not_allowed() {
        let now = Utc::now();
        let v = evaluate(&meta(Some(7200), now), vec![], vec![], &policy(), now, None);
        assert_eq!(v.state, VerdictState::Quarantined);
        assert_eq!(v.reason_codes, vec![ReasonCode::ScanPending]);
        assert!(
            v.available_at.is_none(),
            "pending carries no clock of its own"
        );
    }

    #[test]
    fn a_scanned_clean_mature_version_is_allowed() {
        let now = Utc::now();
        let v = evaluate(&meta(Some(7200), now), vec![], done(), &policy(), now, None);
        assert_eq!(v.state, VerdictState::Allowed);
        assert!(v.reason_codes.is_empty());
    }

    #[test]
    fn a_finding_at_the_threshold_denies_in_block_mode_and_warns_in_warn_mode() {
        let now = Utc::now();
        let m = meta(Some(7200), now);
        let v = evaluate(&m, vec![vuln(Severity::High)], done(), &policy(), now, None);
        assert_eq!(v.state, VerdictState::Denied);
        assert_eq!(v.reason_codes, vec![ReasonCode::Vulnerability]);

        let mut warn = policy();
        warn.mode = SecurityMode::Warn;
        let v = evaluate(&m, vec![vuln(Severity::High)], done(), &warn, now, None);
        assert_eq!(v.state, VerdictState::Warned);

        let v = evaluate(&m, vec![vuln(Severity::Low)], done(), &policy(), now, None);
        assert_eq!(v.state, VerdictState::Allowed, "under the threshold");
        assert!(v.reason_codes.is_empty());
        assert_eq!(v.findings.len(), 1, "but recorded");
    }

    #[test]
    fn precedence_is_denied_over_quarantined_over_warned() {
        let now = Utc::now();
        let young = meta(Some(60), now);
        let block = Finding::new(
            "block_list",
            FindingKind::BlockList,
            ReasonCode::BlockList,
            Severity::Critical,
            "blocked by admin",
        );
        let v = evaluate(&young, vec![block], vec![], &policy(), now, None);
        assert_eq!(v.state, VerdictState::Denied, "a block beats every hold");
        assert!(v.reason_codes.contains(&ReasonCode::BlockList));
        assert!(v.reason_codes.contains(&ReasonCode::MinAgeNotMet));
        assert!(v.available_at.is_none(), "a denial carries no clock");
        assert_eq!(v.retry_after_secs(now), None);

        let mut warn = policy();
        warn.mode = SecurityMode::Warn;
        let v = evaluate(
            &young,
            vec![vuln(Severity::Critical)],
            done(),
            &warn,
            now,
            None,
        );
        assert_eq!(
            v.state,
            VerdictState::Quarantined,
            "age holds even in warn mode"
        );
    }

    #[test]
    fn a_missing_timestamp_holds_open_ended_by_default_and_is_skipped_when_told() {
        let now = Utc::now();
        let undated = meta(None, now);
        let v = evaluate(&undated, vec![], done(), &policy(), now, None);
        assert_eq!(v.state, VerdictState::Quarantined);
        assert_eq!(v.reason_codes, vec![ReasonCode::TimestampMissing]);
        assert!(v.available_at.is_none());
        assert_eq!(v.retry_after_secs(now), None, "waiting cannot help");

        let mut p = policy();
        p.hold_missing_timestamp = false;
        let v = evaluate(&undated, vec![], done(), &p, now, None);
        assert_eq!(v.state, VerdictState::Allowed);
    }

    #[test]
    fn the_maturity_bypass_serves_an_old_unscanned_version_as_warned_and_nothing_else() {
        let now = Utc::now();
        let old = meta(Some(200_000), now);
        let v = evaluate(&old, vec![], vec![], &policy(), now, None);
        assert_eq!(v.state, VerdictState::Warned);
        assert_eq!(
            v.reason_codes,
            vec![ReasonCode::ScanPending],
            "the code stays visible"
        );

        // A scanner error follows the same threshold.
        let err = Finding::new(
            "osv",
            FindingKind::ScannerError,
            ReasonCode::ScannerError,
            Severity::High,
            "timed out",
        );
        let v = evaluate(&old, vec![err.clone()], vec![], &policy(), now, None);
        assert_eq!(v.state, VerdictState::Warned);

        // Below the maturity age the same holds.
        let v = evaluate(
            &meta(Some(7200), now),
            vec![err],
            vec![],
            &policy(),
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Quarantined);

        // A real finding is never downgraded.
        let v = evaluate(
            &old,
            vec![vuln(Severity::High)],
            vec![],
            &policy(),
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Denied);

        // `mature_age = 0` means never serve unscanned.
        let mut never = policy();
        never.mature_age = Duration::ZERO;
        let v = evaluate(&old, vec![], vec![], &never, now, None);
        assert_eq!(v.state, VerdictState::Quarantined);

        // An undated version cannot mature: the bypass is computed from a date.
        let v = evaluate(&meta(None, now), vec![], vec![], &policy(), now, None);
        assert_eq!(v.state, VerdictState::Quarantined);
    }

    #[test]
    fn scanner_error_modes() {
        let now = Utc::now();
        let m = meta(Some(7200), now);
        let err = || {
            Finding::new(
                "osv",
                FindingKind::ScannerError,
                ReasonCode::ScannerError,
                Severity::High,
                "crashed",
            )
        };
        for (mode, expected) in [
            (ScannerErrorMode::Quarantine, VerdictState::Quarantined),
            (ScannerErrorMode::Warn, VerdictState::Warned),
            (ScannerErrorMode::Ignore, VerdictState::Allowed),
        ] {
            let mut p = policy();
            p.scanner_error = mode;
            let v = evaluate(&m, vec![err()], done(), &p, now, None);
            assert_eq!(v.state, expected, "{mode:?}");
        }
    }

    #[test]
    fn an_override_flips_a_hold_to_warned_and_never_to_allowed() {
        let now = Utc::now();
        let held = evaluate(&meta(Some(60), now), vec![], vec![], &policy(), now, None);
        let over = apply_override(held);
        assert_eq!(over.state, VerdictState::Warned);
        assert!(over.reason_codes.contains(&ReasonCode::AdminOverride));
        assert!(over.reason_codes.contains(&ReasonCode::MinAgeNotMet));
        let clean = evaluate(&meta(Some(7200), now), vec![], done(), &policy(), now, None);
        assert_eq!(
            apply_override(clean).state,
            VerdictState::Allowed,
            "nothing to override"
        );
    }

    #[test]
    fn escalation_combines_transitions_from_one_scanner_only() {
        let now = Utc::now();
        let m = meta(Some(7200), now);
        let t = |scanner: &str, code: ReasonCode| {
            Finding::new(
                scanner,
                FindingKind::Transition,
                code,
                Severity::Medium,
                "moved",
            )
        };
        let mut p = policy();
        p.escalation.insert(
            "postmortem".into(),
            Escalation {
                kinds: vec![FindingKind::Transition],
                count: 2,
                from: Severity::Medium,
                to: Severity::High,
            },
        );
        // Two from postmortem: raised to high → denied.
        let v = evaluate(
            &m,
            vec![
                t("postmortem", ReasonCode::PublisherChanged),
                t("postmortem", ReasonCode::InstallHookAdded),
            ],
            done(),
            &p,
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Denied);
        // One from each of two scanners: never combined.
        let v = evaluate(
            &m,
            vec![
                t("postmortem", ReasonCode::PublisherChanged),
                t("guarddog", ReasonCode::InstallHookAdded),
            ],
            done(),
            &p,
            now,
            None,
        );
        assert_eq!(
            v.state,
            VerdictState::Allowed,
            "medium is under the high threshold"
        );
    }

    #[test]
    fn a_verdict_is_about_the_version_not_the_file() {
        let id = PackageId::new("npm", "left-pad", "1.3.1").with_artifact("tarball");
        assert_eq!(
            coordinate_key(&id),
            PackageId::new("npm", "left-pad", "1.3.1")
        );
    }

    // ── RFC 0041 §4.5: unrated findings, MAL-, judged_as ─────────────────────

    fn unrated(reference: &str) -> Finding {
        Finding::new(
            "osv",
            FindingKind::Vulnerability,
            ReasonCode::Vulnerability,
            Severity::Unknown,
            reference,
        )
        .with_reference(reference)
    }

    fn mature(now: DateTime<Utc>) -> PackageMetadata {
        meta(Some(30 * 86_400), now)
    }

    #[test]
    fn an_unrated_finding_is_judged_high_by_default_and_keeps_its_stored_severity() {
        let now = Utc::now();
        let v = evaluate(
            &mature(now),
            vec![unrated("GO-2024-1")],
            done(),
            &policy(),
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Denied);
        assert_eq!(v.reason_codes, vec![ReasonCode::Vulnerability]);
        assert_eq!(v.findings[0].severity, Severity::Unknown);
        assert_eq!(v.findings[0].judged_as, Some(Severity::High));
        assert!(
            !v.findings[0].summary.contains("judged"),
            "never in the summary"
        );
    }

    #[test]
    fn unknown_severity_unknown_restores_the_old_behaviour() {
        let now = Utc::now();
        let mut p = policy();
        p.unknown_severity = Severity::Unknown;
        let v = evaluate(
            &mature(now),
            vec![unrated("GO-2024-1")],
            done(),
            &p,
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Allowed);
        assert_eq!(v.findings[0].judged_as, None);
    }

    #[test]
    fn an_unrated_finding_warns_under_mode_warn() {
        let now = Utc::now();
        let mut p = policy();
        p.mode = SecurityMode::Warn;
        let v = evaluate(
            &mature(now),
            vec![unrated("GO-2024-1")],
            done(),
            &p,
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Warned);
    }

    #[test]
    fn a_stated_severity_is_never_substituted() {
        let now = Utc::now();
        let v = evaluate(
            &mature(now),
            vec![vuln(Severity::Low)],
            done(),
            &policy(),
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Allowed);
        assert_eq!(v.findings[0].judged_as, None);
    }

    #[test]
    fn a_stale_judged_as_is_cleared_when_the_setting_changes() {
        let now = Utc::now();
        let first = evaluate(
            &mature(now),
            vec![unrated("GO-2024-1")],
            done(),
            &policy(),
            now,
            None,
        );
        assert_eq!(first.findings[0].judged_as, Some(Severity::High));
        let mut p = policy();
        p.unknown_severity = Severity::Unknown;
        let again = evaluate(&mature(now), first.findings, done(), &p, now, None);
        assert_eq!(again.state, VerdictState::Allowed);
        assert_eq!(again.findings[0].judged_as, None);
    }

    #[test]
    fn forge_ref_codes_are_not_judged() {
        let now = Utc::now();
        let f = Finding::new(
            "forge_refs",
            FindingKind::Ref,
            ReasonCode::MutableRef,
            Severity::Unknown,
            "a branch",
        );
        let v = evaluate(&mature(now), vec![f], done(), &policy(), now, None);
        assert_eq!(v.findings[0].judged_as, None);
        assert_eq!(v.state, VerdictState::Warned);
    }

    #[test]
    fn an_unrated_finding_counts_toward_an_escalation_at_its_judged_level() {
        let now = Utc::now();
        let mut p = policy();
        p.max_severity = Severity::Critical;
        p.escalation.insert(
            "osv".into(),
            Escalation {
                kinds: vec![FindingKind::Vulnerability],
                count: 2,
                from: Severity::High,
                to: Severity::Critical,
            },
        );
        let v = evaluate(
            &mature(now),
            vec![unrated("GO-2024-1"), unrated("GO-2024-2")],
            done(),
            &p,
            now,
            None,
        );
        assert_eq!(v.state, VerdictState::Denied);
        assert!(v
            .findings
            .iter()
            .all(|f| f.severity == Severity::Critical && f.judged_as.is_none()));
    }

    #[test]
    fn a_stored_mal_finding_is_known_malicious_on_read_under_warn_and_critical() {
        let now = Utc::now();
        for (mode, max) in [
            (SecurityMode::Warn, Severity::High),
            (SecurityMode::Block, Severity::Critical),
        ] {
            let mut p = policy();
            p.mode = mode;
            p.max_severity = max;
            // The shape M1 stored: VULNERABILITY / unknown / MAL-2023-1000.
            let v = evaluate(
                &mature(now),
                vec![unrated("MAL-2023-1000")],
                done(),
                &p,
                now,
                None,
            );
            assert_eq!(v.state, VerdictState::Denied, "{mode:?} {max:?}");
            assert_eq!(v.reason_codes, vec![ReasonCode::KnownMalicious]);
        }
    }

    #[test]
    fn a_mal_alias_is_known_malicious_too() {
        let now = Utc::now();
        let f = vuln(Severity::Low)
            .with_raw(serde_json::json!({ "id": "GHSA-x", "aliases": ["MAL-2022-1"] }));
        let v = evaluate(&mature(now), vec![f], done(), &policy(), now, None);
        assert_eq!(v.reason_codes, vec![ReasonCode::KnownMalicious]);
    }

    #[test]
    fn an_exemption_lifts_known_malicious_to_warned_with_admin_override() {
        let now = Utc::now();
        let v = evaluate(
            &mature(now),
            vec![unrated("MAL-2023-1000")],
            done(),
            &policy(),
            now,
            None,
        );
        let v = apply_override(v);
        assert_eq!(v.state, VerdictState::Warned);
        assert!(v.reason_codes.contains(&ReasonCode::AdminOverride));
    }

    #[test]
    fn a_verdict_carries_the_evaluator_tag() {
        let now = Utc::now();
        let v = evaluate(&mature(now), vec![], done(), &policy(), now, None);
        assert_eq!(v.policy_ref, format!("npm-public/default {EVALUATOR_TAG}"));
    }

    /// `Severity::judged` is the one substitution (RFC 0041 §5.4): called by
    /// `evaluate`, `FlagsRule` and `CveGateRule`, and nowhere else.
    #[test]
    fn severity_judged_is_called_at_the_three_sites_only() {
        fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir)
                .expect("readable source tree")
                .flatten()
            {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&path).expect("utf-8 source");
                    // Code before the file's test module only.
                    let code = text.split("#[cfg(test)]").next().unwrap_or_default();
                    if code.contains(".judged(") {
                        out.push(path.display().to_string());
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/core has a parent");
        let mut found = Vec::new();
        for krate in ["core", "adapters", "web", "config"] {
            walk(&root.join(krate).join("src"), &mut found);
        }
        let mut found: Vec<String> = found
            .iter()
            .map(|p| p.rsplit("crates/").next().unwrap_or(p).to_owned())
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                "core/src/rules/cve_gate.rs",
                "core/src/rules/flags.rs",
                "core/src/services/verdict.rs",
            ]
        );
    }
}

/// `record_scan`'s carry-forward, which decides what a rescan is allowed to
/// forget.
///
/// The `flags` scanner re-emits every live flag on each run, so a run that
/// succeeded revokes what it no longer emits. A run that *errored* emits
/// nothing and must revoke nothing. The two used to be indistinguishable here,
/// because the scanner's "done" marker was inherited without its findings, and
/// these tests hold both halves of the distinction in place.
#[cfg(test)]
mod record_scan_tests {
    use super::*;
    use crate::entities::{PackageId, PackageMetadata, ScanJob, ScanTrigger};
    use crate::ports::{QueuedCount, ScanQueue, VerdictRepository};
    use std::sync::Mutex;
    use std::time::Duration;

    /// One verdict, in memory. Only `upsert`/`get` are exercised; the listing
    /// methods belong to the read side and are not on this path.
    #[derive(Default)]
    struct OneVerdict(Mutex<Option<Verdict>>, std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl VerdictRepository for OneVerdict {
        async fn upsert(&self, verdict: &Verdict) -> Result<(), CoreError> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.0.lock().expect("test mutex") = Some(verdict.clone());
            Ok(())
        }
        async fn get(&self, _package: &PackageId) -> Result<Option<Verdict>, CoreError> {
            Ok(self.0.lock().expect("test mutex").clone())
        }
        async fn list_for_package(
            &self,
            _registry: &str,
            _package: &str,
        ) -> Result<Vec<Verdict>, CoreError> {
            Ok(vec![])
        }
        async fn list_by_state(
            &self,
            _registry: &str,
            _state: VerdictState,
            _limit: u64,
        ) -> Result<Vec<Verdict>, CoreError> {
            Ok(vec![])
        }
        async fn list_due_for_rescan(
            &self,
            _registry: &str,
            _before: DateTime<Utc>,
            _limit: u64,
        ) -> Result<Vec<PackageId>, CoreError> {
            Ok(vec![])
        }
    }

    #[derive(Default)]
    struct NoQueue;

    #[async_trait::async_trait]
    impl ScanQueue for NoQueue {
        async fn enqueue(
            &self,
            _package: &PackageId,
            _published_at: Option<DateTime<Utc>>,
            _trigger: ScanTrigger,
        ) -> Result<bool, CoreError> {
            Ok(false)
        }
        async fn lease(
            &self,
            _worker_id: &str,
            _registries: &[String],
            _n: u32,
            _lease_secs: u64,
            _max_attempts: u32,
        ) -> Result<Vec<ScanJob>, CoreError> {
            Ok(vec![])
        }
        async fn heartbeat(&self, _job_id: uuid::Uuid, _lease_secs: u64) -> Result<(), CoreError> {
            Ok(())
        }
        async fn complete(&self, _job_id: uuid::Uuid, _: Option<&str>) -> Result<(), CoreError> {
            Ok(())
        }
        async fn fail(&self, _job_id: uuid::Uuid, _: &str, _error: &str) -> Result<(), CoreError> {
            Ok(())
        }
        async fn exhausted(&self, _max: u32, _n: u32) -> Result<Vec<ScanJob>, CoreError> {
            Ok(vec![])
        }
        async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError> {
            Ok(vec![])
        }
        async fn try_lead(&self, _key: i64) -> Result<bool, CoreError> {
            Ok(true)
        }
    }

    fn svc() -> VerdictService {
        VerdictService::new(Arc::new(OneVerdict::default()), Arc::new(NoQueue))
    }

    /// A version published long enough ago that `mature_age` would lift a
    /// bypassable hold — which is what made the erasure serve bytes rather than
    /// merely lose a finding.
    fn mature_package(now: DateTime<Utc>) -> PackageMetadata {
        let mut m = PackageMetadata::minimal(
            PackageId::new("npm-public", "evil-pkg", "1.0.0"),
            serde_json::Value::Null,
        );
        m.published_at = Some(now - chrono::Duration::days(30));
        m
    }

    fn policy() -> SecurityPolicy {
        let mut p = SecurityPolicy::defaults_for("npm-public");
        p.min_age = Duration::from_secs(0);
        p.mature_age = Duration::from_secs(86_400);
        p
    }

    /// What a SOC `hard_block` push stores.
    fn soc_hard_block() -> Finding {
        Finding::new(
            crate::entities::FLAGS_SCANNER,
            FindingKind::SocVerdict,
            ReasonCode::SocVerdict,
            Severity::Critical,
            "malware confirmed",
        )
    }

    fn scanner_error() -> Finding {
        Finding::new(
            crate::entities::FLAGS_SCANNER,
            FindingKind::ScannerError,
            ReasonCode::ScannerError,
            Severity::High,
            "flag store unreadable: connection timed out",
        )
    }

    /// The regression: a rescan in which the flags scanner *errored* must not
    /// drop the block it could not re-read.
    #[tokio::test]
    async fn a_flags_error_does_not_erase_a_hard_block() {
        let now = Utc::now();
        let s = svc();
        let pkg = mature_package(now);

        // The push: denied, on the SOC's word.
        let (_, first) = s
            .record_scan(
                &pkg,
                &policy(),
                vec![soc_hard_block()],
                vec![crate::entities::FLAGS_SCANNER.into(), "osv".into()],
                now,
            )
            .await
            .expect("record the push");
        assert_eq!(first.state, VerdictState::Denied);

        // The rescan it enqueues, with the flag store unreadable. `run_scanner`
        // adds a scanner to `done` only on success, so `flags` is absent here.
        let (_, after) = s
            .record_scan(
                &pkg,
                &policy(),
                vec![scanner_error()],
                vec!["osv".into()],
                now,
            )
            .await
            .expect("record the failed rescan");

        assert_eq!(
            after.state,
            VerdictState::Denied,
            "a flags outage must not downgrade a hard block; got {:?} with {:?}",
            after.state,
            after.reason_codes
        );
        assert!(
            !after.state.is_served(),
            "the artifact must not be served while the block stands"
        );
        assert!(
            after
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::SocVerdict),
            "the SOC finding itself must survive the outage"
        );
    }

    /// The other half: a flags run that *succeeded* and emitted nothing is a
    /// revoke, and must still clear the block. Without this the fix above would
    /// make a hard block permanent.
    #[tokio::test]
    async fn a_successful_flags_run_still_revokes() {
        let now = Utc::now();
        let s = svc();
        let pkg = mature_package(now);

        s.record_scan(
            &pkg,
            &policy(),
            vec![soc_hard_block()],
            vec![crate::entities::FLAGS_SCANNER.into(), "osv".into()],
            now,
        )
        .await
        .expect("record the push");

        // The revoke: flags ran, found nothing live, and says so by being in
        // `done` with no findings.
        let (_, after) = s
            .record_scan(
                &pkg,
                &policy(),
                vec![],
                vec![crate::entities::FLAGS_SCANNER.into(), "osv".into()],
                now,
            )
            .await
            .expect("record the revoke");

        assert!(
            after.state.is_served(),
            "a revoke must lift the block; got {:?} with {:?}",
            after.state,
            after.reason_codes
        );
        assert!(
            !after
                .findings
                .iter()
                .any(|f| f.kind == FindingKind::SocVerdict),
            "the revoked finding must not survive a successful run"
        );
    }

    // ── RFC 0041 decision 45: a substitution is saved once per evaluator ─────

    fn unrated_mal() -> Finding {
        Finding::new(
            "osv",
            FindingKind::Vulnerability,
            ReasonCode::Vulnerability,
            Severity::Unknown,
            "MAL-2023-1000",
        )
        .with_reference("MAL-2023-1000")
    }

    #[tokio::test]
    async fn an_on_read_substitution_is_saved_once_with_the_evaluator_tag() {
        let now = Utc::now();
        let pkg = mature_package(now);
        let mut p = SecurityPolicy::defaults_for("npm-public");
        p.min_age = Duration::from_secs(0);
        // What an older release stored: allowed, no tag.
        let old = Verdict {
            package: pkg.id.clone(),
            state: VerdictState::Allowed,
            reason_codes: vec![],
            findings: vec![unrated_mal()],
            policy_ref: "npm-public/default".into(),
            available_at: None,
            evaluated_at: now,
            last_scanned_at: Some(now),
            scanners_done: vec!["osv".into()],
        };
        let repo = Arc::new(OneVerdict::default());
        repo.upsert(&old).await.unwrap();
        let svc = VerdictService::new(repo.clone(), Arc::new(NoQueue));
        let saves = || repo.1.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(saves(), 1);

        let v = svc.current(&pkg, &p, now).await.unwrap();
        assert_eq!(v.state, VerdictState::Denied);
        assert_eq!(saves(), 2, "saved once");
        let stored = repo.get(&pkg.id).await.unwrap().unwrap();
        assert_eq!(stored.state, VerdictState::Denied, "listings see it now");
        assert!(stored.policy_ref.ends_with(EVALUATOR_TAG));

        svc.current(&pkg, &p, now).await.unwrap();
        assert_eq!(saves(), 2, "not re-saved on the next read");
    }

    #[tokio::test]
    async fn a_tagged_row_is_not_resaved_for_a_substitution_alone() {
        let now = Utc::now();
        let pkg = mature_package(now);
        let mut p = SecurityPolicy::defaults_for("npm-public");
        p.min_age = Duration::from_secs(0);
        let unrated = Finding::new(
            "osv",
            FindingKind::Vulnerability,
            ReasonCode::Vulnerability,
            Severity::Unknown,
            "GO-2024-1",
        );
        // Tagged by this evaluator, but stored allowed (as an older release
        // rewrote it during the rollout): the difference is the substitution.
        let row = Verdict {
            package: pkg.id.clone(),
            state: VerdictState::Allowed,
            reason_codes: vec![],
            findings: vec![unrated],
            policy_ref: format!("npm-public/default {EVALUATOR_TAG}"),
            available_at: None,
            evaluated_at: now,
            last_scanned_at: Some(now),
            scanners_done: vec!["osv".into()],
        };
        let repo = Arc::new(OneVerdict::default());
        repo.upsert(&row).await.unwrap();
        let svc = VerdictService::new(repo.clone(), Arc::new(NoQueue));
        let v = svc.current(&pkg, &p, now).await.unwrap();
        assert_eq!(v.state, VerdictState::Denied, "judged on every read");
        assert_eq!(
            repo.1.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "not saved again"
        );
    }
}
