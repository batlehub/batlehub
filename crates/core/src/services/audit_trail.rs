//! The audit trail's lifecycle (RFC 0036 §5.2–5.3, §6.3–6.4): sealing closed
//! windows into a signed chain, pseudonymising and expiring rows by class,
//! erasing a data subject, exporting one, and verifying the whole.
//!
//! Every change to a row in a sealed window is a chain record — `amend` for a
//! rewrite, `expire` for a deletion — committed in the same transaction as the
//! change, so the lifecycle can change sealed rows and nothing else can
//! without leaving a window whose digest matches no signed record.
//!
//! The store guards the chain's head: every batch names the head it was built
//! on, and the store refuses it if another writer moved the chain in between
//! ([`CoreError::Conflict`]). The callers here retry from a fresh read. That is
//! what lets an erasure on any process and the sealer on the leader run at the
//! same time without one sealing a window the other is rewriting.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, TimeZone, Utc};
use hmac::{KeyInit, Mac};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::entities::audit_seal::{record_digest, rows_digest, GENESIS_DIGEST};
use crate::entities::{
    AccessAction, AccessEvent, AccessResult, CallerNet, Identity, Role, SealKind, SealRecord,
};
use crate::error::CoreError;
use crate::ports::{
    AuditTrailStore, CandidateQuery, LeaderLock, PackageRepository, RowOp, TrailBatch,
};
use crate::services::audit_stream;
use crate::services::signature::VsxSigningKey;

/// Rows taken per store read. RFC 0036 §6.3's batch size.
pub const BATCH: u64 = 5_000;
/// A lifecycle tick stops after this many rows, to come back on the next.
const TICK_BUDGET: u64 = 50_000;
/// Windows sealed per tick at most: a process back after a long outage
/// catches up over a few ticks instead of one long one.
const SEALS_PER_TICK: usize = 100;
/// Attempts at a batch whose chain moved underneath it.
const CONFLICT_RETRIES: usize = 5;
/// How often the lifecycle job looks.
pub const LIFECYCLE_TICK: Duration = Duration::from_secs(3600);
/// The advisory-lock key the sealer and the lifecycle are led under: one
/// process seals and expires, whatever the number of workers.
pub const AUDIT_LEADER_KEY: i64 = 0x0036_a0d1_7000;

/// `[audit]`, resolved: what the service enforces.
#[derive(Clone, Default)]
pub struct AuditPolicy {
    pub access_retention: Option<chrono::Duration>,
    pub security_retention: Option<chrono::Duration>,
    pub pseudonymise_after: Option<chrono::Duration>,
    /// Window length and the key that signs; sealing is off without both.
    pub sealing: Option<(Duration, VsxSigningKey)>,
    pub erasure_key: Option<String>,
}

impl AuditPolicy {
    /// From `[audit]`'s day counts, `0` meaning never.
    pub fn from_days(
        access_retention_days: u32,
        security_retention_days: u32,
        pseudonymise_after_days: u32,
    ) -> Self {
        let days = |d: u32| (d > 0).then(|| chrono::Duration::days(d.into()));
        Self {
            access_retention: days(access_retention_days),
            security_retention: days(security_retention_days),
            pseudonymise_after: days(pseudonymise_after_days),
            ..Self::default()
        }
    }
}

/// What a lifecycle tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleReport {
    pub leader: bool,
    pub pseudonymised: u64,
    pub expired: u64,
}

/// What an erasure did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EraseReport {
    /// What the subject is called from now on, in every row it touched.
    pub pseudonym: String,
    pub audit_rows: u64,
    pub tokens: u64,
    pub blocks: u64,
}

/// One problem `verify` found.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VerifyFailure {
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
    pub seq: Option<i64>,
    pub reason: String,
}

/// What `verify` found (RFC 0036 §6.4).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VerifyReport {
    pub ok: bool,
    pub records: u64,
    pub windows_checked: u64,
    /// Windows whose rows retention has removed entirely.
    pub windows_expired: u64,
    /// The newest record's digest — what `--head` compares against.
    pub head: Option<String>,
    /// Whether `head` was compared with a digest the SIEM holds. Without it a
    /// truncated tail verifies (RFC 0036 §5.3).
    pub truncation_checked: bool,
    pub failures: Vec<VerifyFailure>,
}

pub struct AuditTrailService {
    store: Arc<dyn AuditTrailStore>,
    events: Arc<dyn PackageRepository>,
    leader: Arc<dyn LeaderLock>,
    policy: AuditPolicy,
}

