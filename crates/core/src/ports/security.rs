//! Where verdicts live and how scan work is handed out (RFC 0018 §6.1).

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::entities::{
    FindingRow, FindingsFilter, PackageId, ReasonCode, ScanJob, ScanJobFilter, ScanTrigger,
    Verdict, VerdictState,
};
use crate::error::CoreError;

/// `artifact_verdicts` and `artifact_findings`.
#[async_trait]
pub trait VerdictRepository: Send + Sync {
    /// Replace the verdict for `verdict.package`, findings included.
    async fn upsert(&self, verdict: &Verdict) -> Result<(), CoreError>;

    async fn get(&self, package: &PackageId) -> Result<Option<Verdict>, CoreError>;

    /// The verdicts of `coords`, across registries, findings **not** loaded
    /// — the batch a list page reads its scan statuses from (RFC 0044 §5.1).
    /// One entry per coordinate, in order. The default asks one at a time;
    /// the stores override it with one statement.
    async fn get_many(&self, coords: &[PackageId]) -> Result<Vec<Option<Verdict>>, CoreError> {
        let mut out = Vec::with_capacity(coords.len());
        for c in coords {
            out.push(self.get(c).await?);
        }
        Ok(out)
    }

    /// Every verdict recorded for one package — what the listing filter reads
    /// to hide held versions (RFC 0018 §4.2 *Listings*). Findings may be
    /// omitted: the filter reads states and clocks, not content.
    async fn list_for_package(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<Verdict>, CoreError>;

    async fn list_by_state(
        &self,
        registry: &str,
        state: VerdictState,
        limit: u64,
    ) -> Result<Vec<Verdict>, CoreError>;

    /// The coordinates of `registry` whose last scan is older than `before`
    /// (or that were never scanned), oldest first, at most `limit` — what
    /// the rescan scheduler queues (RFC 0018 phase 4). Findings are not
    /// loaded: the scheduler needs the coordinate, not the content.
    async fn list_due_for_rescan(
        &self,
        registry: &str,
        before: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<PackageId>, CoreError>;

    /// The findings list (RFC 0044 §6.2): one page of findings inside
    /// `filter.registries`, most severe first, with the total before paging.
    /// `raw` is not loaded. The default refuses, as `list_jobs` does.
    async fn list_findings(
        &self,
        _filter: &FindingsFilter,
        _limit: u64,
        _offset: u64,
    ) -> Result<(Vec<FindingRow>, u64), CoreError> {
        Err(CoreError::Other(anyhow::anyhow!(
            "this verdict store cannot list findings"
        )))
    }

    /// Per registry, how many versions hold a finding of severity
    /// `unknown` — release N's upgrade count (RFC 0041 §6.8). `None` when
    /// another replica is counting.
    async fn count_unrated_by_registry(&self) -> Result<Option<Vec<(String, u64)>>, CoreError> {
        Ok(Some(Vec::new()))
    }

    /// How many verdicts each registry holds in each state — the security
    /// console's summary (RFC 0042 §5.2). The default refuses rather than
    /// answering empty: a backend that never counted must not read as
    /// "nothing is held".
    async fn count_by_registry_and_state(
        &self,
    ) -> Result<Vec<(String, VerdictState, u64)>, CoreError> {
        Err(CoreError::NotSupported(
            "this verdict store cannot count by state".to_owned(),
        ))
    }

    /// Per registry with a held version, what the console's *Holds* line
    /// counts (RFC 0050 §4.2), over every held row: never from a page. A
    /// `SCAN_PENDING` hold evaluated before `pending_before` is over the hour.
    async fn hold_counts(
        &self,
        _pending_before: DateTime<Utc>,
    ) -> Result<Vec<HoldCounts>, CoreError> {
        Err(CoreError::NotSupported(
            "this verdict store cannot count holds".to_owned(),
        ))
    }

    /// One page of verdicts and the exact count of every match, so a list of
    /// thousands can be walked to its end. Findings loaded.
    async fn page_verdicts(&self, _page: &VerdictPage) -> Result<(Vec<Verdict>, u64), CoreError> {
        Err(CoreError::NotSupported(
            "this verdict store cannot page verdicts".to_owned(),
        ))
    }

    /// Denied or quarantined verdicts carrying `code`, across registries,
    /// newest first, at most `limit`; findings **not** loaded.
    async fn list_held_with_code(
        &self,
        _code: ReasonCode,
        _limit: u64,
    ) -> Result<Vec<Verdict>, CoreError> {
        Err(CoreError::NotSupported(
            "this verdict store cannot list holds by code".to_owned(),
        ))
    }
}

/// One registry's held versions as the *Holds* line counts them
/// ([`crate::services::security_overview`] holds the per-verdict rules).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HoldCounts {
    pub registry: String,
    /// Quarantined for a reason other than `MIN_AGE_NOT_MET`/`SCAN_PENDING`.
    pub quarantined_blocking: u64,
    /// Held for a malware code (`KNOWN_MALICIOUS`, `MALWARE_SIGNAL`, `SOC_VERDICT`).
    pub malware: u64,
    /// Quarantined only for `MIN_AGE_NOT_MET` or `SCAN_PENDING`.
    pub waiting: u64,
    /// Quarantined for `SCAN_PENDING`, evaluated before the cutoff.
    pub pending_over_hour: u64,
    /// Held for `SOC_VERDICT`: a SOC's hard block, counted exactly whatever
    /// their number (a mass compromise holds thousands at once).
    pub soc_verdict: u64,
}

/// One page of `GET /admin/verdicts`: the verdicts of `registries` in
/// `states`, optionally carrying `reason`; denied first, then quarantined,
/// warned, allowed, each newest evaluation first.
#[derive(Debug, Clone, Default)]
pub struct VerdictPage {
    pub registries: Vec<String>,
    pub states: Vec<VerdictState>,
    /// A reason code as stored (`SOC_VERDICT`, `KNOWN_MALICIOUS`, …).
    pub reason: Option<String>,
    pub limit: u64,
    pub offset: u64,
}

/// The count of open jobs, per registry and trigger — the
/// `batlehub_scan_jobs_queued` gauge (leasable rows, what the worker HPA
/// scales on) and `batlehub_scan_jobs_deferred` (rows waiting out an
/// advisory database's outage, RFC 0041 decision 44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedCount {
    pub registry: String,
    pub trigger: ScanTrigger,
    pub count: u64,
    pub deferred: u64,
    /// Open jobs deferred for longer than `DEFERRED_ATTENTION`, counted in
    /// the store so the figure is exact on any queue size.
    pub deferred_over_day: u64,
}

