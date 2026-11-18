//! The security console's summary endpoints (RFC 0050 §4.4, §6.4): the
//! overview, trends, coverage and scanner health under
//! `/api/v1/admin/security/*`.
//!
//! What is cached is **identity-independent source data** — verdict counts
//! and holds, flags, heartbeats, scanner rows, the day rows — once per
//! process, with no key per caller ([`SourceCache`]). Every request then
//! applies its own filters: §4.2's verb table, `authorize_control` per
//! registry, `config_tier_holds` per coordinate. A sealed registry is
//! answered as an unknown one (decision 27).

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{get, web, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use futures::future::{BoxFuture, FutureExt, Shared};
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use batlehub_core::entities::{
    Action, FlagEffect, FlagFilter, GateExemption, Identity, PackageFlag, PackageId, ReasonCode,
    ScanJob, ScanJobFilter, Verdict, VerdictState,
};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{
    AdvisoryRepository, DailyEvents, DailyState, FlagPulls, HoldCounts, QueuedCount, ScannerStatus,
    SecurityDailyRow, WorkerHeartbeat,
};
use batlehub_core::services::authz;
use batlehub_core::services::hot_config::HotConfigLock;
use batlehub_core::services::security_overview::{self as rules, LineState, ScannerState};

use crate::error::AppError;
use crate::extractors::AuthIdentity;

// ── the source cache ─────────────────────────────────────────────────────────

/// How long one source read may take before its block reads `unknown`.
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long an entry serves, except the scanners page's own read (5 s).
pub const SOURCE_TTL: Duration = Duration::from_secs(30);
pub const SCANNERS_TTL: Duration = Duration::from_secs(5);

type Loaded = Result<Arc<dyn Any + Send + Sync>, String>;

struct Entry {
    at: Instant,
    fut: Shared<BoxFuture<'static, Loaded>>,
}

/// One entry per source, no caller key, filled single-flight (a shared
/// future), each read under [`SOURCE_TIMEOUT`]. With `[database] read_cache
/// = false` an entry serves only while it is in flight: concurrent identical
/// reads still share one query, and nothing outlives it (decision 37).
pub struct SourceCache {
    enabled: bool,
    entries: tokio::sync::Mutex<HashMap<&'static str, Entry>>,
}

impl SourceCache {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            entries: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The source `key`, from the entry if it is fresh (or in flight), else
    /// from `load`. An error is a string: the block's `unknown_reason`.
    pub async fn get<T, F>(
        &self,
        key: &'static str,
        ttl: Duration,
        load: F,
    ) -> Result<Arc<T>, String>
    where
        T: Any + Send + Sync,
        F: Future<Output = Result<T, CoreError>> + Send + 'static,
    {
        let fut = {
            let mut entries = self.entries.lock().await;
            let fresh = entries.get(key).filter(|e| {
                e.fut.peek().is_none()
                    || (self.enabled
                        && e.at.elapsed() < ttl
                        && e.fut.peek().is_some_and(Result::is_ok))
            });
            match fresh {
                Some(e) => e.fut.clone(),
                None => {
                    // Its own task: the timeout runs, and the load's connections
                    // go back to the pool, even when every caller has gone.
                    let task = tokio::spawn(async move {
                        match tokio::time::timeout(SOURCE_TIMEOUT, load).await {
                            Ok(Ok(v)) => Ok(Arc::new(v) as Arc<dyn Any + Send + Sync>),
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(_) => Err("the source did not answer within 2 s".to_owned()),
                        }
                    });
                    let fut = async move {
                        task.await
                            .unwrap_or_else(|e| Err(format!("the source failed: {e}")))
                    }
                    .boxed()
                    .shared();
                    entries.insert(
                        key,
                        Entry {
                            at: Instant::now(),
                            fut: fut.clone(),
                        },
                    );
                    fut
                }
            }
        };
        fut.await?
            .downcast::<T>()
            .map_err(|_| format!("source '{key}' holds another type"))
    }
}

/// The process's cache, or a fresh disabled one when the app registered none.
pub(crate) fn cache_of(cache: Option<web::Data<SourceCache>>) -> Arc<SourceCache> {
    cache.map_or_else(|| Arc::new(SourceCache::new(false)), |c| c.into_inner())
}

// ── the sources ──────────────────────────────────────────────────────────────

/// The hard-block holds read for *Needs a decision*; the line's counts are
/// store aggregates. shortcut: past this many `SOC_VERDICT` holds the
/// oldest are not listed (nor in `total`); page it if an estate gets there.
const HARD_BLOCKS_READ: u64 = 1000;

/// The failed jobs read as rows; the per-registry counts stay exact past it.
const FAILED_PAGE: u64 = 1000;

/// How many of one source's per-registry reads run at once.
const SOURCE_READS_AT_ONCE: usize = 4;

struct HoldsSource {
    counts: Vec<(String, VerdictState, u64)>,
    /// Exact per-registry figures of the *Holds* line.
    holds: Vec<HoldCounts>,
    /// Held for `SOC_VERDICT`, newest first, without findings.
    hard_blocks: Vec<Verdict>,
}

struct FlagsSource {
    live: Vec<PackageFlag>,
    /// Per version, pulls after a flag still live (30 days).
    pulled_after: Vec<FlagPulls>,
}

struct ScannersSource {
    workers: Vec<WorkerHeartbeat>,
    queued: Vec<QueuedCount>,
    /// The newest failed jobs, for *Needs a decision*; the counts come from
    /// `failed_by_registry`, exact past this page.
    failed: Vec<ScanJob>,
    failed_by_registry: BTreeMap<String, u64>,
    status: Vec<ScannerStatus>,
}

pub(crate) struct ExemptionRow {
    pub registry: String,
    pub package: String,
    pub version: String,
    pub exempt_until: DateTime<Utc>,
    pub self_approved: bool,
}

async fn protected_registries(hot: &HotConfigLock) -> Vec<String> {
    let mut names: Vec<String> = hot.read().await.security.keys().cloned().collect();
    names.sort();
    names
}

async fn holds_source(
    cache: &SourceCache,
    hot: &HotConfigLock,
) -> Result<Arc<HoldsSource>, String> {
    let hot = Arc::clone(hot);
    cache
        .get("holds", SOURCE_TTL, async move {
            let verdicts = hot.read().await.verdicts.clone().ok_or_else(|| {
                CoreError::NotSupported("no verdict store in this process".into())
            })?;
            // Aggregates, not pages: the old holds the rules catch are counted.
            let pending_before = Utc::now() - rules::PENDING_ATTENTION;
            let (counts, holds, hard_blocks) = tokio::try_join!(
                verdicts.count_by_registry_and_state(),
                verdicts.hold_counts(pending_before),
                verdicts.list_held_with_code(ReasonCode::SocVerdict, HARD_BLOCKS_READ),
            )?;
            Ok(HoldsSource {
                counts,
                holds,
                hard_blocks,
            })
        })
        .await
}

async fn flags_source(
    cache: &SourceCache,
    advisories: Option<Arc<dyn AdvisoryRepository>>,
) -> Result<Arc<FlagsSource>, String> {
    cache
        .get("flags", SOURCE_TTL, async move {
            let repo = advisories
                .ok_or_else(|| CoreError::NotSupported("no flag store in this process".into()))?;
            let live = repo
                .list_flags(&FlagFilter {
                    limit: 10_000,
                    ..Default::default()
                })
                .await?;
            // Every flagged version, per version, live flags only: the
            // caller's filter runs on the whole set, not on a global page.
            let now = Utc::now();
            let pulled_after = repo
                .pulls_after_live_flags(now - chrono::Duration::days(30), now)
                .await?;
            Ok(FlagsSource { live, pulled_after })
        })
        .await
}

async fn scanners_source(
    cache: &SourceCache,
    hot: &HotConfigLock,
    ttl: Duration,
) -> Result<Arc<ScannersSource>, String> {
    let hot = Arc::clone(hot);
    cache
        .get("scanners", ttl, async move {
            let names = protected_registries(&hot).await;
            let (queue, workers, status) = {
                let h = hot.read().await;
                (
                    h.scan_queue.clone(),
                    h.worker_registry.clone(),
                    h.scanner_status.clone(),
                )
            };
            let queue = queue
                .ok_or_else(|| CoreError::NotSupported("no scan queue in this process".into()))?;
            let workers = match workers {
                Some(w) => w.workers().await?,
                None => Vec::new(),
            };
            let status = match status {
                Some(s) => s.list().await?,
                None => Vec::new(),
            };
            let queued = queue.queued().await?;
            let (failed, total) = queue
                .list_jobs(&names, ScanJobFilter::Failed, FAILED_PAGE, 0)
                .await?;
            let mut failed_by_registry: BTreeMap<String, u64> = BTreeMap::new();
            if total > failed.len() as u64 {
                // Past one page, one exact count per registry, concurrently.
                let reads: Vec<BoxFuture<'static, Result<(String, u64), CoreError>>> = names
                    .iter()
                    .map(|n| {
                        let (queue, n) = (Arc::clone(&queue), n.clone());
                        async move {
                            let one = std::slice::from_ref(&n);
                            let (_, t) = queue.list_jobs(one, ScanJobFilter::Failed, 1, 0).await?;
                            Ok((n, t))
                        }
                        .boxed()
                    })
                    .collect();
                let totals: Vec<(String, u64)> = futures::stream::iter(reads)
                    .buffer_unordered(SOURCE_READS_AT_ONCE)
                    .try_collect()
                    .await?;
                failed_by_registry.extend(totals);
            } else {
                for j in &failed {
                    *failed_by_registry
                        .entry(j.package.registry.clone())
                        .or_default() += 1;
                }
            }
            Ok(ScannersSource {
                workers,
                queued,
                failed,
                failed_by_registry,
                status,
            })
        })
        .await
}

pub(crate) async fn exemptions_source(
    cache: &SourceCache,
    hot: &HotConfigLock,
) -> Result<Arc<Vec<ExemptionRow>>, String> {
    let hot = Arc::clone(hot);
    cache
        .get("exemptions", SOURCE_TTL, async move {
            let (repo, mut names) = {
                let h = hot.read().await;
                (
                    h.policy_repo.clone(),
                    h.registries.keys().cloned().collect::<Vec<_>>(),
                )
            };
            names.sort();
            let Some(repo) = repo else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for registry in names {
                for row in repo.exemptions_in_registry(&registry).await? {
                    let (package, version) = row
                        .node_key
                        .rsplit_once('@')
                        .map(|(p, v)| (p.to_owned(), v.to_owned()))
                        .unwrap_or_else(|| (row.node_key.clone(), String::new()));
                    // One row per version, over the gates still exempted: the
                    // first of those to lapse is when it needs a decision, and
                    // any self-approval taints it. A lapsed gate is history.
                    let now = Utc::now();
                    let mut merged: Option<ExemptionRow> = None;
                    for rule in &row.rules {
                        let Ok(ex) = serde_json::from_value::<GateExemption>(rule.settings.clone())
                        else {
                            continue;
                        };
                        if !ex.exempt || ex.exempt_until <= now {
                            continue;
                        }
                        match &mut merged {
                            Some(m) => {
                                m.exempt_until = m.exempt_until.min(ex.exempt_until);
                                m.self_approved |= ex.self_approved;
                            }
                            None => {
                                merged = Some(ExemptionRow {
                                    registry: registry.clone(),
                                    package: package.clone(),
                                    version: version.clone(),
                                    exempt_until: ex.exempt_until,
                                    self_approved: ex.self_approved,
                                })
                            }
                        }
                    }
                    out.extend(merged);
                }
            }
            Ok(out)
        })
        .await
}

async fn daily_source(
    cache: &SourceCache,
    hot: &HotConfigLock,
) -> Result<Arc<Vec<SecurityDailyRow>>, String> {
    let hot = Arc::clone(hot);
    cache
        .get("daily", SOURCE_TTL, async move {
            let repo = hot.read().await.security_daily.clone().ok_or_else(|| {
                CoreError::NotSupported("no history store in this process".into())
            })?;
            let today = Utc::now().date_naive();
            repo.range(today - chrono::Duration::days(89), today).await
        })
        .await
}

// ── per-request filters ──────────────────────────────────────────────────────

pub(crate) async fn holds_at(
    hot: &HotConfigLock,
    id: &Identity,
    registry: Option<&str>,
    action: Action,
) -> bool {
    authz::authorize_control(hot, registry, id, action)
        .await
        .is_ok()
}

/// The registries among `names` the caller holds `action` at: a sealed one is
/// as absent as an unknown one.
pub(crate) async fn visible(
    hot: &HotConfigLock,
    id: &Identity,
    names: &[String],
    action: Action,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for name in names {
        if holds_at(hot, id, Some(name), action).await {
            out.insert(name.clone());
        }
    }
    out
}

/// Which of `coords` the caller holds `action` on, at their registry and
/// namespace (the per-coordinate check of `sealed_out`).
pub(crate) async fn coords_held(
    hot: &HotConfigLock,
    id: &Identity,
    coords: &[PackageId],
    action: Action,
) -> Vec<bool> {
    authz::config_tier_holds(hot, id, coords, &[action])
        .await
        .into_iter()
        .map(|h| h.first().copied().unwrap_or(false))
        .collect()
}

fn forbidden() -> AppError {
    AppError::forbidden("this endpoint requires the 'quarantine:read' permission")
}

// ── the overview ─────────────────────────────────────────────────────────────

/// The *Holds* line.
#[derive(Debug, Serialize, ToSchema)]
pub struct HoldsBlock {
    pub state: LineState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
    /// Denied + quarantined.
    pub held: u64,
    pub denied: u64,
    pub quarantined: u64,
    /// Held for `KNOWN_MALICIOUS`, `MALWARE_SIGNAL` or `SOC_VERDICT`.
    pub malware: u64,
    /// Quarantined only for `MIN_AGE_NOT_MET` or `SCAN_PENDING`.
    pub waiting: u64,
    pub pending_over_hour: u64,
    /// Held for `SOC_VERDICT`, exact at any volume: the figure a SOC reads
    /// in a mass compromise, before paging the full list (`/admin/verdicts`).
    pub soc_verdict: u64,
}

/// The *Flags* line.
#[derive(Debug, Serialize, ToSchema)]
pub struct FlagsBlock {
    pub state: LineState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
    pub live: u64,
    /// Live flags that block downloads (`gate`, `hard_block`).
    pub blocking: u64,
    /// Flagged versions downloaded after the flag (last 30 days).
    pub pulled_after_flag: u64,
}

/// The *Scanners & worker* line.
#[derive(Debug, Serialize, ToSchema)]
pub struct ScannersBlock {
    pub state: LineState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
    pub live_workers: u64,
    /// Protected registries no live worker covers.
    pub uncovered: Vec<String>,
    pub queued: u64,
    pub deferred: u64,
    pub deferred_over_day: u64,
    pub failed: u64,
    pub scanners_attention: u64,
    pub never_ran: u64,
    pub breaker_open: bool,
}

/// The *Coverage* line.
#[derive(Debug, Serialize, ToSchema)]
pub struct CoverageBlock {
    pub state: LineState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
    pub registries: u64,
    /// Visible registries that serve upstream (`mode` other than `local`).
    pub upstream: u64,
    pub protected: u64,
    /// `proxy`/`hybrid` registries with no `[security]` and no `cve_gate`.
    pub unprotected: Vec<String>,
}

/// One item of *Needs a decision*.
#[derive(Debug, Serialize, ToSchema)]
pub struct DecisionItem {
    /// `hold_on_hard_block`, `pulled_after_flag`, `exemption_expiring` or
    /// `failed_scan`.
    pub kind: String,
    pub registry: String,
    pub name: String,
    pub version: String,
    pub at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pulls_after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// *Needs a decision*: at most ten, most urgent first, and how many there are.
#[derive(Debug, Serialize, ToSchema)]
pub struct DecisionList {
    pub total: u64,
    pub items: Vec<DecisionItem>,
    /// The sources (`holds`, `scanners`, `exemptions`, `flags`) that could
    /// not be read: the list is known to be incomplete.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
}

/// `GET /api/v1/admin/security/overview`. A block is absent when its verb is
/// not held; present with `state = "unknown"` when its source failed.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityOverview {
    pub computed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holds: Option<HoldsBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<FlagsBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanners: Option<ScannersBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageBlock>,
    pub decisions: DecisionList,
}

const DECISIONS_SHOWN: usize = 10;

fn holds_block(src: &Result<Arc<HoldsSource>, String>, regs: &BTreeSet<String>) -> HoldsBlock {
    let src = match src {
        Ok(s) => s,
        Err(e) => {
            return HoldsBlock {
                state: LineState::Unknown,
                unknown_reason: Some(e.clone()),
                held: 0,
                denied: 0,
                quarantined: 0,
                malware: 0,
                waiting: 0,
                pending_over_hour: 0,
                soc_verdict: 0,
            }
        }
    };
    let (mut denied, mut quarantined) = (0, 0);
    for (r, state, n) in &src.counts {
        if regs.contains(r) {
            match state {
                VerdictState::Denied => denied += n,
                VerdictState::Quarantined => quarantined += n,
                _ => {}
            }
        }
    }
    let mut c = HoldCounts::default();
    for h in src.holds.iter().filter(|h| regs.contains(&h.registry)) {
        c.quarantined_blocking += h.quarantined_blocking;
        c.malware += h.malware;
        c.waiting += h.waiting;
        c.pending_over_hour += h.pending_over_hour;
        c.soc_verdict += h.soc_verdict;
    }
    HoldsBlock {
        state: LineState::from_attention(rules::holds_need_attention(
            denied,
            c.quarantined_blocking,
            c.pending_over_hour,
        )),
        unknown_reason: None,
        held: denied + quarantined,
        denied,
        quarantined,
        malware: c.malware,
        waiting: c.waiting,
        pending_over_hour: c.pending_over_hour,
        soc_verdict: c.soc_verdict,
    }
}

fn flags_block(src: &Result<Arc<FlagsSource>, String>, regs: &BTreeSet<String>) -> FlagsBlock {
    let src = match src {
        Ok(s) => s,
        Err(e) => {
            return FlagsBlock {
                state: LineState::Unknown,
                unknown_reason: Some(e.clone()),
                live: 0,
                blocking: 0,
                pulled_after_flag: 0,
            }
        }
    };
    let now = Utc::now();
    let live: Vec<&PackageFlag> = src
        .live
        .iter()
        .filter(|f| f.is_live(now) && regs.contains(&f.registry))
        .collect();
    let blocking = live
        .iter()
        .filter(|f| matches!(f.effect, FlagEffect::Gate | FlagEffect::HardBlock))
        .count() as u64;
    // One row per version already.
    let pulled = src
        .pulled_after
        .iter()
        .filter(|r| regs.contains(&r.registry))
        .count() as u64;
    FlagsBlock {
        state: LineState::from_attention(blocking > 0),
        unknown_reason: None,
        live: live.len() as u64,
        blocking,
        pulled_after_flag: pulled,
    }
}

/// The scanners every visible protected registry configures, less those that
/// cannot scan its kind: the worker never runs them there, so they would read
/// as never ran for good.
async fn configured_scanners(
    hot: &HotConfigLock,
    regs: &BTreeSet<String>,
) -> BTreeMap<String, bool> {
    let hot = hot.read().await;
    let mut out: BTreeMap<String, bool> = BTreeMap::new();
    for (name, policy) in hot.security.iter().filter(|(n, _)| regs.contains(*n)) {
        let kind = hot.registries.get(name).and_then(|c| {
            c.registry_type()
                .parse::<batlehub_core::entities::RegistryKind>()
                .ok()
        });
        for s in &policy.scanners {
            if let (Some(k), Some(kinds)) = (kind, hot.scanner_kinds.get(s)) {
                if !kinds.contains(&k) {
                    continue;
                }
            }
            let required = policy.required_scanners.contains(s);
            let e = out.entry(s.clone()).or_default();
            *e = *e || required;
        }
    }
    out
}

/// The workers as far as the caller sees: one that serves every registry (an
/// empty list) as it is, one that serves named registries with only the
/// visible ones, and none whose registries are all sealed (decision 27).
fn visible_workers(workers: &[WorkerHeartbeat], regs: &BTreeSet<String>) -> Vec<WorkerHeartbeat> {
    workers
        .iter()
        .filter_map(|w| {
            if w.registries.is_empty() {
                return Some(w.clone());
            }
            let registries: Vec<String> = w
                .registries
                .iter()
                .filter(|r| regs.contains(*r))
                .cloned()
                .collect();
            (!registries.is_empty()).then(|| WorkerHeartbeat {
                registries,
                ..w.clone()
            })
        })
        .collect()
}

async fn scanners_block(
    hot: &HotConfigLock,
    src: &Result<Arc<ScannersSource>, String>,
    regs: &BTreeSet<String>,
) -> ScannersBlock {
    let src = match src {
        Ok(s) => s,
        Err(e) => {
            return ScannersBlock {
                state: LineState::Unknown,
                unknown_reason: Some(e.clone()),
                live_workers: 0,
                uncovered: Vec::new(),
                queued: 0,
                deferred: 0,
                deferred_over_day: 0,
                failed: 0,
                scanners_attention: 0,
                never_ran: 0,
                breaker_open: false,
            }
        }
    };
    let now = Utc::now();
    let live_workers = visible_workers(&src.workers, regs)
        .iter()
        .filter(|w| rules::worker_is_live(w, now))
        .count() as u64;
    let uncovered = rules::uncovered(regs.iter(), &src.workers, now);
    let (mut queued, mut deferred, mut deferred_over) = (0, 0, 0);
    for c in src.queued.iter().filter(|c| regs.contains(&c.registry)) {
        queued += c.count;
        deferred += c.deferred;
        deferred_over += c.deferred_over_day;
    }
    let failed = src
        .failed_by_registry
        .iter()
        .filter(|(r, _)| regs.contains(*r))
        .map(|(_, n)| n)
        .sum();
    let rows: HashMap<&str, &ScannerStatus> =
        src.status.iter().map(|s| (s.scanner.as_str(), s)).collect();
    let configured = configured_scanners(hot, regs).await;
    let states: Vec<ScannerState> = configured
        .keys()
        .map(|name| rules::scanner_state(rows.get(name.as_str()).copied(), now))
        .collect();
    let scanners_attention = states
        .iter()
        .filter(|s| **s == ScannerState::Attention)
        .count() as u64;
    let never_ran = states
        .iter()
        .filter(|s| **s == ScannerState::NeverRan)
        .count() as u64;
    let breaker_open = configured.keys().any(|n| {
        rows.get(n.as_str())
            .and_then(|r| r.unavailable_until)
            .is_some_and(|t| t > now)
    });
    let attention = !uncovered.is_empty()
        || scanners_attention > 0
        || never_ran > 0
        || breaker_open
        || deferred_over > 0;
    ScannersBlock {
        state: LineState::from_attention(attention),
        unknown_reason: None,
        live_workers,
        uncovered,
        queued,
        deferred,
        deferred_over_day: deferred_over,
        failed,
        scanners_attention,
        never_ran,
        breaker_open,
    }
}

/// `mode` of a registry, from the mode map when the app registered one.
fn mode_of(modes: Option<&web::Data<crate::RegistryModeMap>>, registry: &str) -> String {
    modes
        .map(|m| m.get(registry).as_str().to_owned())
        .unwrap_or_else(|| "proxy".to_owned())
}

async fn coverage_block(
    hot: &HotConfigLock,
    modes: Option<&web::Data<crate::RegistryModeMap>>,
    regs: &BTreeSet<String>,
) -> CoverageBlock {
    let hot = hot.read().await;
    let mut unprotected = Vec::new();
    let (mut protected, mut upstream) = (0, 0);
    for r in regs {
        let is_protected = hot.security.contains_key(r);
        protected += u64::from(is_protected);
        let cve = has_rule(&hot, r, "cve_gate");
        let serves_upstream = mode_of(modes, r) != "local";
        upstream += u64::from(serves_upstream);
        if rules::coverage_needs_attention(serves_upstream, is_protected, cve) {
            unprotected.push(r.clone());
        }
    }
    CoverageBlock {
        state: LineState::from_attention(!unprotected.is_empty()),
        unknown_reason: None,
        registries: regs.len() as u64,
        upstream,
        protected,
        unprotected,
    }
}

fn has_rule(
    hot: &batlehub_core::services::hot_config::HotConfig,
    registry: &str,
    rule: &str,
) -> bool {
    // On a `[security]` registry the builder moves `cve_gate`/`license_gate`
    // out of the chain into the worker's internal scanners, under one name.
    hot.policies
        .get(registry)
        .is_some_and(|p| p.rules.iter().any(|r| r.name() == rule))
        || hot
            .internal_scanners
            .get(registry)
            .is_some_and(|s| s.iter().any(|s| s.name() == rule))
}

/// Holds on a SOC hard block in `regs`; returns how many such holds the
/// counts saw past the rows read, so the exact figure still reaches `total`:
/// in a mass compromise the SOC must not read "1 000" for "1 400".
fn hard_block_decisions(
    src: &HoldsSource,
    regs: &BTreeSet<String>,
    out: &mut Vec<DecisionItem>,
) -> u64 {
    let listed = src
        .hard_blocks
        .iter()
        .filter(|v| regs.contains(&v.package.registry))
        .count() as u64;
    let counted: u64 = src
        .holds
        .iter()
        .filter(|h| regs.contains(&h.registry))
        .map(|h| h.soc_verdict)
        .sum();
    out.extend(
        src.hard_blocks
            .iter()
            .filter(|v| regs.contains(&v.package.registry))
            .map(|v| DecisionItem {
                kind: "hold_on_hard_block".into(),
                registry: v.package.registry.clone(),
                name: v.package.name.clone(),
                version: v.package.version.clone(),
                at: v.evaluated_at,
                reason_code: Some(ReasonCode::SocVerdict.as_str().to_owned()),
                pulls_after: None,
                expires_at: None,
            }),
    );
    counted.saturating_sub(listed)
}

/// Failed scans in `regs`, one item per version: a version whose rescan
/// failed again has two failed jobs, newest first.
fn failed_scan_decisions(
    src: &ScannersSource,
    regs: &BTreeSet<String>,
    out: &mut Vec<DecisionItem>,
) {
    let mut seen = BTreeSet::new();
    for j in src.failed.iter().filter(|j| {
        regs.contains(&j.package.registry)
            && seen.insert((
                j.package.registry.as_str(),
                j.package.name.as_str(),
                j.package.version.as_str(),
            ))
    }) {
        out.push(DecisionItem {
            kind: "failed_scan".into(),
            registry: j.package.registry.clone(),
            name: j.package.name.clone(),
            version: j.package.version.clone(),
            at: j.completed_at.unwrap_or(j.created_at),
            reason_code: None,
            pulls_after: None,
            expires_at: None,
        });
    }
}

/// Exemptions in `visible` that lapse within seven days.
fn expiring_exemption_decisions(
    src: &[ExemptionRow],
    visible: &BTreeSet<String>,
    now: DateTime<Utc>,
    out: &mut Vec<DecisionItem>,
) {
    let soon = now + chrono::Duration::days(7);
    out.extend(
        src.iter()
            .filter(|e| {
                visible.contains(&e.registry) && e.exempt_until > now && e.exempt_until <= soon
            })
            .map(|e| DecisionItem {
                kind: "exemption_expiring".into(),
                registry: e.registry.clone(),
                name: e.package.clone(),
                version: e.version.clone(),
                at: e.exempt_until,
                reason_code: None,
                pulls_after: None,
                expires_at: Some(e.exempt_until),
            }),
    );
}

/// The security console's overview: one line per domain the caller may
/// read, and *needs a decision* (RFC 0050 §4.2).
#[utoipa::path(
    get,
    path = "/api/v1/admin/security/overview",
    tag = "security",
    responses(
        (status = 200, description = "The status strip and the items that need a decision", body = SecurityOverview),
        (status = 403, description = "Neither `quarantine:read` nor `flags:read` at the instance tier"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/security/overview")]
pub async fn security_overview(
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    advisories: Option<web::Data<Arc<dyn AdvisoryRepository>>>,
    modes: Option<web::Data<crate::RegistryModeMap>>,
    cache: Option<web::Data<SourceCache>>,
) -> Result<HttpResponse, AppError> {
    let id = &identity.0;
    let can_q = holds_at(&hot, id, None, Action::QuarantineRead).await;
    let can_f = holds_at(&hot, id, None, Action::FlagsRead).await;
    // The check runs before any source is read (§6.P observation 3).
    if !can_q && !can_f {
        return Err(forbidden());
    }
    let cache = cache_of(cache);
    let advisories = advisories.map(|a| Arc::clone(a.get_ref()));
    let protected = protected_registries(&hot).await;
    let all: Vec<String> = {
        let mut v: Vec<String> = hot.read().await.registries.keys().cloned().collect();
        v.sort();
        v
    };
    let now = Utc::now();
    let mut decisions: Vec<DecisionItem> = Vec::new();
    let mut unlisted_hard_blocks: u64 = 0;
    // A source that failed drops its items: say so rather than look complete.
    let mut unknown: Vec<String> = Vec::new();

    let (holds, scanners, coverage) = if can_q {
        let regs = visible(&hot, id, &protected, Action::QuarantineRead).await;
        let all_visible = visible(&hot, id, &all, Action::QuarantineRead).await;
        let (h, s, ex) = tokio::join!(
            holds_source(&cache, &hot),
            scanners_source(&cache, &hot, SOURCE_TTL),
            exemptions_source(&cache, &hot),
        );
        // Holds on a hard block, failed scans and expiring exemptions, each
        // coordinate checked at its own registry and namespace.
        let mut candidates: Vec<DecisionItem> = Vec::new();
        for (name, failed) in [
            ("holds", h.is_err()),
            ("scanners", s.is_err()),
            ("exemptions", ex.is_err()),
        ] {
            if failed {
                unknown.push(name.to_owned());
            }
        }
        if let Ok(src) = &h {
            unlisted_hard_blocks = hard_block_decisions(src, &regs, &mut candidates);
        }
        if let Ok(src) = &s {
            failed_scan_decisions(src, &regs, &mut candidates);
        }
        if let Ok(src) = &ex {
            expiring_exemption_decisions(src, &all_visible, now, &mut candidates);
        }
        let coords: Vec<PackageId> = candidates
            .iter()
            .map(|d| PackageId::new(&d.registry, &d.name, &d.version))
            .collect();
        let held = coords_held(&hot, id, &coords, Action::QuarantineRead).await;
        decisions.extend(
            candidates
                .into_iter()
                .zip(held)
                .filter(|(_, h)| *h)
                .map(|(d, _)| d),
        );
        (
            Some(holds_block(&h, &regs)),
            Some(scanners_block(&hot, &s, &regs).await),
            Some(coverage_block(&hot, modes.as_ref(), &all_visible).await),
        )
    } else {
        (None, None, None)
    };

    let flags = if can_f {
        let regs = visible(&hot, id, &all, Action::FlagsRead).await;
        let src = flags_source(&cache, advisories).await;
        if src.is_err() {
            unknown.push("flags".to_owned());
        }
        if let Ok(src) = &src {
            let candidates: Vec<DecisionItem> = src
                .pulled_after
                .iter()
                .filter(|r| regs.contains(&r.registry))
                .map(|r| DecisionItem {
                    kind: "pulled_after_flag".into(),
                    registry: r.registry.clone(),
                    name: r.package_name.clone(),
                    version: r.version.clone(),
                    at: r.last_pull,
                    reason_code: None,
                    pulls_after: Some(r.pulls_after),
                    expires_at: None,
                })
                .collect();
            let coords: Vec<PackageId> = candidates
                .iter()
                .map(|d| PackageId::new(&d.registry, &d.name, &d.version))
                .collect();
            let held = coords_held(&hot, id, &coords, Action::FlagsRead).await;
            decisions.extend(
                candidates
                    .into_iter()
                    .zip(held)
                    .filter(|(_, h)| *h)
                    .map(|(d, _)| d),
            );
        }
        Some(flags_block(&src, &regs))
    } else {
        None
    };

    decisions.sort_by(by_urgency);
    let total = decisions.len() as u64 + unlisted_hard_blocks;
    decisions.truncate(DECISIONS_SHOWN);
    Ok(HttpResponse::Ok().json(SecurityOverview {
        computed_at: now,
        holds,
        flags,
        scanners,
        coverage,
        decisions: DecisionList {
            total,
            items: decisions,
            unknown,
        },
    }))
}

/// Most urgent first: hard-block holds and pulls after a flag (newest
/// first), then exemptions (soonest ending first, as instance security), then
/// failed scans (newest first). An exemption's `at` is its future end, so one
/// newest-first sort would put every exemption on top.
fn by_urgency(a: &DecisionItem, b: &DecisionItem) -> std::cmp::Ordering {
    let rank = |d: &DecisionItem| match d.kind.as_str() {
        "hold_on_hard_block" | "pulled_after_flag" => 0,
        "exemption_expiring" => 1,
        _ => 2,
    };
    rank(a).cmp(&rank(b)).then_with(|| {
        if rank(a) == 1 {
            a.at.cmp(&b.at)
        } else {
            b.at.cmp(&a.at)
        }
    })
}

// ── trends ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TrendsQuery {
    /// `7d`, `30d` or `90d`; anything else reads as `30d`.
    pub window: Option<String>,
    /// One registry; absent sums every registry the caller may see.
    pub registry: Option<String>,
}

/// One UTC day. `null` is a gap — no data for that day — never a zero.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct TrendDay {
    pub day: NaiveDate,
    pub allowed: Option<u64>,
    pub warned: Option<u64>,
    pub quarantined: Option<u64>,
    pub denied: Option<u64>,
    pub findings_critical: Option<u64>,
    pub findings_high: Option<u64>,
    pub findings_medium: Option<u64>,
    pub findings_low: Option<u64>,
    pub findings_unknown: Option<u64>,
    pub refusals: Option<u64>,
    pub refusals_by_code: Option<BTreeMap<String, u64>>,
    /// Only with `flags:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags_opened: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags_revoked: Option<u64>,
    pub jobs_done: Option<u64>,
    pub jobs_failed: Option<u64>,
}

/// `GET /api/v1/admin/security/trends`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecurityTrends {
    pub window_days: u32,
    /// `[stats] history_enabled`: `false` records nothing.
    pub history_enabled: bool,
    /// The oldest day with a figure (an all-`null` row is not one), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_starts: Option<NaiveDate>,
    /// Every day of the window, oldest first.
    pub days: Vec<TrendDay>,
}