/// The sealed windows, by start, and where the chain begins and ends.
struct Windows {
    by_start: BTreeMap<DateTime<Utc>, DateTime<Utc>>,
    head: Option<SealRecord>,
}

impl Windows {
    fn sealed_until(&self) -> Option<DateTime<Utc>> {
        self.by_start.values().next_back().copied()
    }
    /// The sealed window holding `t`, if any.
    fn holding(&self, t: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let (start, end) = self.by_start.range(..=t).next_back()?;
        (t < *end).then_some((*start, *end))
    }
    fn head_seq(&self) -> i64 {
        self.head.as_ref().map_or(0, |h| h.seq)
    }
}

impl AuditTrailService {
    pub fn new(
        store: Arc<dyn AuditTrailStore>,
        events: Arc<dyn PackageRepository>,
        leader: Arc<dyn LeaderLock>,
        policy: AuditPolicy,
    ) -> Self {
        Self {
            store,
            events,
            leader,
            policy,
        }
    }

    pub fn sealing(&self) -> bool {
        self.policy.sealing.is_some()
    }

    async fn windows(&self) -> Result<Windows, CoreError> {
        let records = self.store.seal_records().await?;
        let by_start = records
            .iter()
            .filter(|r| r.kind == SealKind::Seal)
            .map(|r| (r.window_start, r.window_end))
            .collect();
        Ok(Windows {
            by_start,
            head: records.into_iter().last(),
        })
    }

    fn sign(&self, mut record: SealRecord) -> SealRecord {
        let key = &self
            .policy
            .sealing
            .as_ref()
            .expect("only called when sealing")
            .1;
        record.digest = record_digest(&record);
        record.signature =
            base64::engine::general_purpose::STANDARD.encode(key.sign(record.digest.as_bytes()));
        record.key_id = key.key_id().to_owned();
        record
    }

    fn next_record(
        &self,
        head: Option<&SealRecord>,
        kind: SealKind,
        window: (DateTime<Utc>, DateTime<Utc>),
        rows: &[AccessEvent],
        affected: u64,
    ) -> SealRecord {
        self.sign(SealRecord {
            seq: head.map_or(1, |h| h.seq + 1),
            kind,
            window_start: window.0,
            window_end: window.1,
            row_count: rows.len() as u64,
            affected,
            rows_digest: rows_digest(rows),
            digest: String::new(),
            prev_digest: head.map_or_else(|| GENESIS_DIGEST.to_owned(), |h| h.digest.clone()),
            signature: String::new(),
            key_id: String::new(),
        })
    }

    // ── Sealing ───────────────────────────────────────────────────────────────

    /// Seal every window that closed at least one window ago (RFC 0036 §5.3: a
    /// late insert cannot land in a window already sealed). Leader only.
    pub async fn seal_tick(&self, now: DateTime<Utc>) -> Result<Vec<SealRecord>, CoreError> {
        let Some((interval, _)) = &self.policy.sealing else {
            return Ok(Vec::new());
        };
        if !self.leader.try_lead().await? {
            return Ok(Vec::new());
        }
        let step = chrono::Duration::from_std(*interval)
            .map_err(|e| CoreError::Config(format!("seal interval: {e}")))?;
        let w = self.windows().await?;
        let mut head = w.head.clone();
        let mut start = match w.sealed_until() {
            Some(end) => end,
            // The chain starts with the newest window that is already one
            // window in the past, so the first tick seals it; rows before it
            // are pre-chain, handled by the lifecycle without records.
            None => align(now, interval.as_secs().max(1)) - step - step,
        };
        let mut sealed = Vec::new();
        while sealed.len() < SEALS_PER_TICK && now >= start + step + step {
            let window = (start, start + step);
            let rows = self.store.rows_in(window.0, window.1).await?;
            let record = self.next_record(head.as_ref(), SealKind::Seal, window, &rows, 0);
            let batch = TrailBatch {
                ops: Vec::new(),
                record: Some(record.clone()),
                expect_head: Some(head.as_ref().map_or(0, |h| h.seq)),
            };
            match self.store.apply(batch).await {
                Ok(()) => {}
                // An erasure moved the chain or the rows: next tick.
                Err(CoreError::Conflict(_)) => break,
                Err(e) => return Err(e),
            }
            audit_stream::emit_seal(&record);
            head = Some(record.clone());
            sealed.push(record);
            start = window.1;
        }
        Ok(sealed)
    }