/// `scan_jobs` (RFC 0018 §5.4): jobs are leased, not consumed.
///
/// A row carries `leased_until` and `attempts`; a worker heartbeats while
/// scanning; a lease that expires returns the job to the queue, and after
/// `max_attempts` the job is failed. Idempotent on the coordinate: a second
/// enqueue for a version with an open job is a no-op.
#[async_trait]
pub trait ScanQueue: Send + Sync {
    /// Enqueue, unless an open job for this coordinate already exists — in
    /// which case a job that outranks it promotes it: its trigger and
    /// priority rise and a deferral is cleared, so a webhook or a flag is
    /// not lost behind a deferred rescan (RFC 0041 decision 44).
    /// Returns whether a row was created.
    async fn enqueue(
        &self,
        package: &PackageId,
        published_at: Option<DateTime<Utc>>,
        trigger: ScanTrigger,
    ) -> Result<bool, CoreError>;

    /// Lease up to `n` jobs for `worker_id`, in priority order, for `lease`
    /// seconds. `registries` empty means any; else only those names.
    async fn lease(
        &self,
        worker_id: &str,
        registries: &[String],
        n: u32,
        lease_secs: u64,
        max_attempts: u32,
    ) -> Result<Vec<ScanJob>, CoreError>;

    /// Extend the lease of a job still being worked on.
    async fn heartbeat(&self, job_id: uuid::Uuid, lease_secs: u64) -> Result<(), CoreError>;

    /// The job is done; the row is closed. With `worker`, only while that
    /// worker still holds the lease — a stale worker cannot close a row
    /// another now holds. `None` is the exhausted-jobs sweep, which closes
    /// rows nobody holds.
    async fn complete(&self, job_id: uuid::Uuid, worker: Option<&str>) -> Result<(), CoreError>;

    /// Close a job whose attempts ran out, nobody holding it, recording
    /// `last_error` — [`crate::entities::EXHAUSTED_PREFIX`] and the last
    /// error, which is how a reader tells a failed scan from a finished one
    /// (RFC 0044 decision 34). The default closes without the record.
    async fn complete_exhausted(
        &self,
        job_id: uuid::Uuid,
        _last_error: &str,
    ) -> Result<(), CoreError> {
        self.complete(job_id, None).await
    }

    /// For each of `coords`, in order: its open job and its most recently
    /// closed one, across registries, in one read (RFC 0044 §5.1). The
    /// default refuses, so a queue that cannot answer makes the status
    /// unavailable rather than wrong.
    async fn jobs_for(
        &self,
        _coords: &[PackageId],
    ) -> Result<Vec<(Option<ScanJob>, Option<ScanJob>)>, CoreError> {
        Err(CoreError::Other(anyhow::anyhow!(
            "this scan queue cannot list jobs per coordinate"
        )))
    }

