//! Where verdicts live and how scan work is handed out (RFC 0018 §6.1).

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::entities::{PackageId, ScanJob, ScanTrigger, Verdict, VerdictState};
use crate::error::CoreError;

/// `artifact_verdicts` and `artifact_findings`.
#[async_trait]
pub trait VerdictRepository: Send + Sync {
    /// Replace the verdict for `verdict.package`, findings included.
    async fn upsert(&self, verdict: &Verdict) -> Result<(), CoreError>;

    async fn get(&self, package: &PackageId) -> Result<Option<Verdict>, CoreError>;

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
}

/// The count of open jobs, per registry and trigger — the
/// `batlehub_scan_jobs_queued` gauge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedCount {
    pub registry: String,
    pub trigger: ScanTrigger,
    pub count: u64,
}

/// `scan_jobs` (RFC 0018 §5.4): jobs are leased, not consumed.
///
/// A row carries `leased_until` and `attempts`; a worker heartbeats while
/// scanning; a lease that expires returns the job to the queue, and after
/// `max_attempts` the job is failed. Idempotent on the coordinate: a second
/// enqueue for a version with an open job is a no-op.
#[async_trait]
pub trait ScanQueue: Send + Sync {
    /// Enqueue, unless an open job for this coordinate already exists.
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

    /// The job is done; the row is closed.
    async fn complete(&self, job_id: uuid::Uuid) -> Result<(), CoreError>;

    /// The attempt failed; the lease is released so another attempt can
    /// happen, or the row is closed once `attempts` reached `max_attempts`.
    async fn fail(&self, job_id: uuid::Uuid, error: &str) -> Result<(), CoreError>;

    /// Jobs whose attempts were exhausted and nobody closed — the ones the
    /// worker turns into `SCANNER_ERROR` verdicts.
    async fn exhausted(&self, max_attempts: u32, n: u32) -> Result<Vec<ScanJob>, CoreError>;

    async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError>;

    /// Whether this process leads the scheduled work keyed by `key` (RFC
    /// 0018 §6.3: one rescan timer per estate, elected with a PostgreSQL
    /// advisory lock). `true` while the lock is held — a process that got
    /// it keeps it until it exits — and `false` for every other process.
    /// The in-memory queue is one process by construction and always leads.
    async fn try_lead(&self, key: i64) -> Result<bool, CoreError>;
}

/// `worker_heartbeats`: which workers are alive, so the proxy can say when
/// none is (RFC 0018 §4.3).
#[async_trait]
pub trait WorkerRegistry: Send + Sync {
    async fn heartbeat(&self, worker_id: &str, registries: &[String]) -> Result<(), CoreError>;

    /// Workers seen within `within_secs`.
    async fn live_count(&self, within_secs: u64) -> Result<u64, CoreError>;
}