    // ── Rewriting rows, sealed or not ─────────────────────────────────────────

    /// Find rows matching `query`, change each with `change`, and commit the
    /// changes — per sealed window with an `amend` or `expire` record, and for
    /// rows outside any sealed window without one. Returns rows changed.
    ///
    /// `created_before` bounds which rows may be touched: the lifecycle passes
    /// the end of the sealed range so an open window is never rewritten.
    async fn rewrite(
        &self,
        query: &CandidateQuery,
        created_before: Option<DateTime<Utc>>,
        change: &(dyn Fn(&AccessEvent) -> Option<RowOp> + Sync),
        kind: SealKind,
        budget: u64,
    ) -> Result<u64, CoreError> {
        let mut changed = 0;
        let mut conflicts = 0;
        while changed < budget {
            match self
                .rewrite_batch(query, created_before, change, kind)
                .await
            {
                Ok(0) => break,
                Ok(n) => {
                    changed += n;
                    conflicts = 0;
                }
                Err(CoreError::Conflict(e)) => {
                    conflicts += 1;
                    if conflicts >= CONFLICT_RETRIES {
                        return Err(CoreError::Conflict(e));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(changed)
    }

    async fn rewrite_batch(
        &self,
        query: &CandidateQuery,
        created_before: Option<DateTime<Utc>>,
        change: &(dyn Fn(&AccessEvent) -> Option<RowOp> + Sync),
        kind: SealKind,
    ) -> Result<u64, CoreError> {
        let w = self.windows().await?;
        let candidates = self.store.candidates(query, created_before, BATCH).await?;
        if candidates.is_empty() {
            return Ok(0);
        }
        let sealing = self.sealing();
        // Rows in a sealed window, grouped by window; everything else loose.
        let mut grouped: BTreeMap<DateTime<Utc>, (DateTime<Utc>, Vec<RowOp>)> = BTreeMap::new();
        let mut loose = Vec::new();
        for row in &candidates {
            let Some(op) = change(row) else { continue };
            match sealing.then(|| w.holding(row.timestamp)).flatten() {
                Some((start, end)) => grouped.entry(start).or_insert((end, Vec::new())).1.push(op),
                None => loose.push(op),
            }
        }
        let expect = sealing.then(|| w.head_seq());
        let mut changed = 0;
        if !loose.is_empty() {
            changed += loose.len() as u64;
            self.store
                .apply(TrailBatch {
                    ops: loose,
                    record: None,
                    expect_head: expect,
                })
                .await?;
        }
        let mut head = w.head.clone();
        for (start, (end, ops)) in grouped {
            let rows = apply_ops(self.store.rows_in(start, end).await?, &ops);
            let record =
                self.next_record(head.as_ref(), kind, (start, end), &rows, ops.len() as u64);
            changed += ops.len() as u64;
            self.store
                .apply(TrailBatch {
                    ops,
                    record: Some(record.clone()),
                    expect_head: Some(head.as_ref().map_or(0, |h| h.seq)),
                })
                .await?;
            audit_stream::emit_seal(&record);
            head = Some(record);
        }
        // Every query's candidates are rows its change applies to, so a batch
        // that changed nothing means there is nothing left.
        Ok(changed)
    }

    // ── The lifecycle ─────────────────────────────────────────────────────────

    /// Expire, then pseudonymise, what `[audit]` says is due. Leader only;
    /// never touches an open seal window (RFC 0036 §4.2).
    pub async fn lifecycle_tick(&self, now: DateTime<Utc>) -> Result<LifecycleReport, CoreError> {
        let mut report = LifecycleReport::default();
        let p = &self.policy;
        if p.access_retention.is_none()
            && p.security_retention.is_none()
            && p.pseudonymise_after.is_none()
        {
            return Ok(report);
        }
        if !self.leader.try_lead().await? {
            return Ok(report);
        }
        report.leader = true;
        let bound = if self.sealing() {
            match self.windows().await?.sealed_until() {
                Some(end) => Some(end),
                // Sealing is on and nothing is sealed yet: wait for the chain.
                None => return Ok(report),
            }
        } else {
            None
        };
        let expire = CandidateQuery::Expire {
            access_before: p.access_retention.map(|d| now - d),
            security_before: p.security_retention.map(|d| now - d),
        };
        if p.access_retention.is_some() || p.security_retention.is_some() {
            report.expired = self
                .rewrite(
                    &expire,
                    bound,
                    &|r| Some(RowOp::Delete(r.id)),
                    SealKind::Expire,
                    TICK_BUDGET,
                )
                .await?;
        }
        if let Some(after) = p.pseudonymise_after {
            let query = CandidateQuery::Pseudonymise {
                before: now - after,
            };
            report.pseudonymised = self
                .rewrite(
                    &query,
                    bound,
                    &pseudonymise_op,
                    SealKind::Amend,
                    TICK_BUDGET,
                )
                .await?;
        }
        if report.expired + report.pseudonymised > 0 {
            self.record(AccessEvent::about_identity(
                AccessAction::AuditLifecycleRun,
                None,
                Role::Admin,
                AccessResult::Allowed,
                CallerNet::unknown(),
                Some(format!(
                    "pseudonymised={} expired={}",
                    report.pseudonymised, report.expired
                )),
            ))
            .await;
        }
        Ok(report)
    }

    async fn record(&self, event: AccessEvent) {
        if let Err(e) = self.events.record_access(event).await {
            tracing::warn!(error = %e, "audit trail: recording its own event failed");
        }
    }

    /// The manual purge: **access-class** rows older than `before`, and nothing
    /// else — a purge row outlives every later purge (RFC 0036 §4.2). Records
    /// the purge itself, naming the operator.
    pub async fn purge_access_before(
        &self,
        before: DateTime<Utc>,
        by: &Identity,
        net: CallerNet,
    ) -> Result<u64, CoreError> {
        let query = CandidateQuery::Expire {
            access_before: Some(before),
            security_before: None,
        };
        let deleted = self
            .rewrite(
                &query,
                None,
                &|r| Some(RowOp::Delete(r.id)),
                SealKind::Expire,
                u64::MAX,
            )
            .await?;
        self.record(AccessEvent::about_identity(
            AccessAction::AuditPurge,
            by.user_id.clone(),
            by.role.clone(),
            AccessResult::Allowed,
            net,
            Some(format!("before={} deleted={deleted}", before.to_rfc3339())),
        ))
        .await;
        Ok(deleted)
    }

    // ── GDPR ──────────────────────────────────────────────────────────────────

    /// What `user_id` becomes after erasure: `erased:` and the first 32 hex of
    /// HMAC-SHA256 under `[audit] erasure_key`. The same subject always maps to
    /// the same pseudonym, so its rows stay linkable to each other and to
    /// nobody.
    pub fn pseudonym(&self, user_id: &str) -> Result<String, CoreError> {
        let key = self.policy.erasure_key.as_deref().ok_or_else(|| {
            CoreError::NotSupported("erasure needs [audit] erasure_key".to_owned())
        })?;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes())
            .expect("HMAC takes a key of any length");
        mac.update(user_id.as_bytes());
        let tag = hex::encode(mac.finalize().into_bytes());
        Ok(format!("erased:{}", &tag[..32]))
    }

    /// Pseudonymise a data subject (RFC 0036 §4.2): their audit rows, tokens
    /// and blocks. Publication and ownership rows are exempt. Refused while an
    /// open decision names them, unless `force`.
    pub async fn erase(
        &self,
        user_id: &str,
        force: bool,
        by: &Identity,
        net: CallerNet,
    ) -> Result<EraseReport, CoreError> {
        let pseudonym = self.pseudonym(user_id)?;
        if user_id.starts_with("erased:") {
            return Err(CoreError::InvalidInput(format!(
                "'{user_id}' is already a pseudonym"
            )));
        }
        if !force {
            let open = self.store.open_decisions(user_id).await?;
            if !open.is_empty() {
                return Err(CoreError::Conflict(format!(
                    "'{user_id}' is named by an open decision ({}); erase with force to proceed",
                    open.join("; ")
                )));
            }
        }
        let subject_detail = format!("subject={user_id}");
        let erased_detail = format!("subject={pseudonym}");
        let change = |r: &AccessEvent| {
            let mut row = r.clone();
            if row.user_id.as_deref() == Some(user_id) {
                row.user_id = Some(pseudonym.clone());
            }
            if row.detail.as_deref() == Some(subject_detail.as_str()) {
                row.detail = Some(erased_detail.clone());
            }
            (row != *r).then_some(RowOp::Replace(Box::new(row)))
        };
        let query = CandidateQuery::Subject {
            user_id: user_id.to_owned(),
        };
        let audit_rows = self
            .rewrite(&query, None, &change, SealKind::Amend, u64::MAX)
            .await?;
        let elsewhere = self.store.erase_elsewhere(user_id, &pseudonym).await?;
        self.record(AccessEvent::about_identity(
            AccessAction::GdprErase,
            by.user_id.clone(),
            by.role.clone(),
            AccessResult::Allowed,
            net,
            Some(format!(
                "subject={pseudonym} rows={audit_rows} tokens={} blocks={}",
                elsewhere.tokens, elsewhere.blocks
            )),
        ))
        .await;
        Ok(EraseReport {
            pseudonym,
            audit_rows,
            tokens: elsewhere.tokens,
            blocks: elsewhere.blocks,
        })
    }

    /// Everything held about a subject, for an access request (Art. 15).
    pub async fn export(
        &self,
        user_id: &str,
        by: &Identity,
        net: CallerNet,
    ) -> Result<serde_json::Value, CoreError> {
        let out = self.store.subject_export(user_id).await?;
        self.record(AccessEvent::about_identity(
            AccessAction::GdprExport,
            by.user_id.clone(),
            by.role.clone(),
            AccessResult::Allowed,
            net,
            Some(format!("subject={user_id}")),
        ))
        .await;
        Ok(out)
    }

    // ── Verification ──────────────────────────────────────────────────────────

    /// Replay the chain (RFC 0036 §6.4): every record's digest and signature,
    /// every link, and every window in `[from, to)` against its latest record.
    /// `head` is the digest of the newest `audit_seal` line the SIEM holds.
    pub async fn verify(
        &self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        head: Option<&str>,
    ) -> Result<VerifyReport, CoreError> {
        let key = self
            .policy
            .sealing
            .as_ref()
            .map(|(_, k)| k)
            .ok_or_else(|| {
                CoreError::NotSupported(
                    "the audit trail is not sealed ([audit] seal_interval_secs)".into(),
                )
            })?;
        let records = self.store.seal_records().await?;
        let mut failures = Vec::new();
        let fail = |r: Option<&SealRecord>, reason: String| VerifyFailure {
            window_start: r.map(|r| r.window_start),
            window_end: r.map(|r| r.window_end),
            seq: r.map(|r| r.seq),
            reason,
        };

        let mut prev: Option<&SealRecord> = None;
        let mut latest: HashMap<(DateTime<Utc>, DateTime<Utc>), &SealRecord> = HashMap::new();
        for r in &records {
            let want_seq = prev.map_or(1, |p| p.seq + 1);
            let want_prev = prev.map_or(GENESIS_DIGEST, |p| p.digest.as_str());
            if r.seq != want_seq {
                failures.push(fail(
                    Some(r),
                    format!("record {} follows {}", r.seq, want_seq - 1),
                ));
            }
            if r.prev_digest != want_prev {
                failures.push(fail(
                    Some(r),
                    "does not chain to the record before it".into(),
                ));
            }
            if record_digest(r) != r.digest {
                failures.push(fail(Some(r), "its digest does not match its fields".into()));
            }
            let signature_ok = r.key_id == key.key_id()
                && base64::engine::general_purpose::STANDARD
                    .decode(&r.signature)
                    .is_ok_and(|sig| key.verify(&sig, r.digest.as_bytes()));
            if !signature_ok {
                failures.push(fail(
                    Some(r),
                    format!("signature does not verify under key {}", key.key_id()),
                ));
            }
            latest.insert((r.window_start, r.window_end), r);
            prev = Some(r);
        }

        let mut windows: Vec<_> = latest.into_iter().collect();
        windows.sort_by_key(|((start, _), _)| *start);
        let (mut checked, mut expired) = (0, 0);
        for ((start, end), r) in windows {
            if from.is_some_and(|f| end <= f) || to.is_some_and(|t| start >= t) {
                continue;
            }
            let rows = self.store.rows_in(start, end).await?;
            if r.kind == SealKind::Expire && r.row_count == 0 && rows.is_empty() {
                expired += 1;
                continue;
            }
            checked += 1;
            if rows.len() as u64 != r.row_count || rows_digest(&rows) != r.rows_digest {
                failures.push(fail(
                    Some(r),
                    format!(
                        "the window holds {} rows that match no signed record (its latest, {}, says {})",
                        rows.len(),
                        r.seq,
                        r.row_count
                    ),
                ));
            }
        }

        let newest = records.last().map(|r| r.digest.clone());
        if let Some(h) = head {
            if newest.as_deref() != Some(h) {
                failures.push(fail(
                    records.last(),
                    "the chain's newest record is not the head the SIEM last received: \
                     its tail was truncated or rewritten"
                        .into(),
                ));
            }
        }
        Ok(VerifyReport {
            ok: failures.is_empty(),
            records: records.len() as u64,
            windows_checked: checked,
            windows_expired: expired,
            head: newest,
            truncation_checked: head.is_some(),
            failures,
        })
    }
}

/// `t` rounded down to a multiple of `secs` since the epoch.
fn align(t: DateTime<Utc>, secs: u64) -> DateTime<Utc> {
    let s = t.timestamp() - t.timestamp().rem_euclid(secs as i64);
    Utc.timestamp_opt(s, 0)
        .single()
        .expect("an aligned timestamp is valid")
}

/// A window's rows with `ops` applied — what it will hold once they commit.
fn apply_ops(rows: Vec<AccessEvent>, ops: &[RowOp]) -> Vec<AccessEvent> {
    let mut replace: HashMap<Uuid, &AccessEvent> = HashMap::new();
    let mut delete = std::collections::HashSet::new();
    for op in ops {
        match op {
            RowOp::Replace(e) => {
                replace.insert(e.id, e);
            }
            RowOp::Delete(id) => {
                delete.insert(*id);
            }
        }
    }
    rows.into_iter()
        .filter(|r| !delete.contains(&r.id))
        .map(|r| match replace.get(&r.id) {
            Some(new) => AccessEvent {
                user_id: new.user_id.clone(),
                ip_address: new.ip_address.clone(),
                user_agent: new.user_agent.clone(),
                detail: new.detail.clone(),
                ..r
            },
            None => r,
        })
        .collect()
}

/// An access row with its IP truncated and its user agent dropped, or `None`
/// when there is nothing left to take.
///
/// An IP that does not parse cannot be made less precise, so it is dropped:
/// it is still personal data, and keeping it would also keep the row a
/// candidate forever.
fn pseudonymise_op(r: &AccessEvent) -> Option<RowOp> {
    let mut row = r.clone();
    row.user_agent = None;
    row.ip_address = r.ip_address.as_deref().and_then(|ip| {
        let cut = truncate_ip(ip);
        (cut != ip || ip.contains('/')).then_some(cut)
    });
    (row != *r).then_some(RowOp::Replace(Box::new(row)))
}

/// An IP reduced to its network: `/24` for IPv4, `/48` for IPv6, written as
/// the network with its prefix (`203.0.113.0/24`). Already-truncated values
/// and anything that does not parse are returned as they are — a value this
/// cannot read is not one it can make less precise.
pub fn truncate_ip(ip: &str) -> String {
    match ip.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            let o = v4.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            let net = std::net::Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0);
            format!("{net}/48")
        }
        Err(_) => ip.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ip_is_cut_to_its_network_and_a_cut_one_stays_cut() {
        assert_eq!(truncate_ip("203.0.113.77"), "203.0.113.0/24");
        assert_eq!(truncate_ip("2001:db8:abcd:12::1"), "2001:db8:abcd::/48");
        assert_eq!(truncate_ip("203.0.113.0/24"), "203.0.113.0/24");
        assert_eq!(truncate_ip("not-an-ip"), "not-an-ip");
    }

    #[test]
    fn windows_align_to_the_epoch() {
        let t = Utc.timestamp_opt(1_000_123, 0).unwrap();
        assert_eq!(align(t, 300).timestamp(), 999_900);
    }

    #[test]
    fn pseudonymisation_takes_what_is_left_and_then_nothing() {
        let mut e = AccessEvent::about_identity(
            AccessAction::Download,
            Some("alice".into()),
            Role::User,
            AccessResult::Allowed,
            CallerNet {
                ip: Some("10.1.2.3".into()),
                user_agent: Some("npm/10".into()),
            },
            None,
        );
        let Some(RowOp::Replace(cut)) = pseudonymise_op(&e) else {
            panic!("a full row is rewritten")
        };
        assert_eq!(cut.ip_address.as_deref(), Some("10.1.2.0/24"));
        assert_eq!(cut.user_agent, None);
        assert_eq!(
            cut.user_id.as_deref(),
            Some("alice"),
            "the user stays; only precision goes"
        );
        e = *cut;
        assert!(
            pseudonymise_op(&e).is_none(),
            "a pseudonymised row is left alone"
        );
        e.ip_address = Some("garbage".into());
        let Some(RowOp::Replace(dropped)) = pseudonymise_op(&e) else {
            panic!("an unreadable IP is still taken")
        };
        assert_eq!(dropped.ip_address, None);
    }
}