    /// The Scans page (RFC 0044 §6.1): the jobs of `registries` matching
    /// `filter`, with the total before paging. Open rows: scanning first,
    /// then oldest first. Failed: most recently closed first. An empty
    /// `registries` lists nothing. The default refuses, as `jobs_for` does.
    async fn list_jobs(
        &self,
        _registries: &[String],
        _filter: ScanJobFilter,
        _limit: u64,
        _offset: u64,
    ) -> Result<(Vec<ScanJob>, u64), CoreError> {
        Err(CoreError::Other(anyhow::anyhow!(
            "this scan queue cannot list jobs"
        )))
    }

    /// The attempt failed; the lease is released so another attempt can
    /// happen, or the row is closed once `attempts` reached `max_attempts`.
    /// Only while `worker` holds the lease.
    async fn fail(&self, job_id: uuid::Uuid, worker: &str, error: &str) -> Result<(), CoreError>;

    /// Put a leased job back until `not_before`, giving its attempt back and
    /// anchoring the deferral's start (RFC 0041 decision 44). Only while
    /// `worker` holds the lease. The default does nothing: a test stub that
    /// never defers.
    async fn defer(
        &self,
        _job_id: uuid::Uuid,
        _worker: &str,
        _not_before: DateTime<Utc>,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    /// Jobs whose attempts were exhausted and nobody closed — the ones the
    /// worker turns into `SCANNER_ERROR` verdicts.
    async fn exhausted(&self, max_attempts: u32, n: u32) -> Result<Vec<ScanJob>, CoreError>;

    async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError>;

    /// Wait up to `max` for work: an idle worker's sleep between passes.
    ///
    /// The default just sleeps. The Postgres queue returns early when *this
    /// process* enqueues, so an embedded worker starts a scan at once however
    /// long its idle backoff has grown; a worker in another process still
    /// waits out `max`.
    async fn wait_for_work(&self, max: std::time::Duration) {
        tokio::time::sleep(max).await;
    }

    /// Whether this process leads the scheduled work keyed by `key` (RFC
    /// 0018 §6.3: one rescan timer per estate, elected with a PostgreSQL
    /// advisory lock). `true` while the lock is held — a process that got
    /// it keeps it until it exits — and `false` for every other process.
    /// The in-memory queue is one process by construction and always leads.
    async fn try_lead(&self, key: i64) -> Result<bool, CoreError>;
}

/// One row of `worker_heartbeats`: a worker, when it last beat, and the
/// registries it scans (empty = every registry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHeartbeat {
    pub worker_id: String,
    pub last_seen: DateTime<Utc>,
    pub registries: Vec<String>,
}

/// `worker_heartbeats`: which workers are alive, so the proxy can say when
/// none is (RFC 0018 §4.3).
#[async_trait]
pub trait WorkerRegistry: Send + Sync {
    async fn heartbeat(&self, worker_id: &str, registries: &[String]) -> Result<(), CoreError>;

    /// Workers seen within `within_secs`.
    async fn live_count(&self, within_secs: u64) -> Result<u64, CoreError>;

    /// Every recorded worker, newest beat first — the security console's
    /// *Scanners & worker* page (RFC 0050 §6.1). The default answers none,
    /// so a test double that never lists stays as it is.
    async fn workers(&self) -> Result<Vec<WorkerHeartbeat>, CoreError> {
        Ok(Vec::new())
    }
}

/// One scanner's last outcome, as the worker records it (RFC 0050 §6.1,
/// decision 28): times and an error **class**, never error text, so no
/// sealed path reaches a reader through it (T5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannerStatus {
    pub scanner: String,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub last_error_at: Option<DateTime<Utc>>,
    /// `ScannerError::class()`: `timeout`, `upstream`, `rate_limited`, …
    pub last_error_class: Option<String>,
    /// While the scanner's breaker is open (the OSV breaker).
    pub unavailable_until: Option<DateTime<Utc>>,
}

/// `scanner_status`: one row per scanner, written by the worker.
#[async_trait]
pub trait ScannerStatusRepository: Send + Sync {
    /// Merge `status` into the scanner's row. Each timestamp only moves
    /// forward (`GREATEST`), so two workers writing out of order never move
    /// one back; the error class follows the newer error.
    async fn record(&self, status: &ScannerStatus) -> Result<(), CoreError>;

    /// Every row, by scanner name.
    async fn list(&self) -> Result<Vec<ScannerStatus>, CoreError>;
}