fn window_days(raw: Option<&str>) -> u32 {
    match raw {
        Some("7d") => 7,
        Some("90d") => 90,
        _ => 30,
    }
}

/// One day's figure over `started`, the registries that had begun that
/// series by then: a gap (`None`) as soon as one of them has no figure (or no
/// row), never a partial sum drawn as the whole; no registry at all is a gap.
fn total(
    started: &[&str],
    rows: &HashMap<&str, &SecurityDailyRow>,
    pick: impl Fn(&SecurityDailyRow) -> Option<u64>,
) -> Option<u64> {
    if started.is_empty() {
        return None;
    }
    started
        .iter()
        .map(|r| rows.get(r).and_then(|row| pick(row)))
        .sum()
}

/// The registries whose series had begun by `day`.
fn started<'a>(from: &HashMap<&'a str, NaiveDate>, day: NaiveDate) -> Vec<&'a str> {
    from.iter()
        .filter(|(_, d)| **d <= day)
        .map(|(r, _)| *r)
        .collect()
}

fn has_events(e: &DailyEvents) -> bool {
    e.refusals.is_some()
        || e.flags_opened.is_some()
        || e.flags_revoked.is_some()
        || e.jobs_done.is_some()
        || e.jobs_failed.is_some()
}

/// Daily holds, findings, refusals, flags and scans, summed over the
/// registries the caller may see (RFC 0050 §4.2 *Trends*).
#[utoipa::path(
    get,
    path = "/api/v1/admin/security/trends",
    tag = "security",
    params(TrendsQuery),
    responses(
        (status = 200, description = "One entry per UTC day of the window", body = SecurityTrends),
        (status = 403, description = "`quarantine:read` at the instance tier required"),
        (status = 404, description = "An unknown registry, or one sealed for the caller"),
        (status = 503, description = "The history store did not answer"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/security/trends")]
pub async fn security_trends(
    query: web::Query<TrendsQuery>,
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    history_enabled: Option<web::Data<SecurityHistoryEnabled>>,
    cache: Option<web::Data<SourceCache>>,
) -> Result<HttpResponse, AppError> {
    let id = &identity.0;
    if !holds_at(&hot, id, None, Action::QuarantineRead).await {
        return Err(forbidden());
    }
    let with_flags = holds_at(&hot, id, None, Action::FlagsRead).await;
    let all: Vec<String> = hot.read().await.registries.keys().cloned().collect();
    let mut regs = visible(&hot, id, &all, Action::QuarantineRead).await;
    if let Some(r) = &query.registry {
        if !regs.contains(r) {
            return Err(AppError::not_found(format!("unknown registry '{r}'")));
        }
        regs = BTreeSet::from([r.clone()]);
    }
    let days = window_days(query.window.as_deref());
    let rows = daily_source(&cache_of(cache), &hot).await.map_err(|e| {
        AppError::service_unavailable(format!("the history store did not answer: {e}"))
    })?;
    let today = Utc::now().date_naive();
    let from = today - chrono::Duration::days(i64::from(days) - 1);
    let mut by_day: BTreeMap<NaiveDate, TrendDay> = (0..days)
        .map(|i| {
            let day = from + chrono::Duration::days(i64::from(i));
            (
                day,
                TrendDay {
                    day,
                    ..Default::default()
                },
            )
        })
        .collect();
    let mine: Vec<&SecurityDailyRow> = rows.iter().filter(|r| regs.contains(&r.registry)).collect();
    // Rule: a registry counts in a series group (state, events) from its first
    // day with a figure there — before it, it was not configured yet, so the
    // backfill's empty rows of a new registry never blank the others' past —
    // and from then on a day it has no figure for is a gap.
    let mut state_from: HashMap<&str, NaiveDate> = HashMap::new();
    let mut events_from: HashMap<&str, NaiveDate> = HashMap::new();
    for r in &mine {
        if r.state.is_some() {
            state_from.entry(&r.registry).or_insert(r.day);
        }
        if has_events(&r.events) {
            events_from.entry(&r.registry).or_insert(r.day);
        }
    }
    // The first day with real data, over the caller's registries only (a
    // sealed one's oldest day is not theirs): an all-NULL row is no history.
    let history_starts = state_from
        .values()
        .chain(events_from.values())
        .min()
        .copied();
    let mut per_day: BTreeMap<NaiveDate, HashMap<&str, &SecurityDailyRow>> = BTreeMap::new();
    for row in &mine {
        per_day
            .entry(row.day)
            .or_default()
            .insert(row.registry.as_str(), row);
    }
    for (day, t) in by_day.iter_mut() {
        let empty = HashMap::new();
        let rows = per_day.get(day).unwrap_or(&empty);
        let (with_state, with_events) = (started(&state_from, *day), started(&events_from, *day));
        let state =
            |f: fn(&DailyState) -> u64| total(&with_state, rows, |r| r.state.as_ref().map(f));
        let events =
            |f: fn(&DailyEvents) -> Option<u64>| total(&with_events, rows, |r| f(&r.events));
        t.allowed = state(|s| s.allowed);
        t.warned = state(|s| s.warned);
        t.quarantined = state(|s| s.quarantined);
        t.denied = state(|s| s.denied);
        t.findings_critical = state(|s| s.findings_critical);
        t.findings_high = state(|s| s.findings_high);
        t.findings_medium = state(|s| s.findings_medium);
        t.findings_low = state(|s| s.findings_low);
        t.findings_unknown = state(|s| s.findings_unknown);
        t.refusals = events(|e| e.refusals);
        t.refusals_by_code = (!with_events.is_empty())
            .then(|| {
                with_events
                    .iter()
                    .map(|r| {
                        rows.get(r)
                            .and_then(|row| row.events.refusals_by_code.as_ref())
                    })
                    .collect::<Option<Vec<_>>>()
            })
            .flatten()
            .map(|maps| {
                let mut into = BTreeMap::new();
                for (code, n) in maps.into_iter().flatten() {
                    *into.entry(code.clone()).or_default() += n;
                }
                into
            });
        if with_flags {
            t.flags_opened = events(|e| e.flags_opened);
            t.flags_revoked = events(|e| e.flags_revoked);
        }
        t.jobs_done = events(|e| e.jobs_done);
        t.jobs_failed = events(|e| e.jobs_failed);
    }
    Ok(HttpResponse::Ok().json(SecurityTrends {
        window_days: days,
        history_enabled: history_enabled.is_none_or(|h| h.0),
        history_starts,
        days: by_day.into_values().collect(),
    }))
}

/// `[stats] history_enabled`, as app data for the trends page's banner.
pub struct SecurityHistoryEnabled(pub bool);

// ── coverage ─────────────────────────────────────────────────────────────────

/// A scanner of a registry's profile.
#[derive(Debug, Serialize, ToSchema)]
pub struct ScannerRef {
    pub name: String,
    pub required: bool,
}

/// One registry's protection (RFC 0050 §4.2 *Coverage*).
#[derive(Debug, Serialize, ToSchema)]
pub struct CoverageRow {
    pub registry: String,
    /// The registry type (`npm`, `pypi`, …).
    pub kind: String,
    /// `proxy`, `local` or `hybrid`.
    pub mode: String,
    /// Has a `[security]` profile.
    pub protected: bool,
    /// The profile's `mode`: `block` or `warn`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_mode: Option<String>,
    pub cve_gate: bool,
    pub license_gate: bool,
    /// The profile's `max_severity`: refused at and above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_age_secs: Option<u64>,
    /// `mature_age_secs`: unscanned versions served after this; `0` never.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serve_unscanned_after_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rescan_interval_secs: Option<u64>,
    /// What an unrated finding counts as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unrated_as: Option<String>,
    /// Only with `config:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanners: Option<Vec<ScannerRef>>,
    /// Allowed + warned + denied verdicts. `null` when the verdict store did
    /// not answer: unknown, never a zero (§4.2).
    pub scanned: Option<u64>,
    /// Quarantined verdicts; `null` as `scanned`.
    pub waiting: Option<u64>,
    /// Failed scan jobs; `null` when the scan queue did not answer.
    pub failed: Option<u64>,
}

/// `GET /api/v1/admin/security/coverage`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CoverageList {
    pub computed_at: DateTime<Utc>,
    pub registries: Vec<CoverageRow>,
}

async fn coverage_rows(
    hot: &HotConfigLock,
    modes: Option<&web::Data<crate::RegistryModeMap>>,
    cache: &SourceCache,
    regs: &BTreeSet<String>,
    with_scanners: bool,
) -> Vec<CoverageRow> {
    let (counts, failed) = tokio::join!(
        holds_source(cache, hot),
        scanners_source(cache, hot, SOURCE_TTL)
    );
    let hot = hot.read().await;
    regs.iter()
        .map(|r| {
            let policy = hot.security.get(r);
            let n = |pred: &dyn Fn(VerdictState) -> bool| {
                counts.as_ref().ok().map(|c| {
                    c.counts
                        .iter()
                        .filter(|(reg, s, _)| reg == r && pred(*s))
                        .map(|(_, _, n)| n)
                        .sum()
                })
            };
            CoverageRow {
                registry: r.clone(),
                kind: hot
                    .registries
                    .get(r)
                    .map(|c| c.registry_type().to_owned())
                    .unwrap_or_default(),
                mode: mode_of(modes, r),
                protected: policy.is_some(),
                security_mode: policy.map(|p| {
                    match p.mode {
                        batlehub_core::entities::SecurityMode::Block => "block",
                        batlehub_core::entities::SecurityMode::Warn => "warn",
                    }
                    .to_owned()
                }),
                cve_gate: has_rule(&hot, r, "cve_gate"),
                license_gate: has_rule(&hot, r, "license_gate"),
                deny_at: policy.map(|p| p.max_severity.as_str().to_owned()),
                min_age_secs: policy.map(|p| p.min_age.as_secs()),
                serve_unscanned_after_secs: policy.map(|p| p.mature_age.as_secs()),
                rescan_interval_secs: policy.and_then(|p| p.rescan_interval.map(|d| d.as_secs())),
                unrated_as: policy.map(|p| p.unknown_severity.as_str().to_owned()),
                scanners: with_scanners.then(|| {
                    policy
                        .map(|p| {
                            p.scanners
                                .iter()
                                .map(|s| ScannerRef {
                                    name: s.clone(),
                                    required: p.required_scanners.contains(s),
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                }),
                scanned: n(&|s| s != VerdictState::Quarantined),
                waiting: n(&|s| s == VerdictState::Quarantined),
                failed: failed
                    .as_ref()
                    .ok()
                    .map(|f| f.failed_by_registry.get(r).copied().unwrap_or(0)),
            }
        })
        .collect()
}

/// Every registry the caller may see, protected or not, and how (RFC 0050
/// §4.2 *Coverage*). Scanner names need `config:read`.
#[utoipa::path(
    get,
    path = "/api/v1/admin/security/coverage",
    tag = "security",
    responses(
        (status = 200, description = "One row per visible registry", body = CoverageList),
        (status = 403, description = "`quarantine:read` at the instance tier required"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/security/coverage")]
pub async fn security_coverage(
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    modes: Option<web::Data<crate::RegistryModeMap>>,
    cache: Option<web::Data<SourceCache>>,
) -> Result<HttpResponse, AppError> {
    let id = &identity.0;
    if !holds_at(&hot, id, None, Action::QuarantineRead).await {
        return Err(forbidden());
    }
    let with_scanners = holds_at(&hot, id, None, Action::ConfigRead).await;
    let all: Vec<String> = hot.read().await.registries.keys().cloned().collect();
    let regs = visible(&hot, id, &all, Action::QuarantineRead).await;
    let registries =
        coverage_rows(&hot, modes.as_ref(), &cache_of(cache), &regs, with_scanners).await;
    Ok(HttpResponse::Ok().json(CoverageList {
        computed_at: Utc::now(),
        registries,
    }))
}

/// One registry's protection. A registry sealed for the caller answers as an
/// unknown one: `404`.
#[utoipa::path(
    get,
    path = "/api/v1/admin/security/coverage/{registry}",
    tag = "security",
    params(("registry" = String, Path, description = "Registry name")),
    responses(
        (status = 200, description = "The registry's protection", body = CoverageRow),
        (status = 403, description = "`quarantine:read` at the instance tier required"),
        (status = 404, description = "An unknown registry, or one sealed for the caller"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/security/coverage/{registry}")]
pub async fn security_coverage_registry(
    path: web::Path<String>,
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    modes: Option<web::Data<crate::RegistryModeMap>>,
    cache: Option<web::Data<SourceCache>>,
) -> Result<HttpResponse, AppError> {
    let id = &identity.0;
    if !holds_at(&hot, id, None, Action::QuarantineRead).await {
        return Err(forbidden());
    }
    let registry = path.into_inner();
    let known = hot.read().await.registries.contains_key(&registry);
    if !known || !holds_at(&hot, id, Some(&registry), Action::QuarantineRead).await {
        return Err(AppError::not_found(format!(
            "unknown registry '{registry}'"
        )));
    }
    let with_scanners = holds_at(&hot, id, None, Action::ConfigRead).await;
    let regs = BTreeSet::from([registry]);
    let mut rows =
        coverage_rows(&hot, modes.as_ref(), &cache_of(cache), &regs, with_scanners).await;
    let row = rows
        .pop()
        .ok_or_else(|| AppError::not_found("unknown registry"))?;
    Ok(HttpResponse::Ok().json(row))
}

// ── scanners & worker ────────────────────────────────────────────────────────

/// A worker as its last heartbeat recorded it.
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkerRow {
    pub worker_id: String,
    pub last_seen: DateTime<Utc>,
    /// Empty = every registry.
    pub registries: Vec<String>,
    pub live: bool,
}

/// The remaining scan queue, over the caller's registries.
#[derive(Debug, Serialize, ToSchema)]
pub struct QueueSummary {
    pub queued: u64,
    pub deferred: u64,
    pub deferred_over_day: u64,
    pub failed: u64,
}

/// One configured scanner.
#[derive(Debug, Serialize, ToSchema)]
pub struct ScannerRow {
    pub name: String,
    pub state: ScannerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_at: Option<DateTime<Utc>>,
    /// Only with `findings:read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_until: Option<DateTime<Utc>>,
}

/// `GET /api/v1/admin/security/scanners`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ScannerHealth {
    pub computed_at: DateTime<Utc>,
    pub workers: Vec<WorkerRow>,
    pub queue: QueueSummary,
    pub scanners: Vec<ScannerRow>,
}

/// Live workers, the remaining queue and each scanner's last outcome (RFC
/// 0050 §4.2 *Scanners & worker*). The error class needs `findings:read`.
#[utoipa::path(
    get,
    path = "/api/v1/admin/security/scanners",
    tag = "security",
    responses(
        (status = 200, description = "Workers, queue and scanners", body = ScannerHealth),
        (status = 403, description = "`quarantine:read` at the instance tier required"),
        (status = 503, description = "The scan queue did not answer"),
    ),
    security(("bearer_token" = [])),
)]
#[get("/api/v1/admin/security/scanners")]
pub async fn security_scanners(
    identity: AuthIdentity,
    hot: web::Data<HotConfigLock>,
    cache: Option<web::Data<SourceCache>>,
) -> Result<HttpResponse, AppError> {
    let id = &identity.0;
    if !holds_at(&hot, id, None, Action::QuarantineRead).await {
        return Err(forbidden());
    }
    let with_class = holds_at(&hot, id, None, Action::FindingsRead).await;
    let protected = protected_registries(&hot).await;
    let regs = visible(&hot, id, &protected, Action::QuarantineRead).await;
    let src = scanners_source(&cache_of(cache), &hot, SCANNERS_TTL)
        .await
        .map_err(|e| {
            AppError::service_unavailable(format!("the scan queue did not answer: {e}"))
        })?;
    let block = scanners_block(&hot, &Ok(Arc::clone(&src)), &regs).await;
    let now = Utc::now();
    let rows: HashMap<&str, &ScannerStatus> =
        src.status.iter().map(|s| (s.scanner.as_str(), s)).collect();
    let scanners = configured_scanners(&hot, &regs)
        .await
        .into_keys()
        .map(|name| {
            let row = rows.get(name.as_str()).copied();
            ScannerRow {
                state: rules::scanner_state(row, now),
                last_ok_at: row.and_then(|r| r.last_ok_at),
                last_error_at: row.and_then(|r| r.last_error_at),
                last_error_class: row
                    .and_then(|r| r.last_error_class.clone())
                    .filter(|_| with_class),
                // A lapsed breaker is closed: the overview reads it the same way.
                unavailable_until: row.and_then(|r| r.unavailable_until).filter(|t| *t > now),
                name,
            }
        })
        .collect();
    Ok(HttpResponse::Ok().json(ScannerHealth {
        computed_at: now,
        workers: visible_workers(&src.workers, &regs)
            .into_iter()
            .map(|w| WorkerRow {
                live: rules::worker_is_live(&w, now),
                worker_id: w.worker_id,
                last_seen: w.last_seen,
                registries: w.registries,
            })
            .collect(),
        queue: QueueSummary {
            queued: block.queued,
            deferred: block.deferred,
            deferred_over_day: block.deferred_over_day,
            failed: block.failed,
        },
        scanners,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// T7: concurrent misses run one load; a fresh entry serves; a disabled
    /// cache reads again once the first read finished.
    #[tokio::test]
    async fn single_flight_and_bypass() {
        for enabled in [true, false] {
            let cache = Arc::new(SourceCache::new(enabled));
            let calls = Arc::new(AtomicU32::new(0));
            let load = |calls: Arc<AtomicU32>| async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok::<u32, CoreError>(7)
            };
            let (a, b) = tokio::join!(
                cache.get("k", SOURCE_TTL, load(Arc::clone(&calls))),
                cache.get("k", SOURCE_TTL, load(Arc::clone(&calls))),
            );
            assert_eq!((*a.unwrap(), *b.unwrap()), (7, 7));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "enabled={enabled}");
            cache
                .get("k", SOURCE_TTL, load(Arc::clone(&calls)))
                .await
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), if enabled { 1 } else { 2 });
        }
    }

    /// A slow source turns into an error string (→ `unknown`), not a hang.
    #[tokio::test]
    async fn a_slow_source_is_unknown() {
        let cache = SourceCache::new(true);
        let r = cache
            .get("slow", SOURCE_TTL, async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                Ok::<u32, CoreError>(1)
            })
            .await;
        assert!(r.unwrap_err().contains("2 s"));
        // A failed entry is not served again.
        let ok = cache
            .get("slow", SOURCE_TTL, async { Ok::<u32, CoreError>(2) })
            .await;
        assert_eq!(*ok.unwrap(), 2);
    }

    #[test]
    fn window_falls_back_to_30_days() {
        assert_eq!(window_days(Some("7d")), 7);
        assert_eq!(window_days(Some("90d")), 90);
        assert_eq!(window_days(Some("14d")), 30);
        assert_eq!(window_days(None), 30);
    }
}
