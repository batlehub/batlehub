//! In-memory verdicts, scan queue and worker registry (RFC 0018), for tests
//! and for a deployment with no database.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

use batlehub_core::entities::{
    FindingRow, FindingsFilter, PackageId, ScanJob, ScanJobFilter, ScanTrigger, Verdict,
    VerdictState, EXHAUSTED_PREFIX,
};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{QueuedCount, ScanQueue, VerdictRepository, WorkerRegistry};

#[derive(Default)]
pub struct InMemoryVerdictRepository {
    rows: RwLock<HashMap<String, Verdict>>,
}

impl InMemoryVerdictRepository {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl VerdictRepository for InMemoryVerdictRepository {
    async fn upsert(&self, v: &Verdict) -> Result<(), CoreError> {
        self.rows
            .write()
            .await
            .insert(v.package.cache_key(), v.clone());
        Ok(())
    }
    async fn get(&self, pkg: &PackageId) -> Result<Option<Verdict>, CoreError> {
        Ok(self.rows.read().await.get(&pkg.cache_key()).cloned())
    }
    async fn list_for_package(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<Verdict>, CoreError> {
        Ok(self
            .rows
            .read()
            .await
            .values()
            .filter(|v| v.package.registry == registry && v.package.name == package)
            .cloned()
            .collect())
    }
    async fn list_due_for_rescan(
        &self,
        registry: &str,
        before: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<PackageId>, CoreError> {
        let rows = self.rows.read().await;
        let mut due: Vec<&Verdict> = rows
            .values()
            .filter(|v| v.package.registry == registry)
            .filter(|v| v.last_scanned_at.is_none_or(|at| at < before))
            .collect();
        due.sort_by_key(|v| v.last_scanned_at);
        Ok(due
            .into_iter()
            .take(limit as usize)
            .map(|v| v.package.clone())
            .collect())
    }

    async fn count_unrated_by_registry(&self) -> Result<Option<Vec<(String, u64)>>, CoreError> {
        let mut counts: HashMap<String, u64> = HashMap::new();
        for v in self.rows.read().await.values() {
            if v.findings
                .iter()
                .any(|f| f.severity == batlehub_core::entities::Severity::Unknown)
            {
                *counts.entry(v.package.registry.clone()).or_default() += 1;
            }
        }
        Ok(Some(counts.into_iter().collect()))
    }

    /// Mirrors the Postgres order: by reference first when asked, then most
    /// severe, reference, coordinate, scanner.
    async fn list_findings(
        &self,
        filter: &FindingsFilter,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<FindingRow>, u64), CoreError> {
        let rows = self.rows.read().await;
        let mut out: Vec<FindingRow> = rows
            .values()
            .filter(|v| filter.registries.contains(&v.package.registry))
            .filter(|v| filter.verdict.is_none_or(|s| s == v.state))
            .flat_map(|v| {
                v.findings.iter().map(|f| FindingRow {
                    package: v.package.clone(),
                    finding: batlehub_core::entities::Finding {
                        raw: serde_json::Value::Null,
                        ..f.clone()
                    },
                    verdict: v.state,
                })
            })
            .filter(|r| filter.min_severity.is_none_or(|m| r.finding.severity >= m))
            .filter(|r| {
                filter
                    .scanner
                    .as_ref()
                    .is_none_or(|s| *s == r.finding.scanner)
            })
            .filter(|r| {
                filter.reference_prefix.as_ref().is_none_or(|p| {
                    r.finding
                        .reference
                        .as_ref()
                        .is_some_and(|x| x.starts_with(p.as_str()))
                })
            })
            .collect();
        // `None` sorts last, as `NULLS LAST` does.
        let reference =
            |r: &FindingRow| (r.finding.reference.is_none(), r.finding.reference.clone());
        out.sort_by(|a, b| {
            let first = if filter.by_reference {
                reference(a).cmp(&reference(b))
            } else {
                std::cmp::Ordering::Equal
            };
            first
                .then(b.finding.severity.cmp(&a.finding.severity))
                .then(reference(a).cmp(&reference(b)))
                .then(a.package.registry.cmp(&b.package.registry))
                .then(a.package.name.cmp(&b.package.name))
                .then(a.package.version.cmp(&b.package.version))
                .then(a.finding.scanner.cmp(&b.finding.scanner))
        });
        let total = out.len() as u64;
        Ok((
            out.into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect(),
            total,
        ))
    }

    async fn count_by_registry_and_state(
        &self,
    ) -> Result<Vec<(String, VerdictState, u64)>, CoreError> {
        let mut counts: HashMap<(String, VerdictState), u64> = HashMap::new();
        for v in self.rows.read().await.values() {
            *counts
                .entry((v.package.registry.clone(), v.state))
                .or_default() += 1;
        }
        Ok(counts.into_iter().map(|((r, s), n)| (r, s, n)).collect())
    }

    async fn list_by_state(
        &self,
        registry: &str,
        state: VerdictState,
        limit: u64,
    ) -> Result<Vec<Verdict>, CoreError> {
        Ok(self
            .rows
            .read()
            .await
            .values()
            .filter(|v| v.package.registry == registry && v.state == state)
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

/// A queue row, with the columns the Postgres one has.
#[derive(Debug, Clone)]
struct Row {
    job: ScanJob,
    completed: bool,
}

/// Pick up to `n` of `sorted` (already in priority-then-age order): `n - 1`
/// from the front, then the oldest job of the *lowest* tier still waiting —
/// the reserved slot — or, when nothing of a lower tier waits, the next in
/// order. With `n == 1` there is nothing to reserve.
pub fn starvation_pick<T: Copy, P: Ord, A: Ord>(
    sorted: &[T],
    n: usize,
    priority: impl Fn(&T) -> P,
    created: impl Fn(&T) -> A,
) -> Vec<T> {
    if n <= 1 || sorted.len() <= n {
        return sorted.iter().take(n).copied().collect();
    }
    let mut out: Vec<T> = sorted.iter().take(n - 1).copied().collect();
    let rest = &sorted[n - 1..];
    let lowest = rest
        .iter()
        .max_by(|a, b| {
            priority(a)
                .cmp(&priority(b))
                .then(created(b).cmp(&created(a)))
        })
        .copied();
    if let Some(j) = lowest {
        out.push(j);
    }
    out
}

#[derive(Default)]
pub struct InMemoryScanQueue {
    rows: RwLock<Vec<Row>>,
}

impl InMemoryScanQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every open job, for assertions.
    pub async fn open_jobs(&self) -> Vec<ScanJob> {
        self.rows
            .read()
            .await
            .iter()
            .filter(|r| !r.completed)
            .map(|r| r.job.clone())
            .collect()
    }

    /// Make a deferred job leasable now, as if its deferral began at
    /// `since` — what a test needs to reach the 24-hour budget.
    pub async fn backdate_deferral(&self, id: Uuid, since: DateTime<Utc>) {
        if let Some(r) = self.rows.write().await.iter_mut().find(|r| r.job.id == id) {
            r.job.deferred_since = Some(since);
            r.job.leased_until = None;
        }
    }

    /// Expire every open lease, as a crashed worker would leave them.
    pub async fn expire_leases(&self) {
        for r in self.rows.write().await.iter_mut().filter(|r| !r.completed) {
            r.job.leased_until = r
                .job
                .leased_until
                .map(|_| Utc::now() - Duration::seconds(1));
        }
    }

    /// The last error a job failed with, for assertions.
    pub async fn last_error(&self, id: Uuid) -> Option<String> {
        self.rows
            .read()
            .await
            .iter()
            .find(|r| r.job.id == id)
            .and_then(|r| r.job.last_error.clone())
    }
}

#[async_trait]
impl ScanQueue for InMemoryScanQueue {
    async fn enqueue(
        &self,
        package: &PackageId,
        published_at: Option<DateTime<Utc>>,
        trigger: ScanTrigger,
    ) -> Result<bool, CoreError> {
        let mut rows = self.rows.write().await;
        if let Some(open) = rows
            .iter_mut()
            .find(|r| !r.completed && r.job.package == *package)
        {
            // Promote: an outranking trigger rises, and clears a deferral (a
            // running lease is left alone) — RFC 0041 decision 44.
            if trigger.priority() < open.job.trigger.priority() {
                open.job.trigger = trigger;
                if open.job.leased_by.is_none() {
                    open.job.leased_until = None;
                }
            }
            return Ok(false);
        }
        rows.push(Row {
            job: ScanJob {
                id: Uuid::new_v4(),
                package: package.clone(),
                published_at,
                artifact_sha256: None,
                trigger,
                attempts: 0,
                leased_until: None,
                created_at: Utc::now(),
                deferred_since: None,
                leased_by: None,
                last_error: None,
                completed_at: None,
            },
            completed: false,
        });
        Ok(true)
    }

    async fn lease(
        &self,
        worker_id: &str,
        registries: &[String],
        n: u32,
        lease_secs: u64,
        max_attempts: u32,
    ) -> Result<Vec<ScanJob>, CoreError> {
        let now = Utc::now();
        let mut rows = self.rows.write().await;
        let mut candidates: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                !r.completed
                    && r.job.leased_until.is_none_or(|u| u < now)
                    && r.job.attempts < max_attempts
                    && (registries.is_empty() || registries.contains(&r.job.package.registry))
            })
            .map(|(i, _)| i)
            .collect();
        candidates.sort_by_key(|&i| (rows[i].job.trigger.priority(), rows[i].job.created_at));
        // The anti-starvation slot (RFC 0018 §4.2, decision 16): of `n`
        // slots, one goes to the *lowest* tier waiting, so a flood of
        // `FirstSeen` never parks a backfill forever. The same rule the
        // Postgres queue applies, in two picks.
        let picked = starvation_pick(
            &candidates,
            n as usize,
            |&i| rows[i].job.trigger.priority(),
            |&i| rows[i].job.created_at,
        );
        let mut out = Vec::new();
        for i in picked {
            let r = &mut rows[i];
            r.job.attempts += 1;
            r.job.leased_until = Some(now + Duration::seconds(lease_secs as i64));
            r.job.leased_by = Some(worker_id.to_owned());
            out.push(r.job.clone());
        }
        Ok(out)
    }

    async fn heartbeat(&self, job_id: Uuid, lease_secs: u64) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        if let Some(r) = rows.iter_mut().find(|r| r.job.id == job_id && !r.completed) {
            r.job.leased_until = Some(Utc::now() + Duration::seconds(lease_secs as i64));
        }
        Ok(())
    }

    async fn complete(&self, job_id: Uuid, worker: Option<&str>) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        if let Some(r) = rows.iter_mut().find(|r| {
            r.job.id == job_id && worker.is_none_or(|w| r.job.leased_by.as_deref() == Some(w))
        }) {
            r.completed = true;
            r.job.leased_until = None;
            r.job.completed_at = Some(Utc::now());
        }
        Ok(())
    }

    async fn complete_exhausted(&self, job_id: Uuid, last_error: &str) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        if let Some(r) = rows.iter_mut().find(|r| r.job.id == job_id && !r.completed) {
            r.completed = true;
            r.job.leased_until = None;
            r.job.completed_at = Some(Utc::now());
            r.job.last_error = Some(last_error.to_owned());
        }
        Ok(())
    }

    /// Rows are kept in creation order and one coordinate has at most one
    /// open job, so the last closed row met is the latest closed.
    async fn jobs_for(
        &self,
        coords: &[PackageId],
    ) -> Result<Vec<(Option<ScanJob>, Option<ScanJob>)>, CoreError> {
        let rows = self.rows.read().await;
        Ok(coords
            .iter()
            .map(|c| {
                let same = |r: &&Row| {
                    r.job.package.registry == c.registry
                        && r.job.package.name == c.name
                        && r.job.package.version == c.version
                };
                let open = rows.iter().filter(same).find(|r| !r.completed);
                let closed = rows.iter().rev().filter(same).find(|r| r.completed);
                (open.map(|r| r.job.clone()), closed.map(|r| r.job.clone()))
            })
            .collect())
    }

    async fn list_jobs(
        &self,
        registries: &[String],
        filter: ScanJobFilter,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<ScanJob>, u64), CoreError> {
        let now = Utc::now();
        let rows = self.rows.read().await;
        let scanning =
            |j: &ScanJob| j.leased_by.is_some() && j.leased_until.is_some_and(|u| u > now);
        let mut hits: Vec<&ScanJob> = match filter {
            ScanJobFilter::Failed => {
                // The latest closed row per coordinate, rows being in creation
                // order; a coordinate with an open job is not failed.
                let mut latest: Vec<&ScanJob> = Vec::new();
                for r in rows.iter().filter(|r| r.completed) {
                    latest.retain(|j| j.package != r.job.package);
                    latest.push(&r.job);
                }
                latest.retain(|j| {
                    j.last_error
                        .as_deref()
                        .is_some_and(|e| e.starts_with(EXHAUSTED_PREFIX))
                        && !rows
                            .iter()
                            .any(|o| !o.completed && o.job.package == j.package)
                });
                latest.sort_by_key(|j| std::cmp::Reverse(j.completed_at));
                latest
            }
            _ => {
                let mut open: Vec<&ScanJob> = rows
                    .iter()
                    .filter(|r| !r.completed)
                    .map(|r| &r.job)
                    .filter(|j| match filter {
                        ScanJobFilter::Queued => !scanning(j),
                        ScanJobFilter::Scanning => scanning(j),
                        _ => true,
                    })
                    .collect();
                open.sort_by_key(|j| (!scanning(j), j.created_at));
                open
            }
        };
        hits.retain(|j| registries.contains(&j.package.registry));
        let total = hits.len() as u64;
        Ok((
            hits.into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .cloned()
                .collect(),
            total,
        ))
    }

    async fn fail(&self, job_id: Uuid, worker: &str, error: &str) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        if let Some(r) = rows.iter_mut().find(|r| {
            r.job.id == job_id && !r.completed && r.job.leased_by.as_deref() == Some(worker)
        }) {
            r.job.leased_until = None;
            r.job.leased_by = None;
            r.job.last_error = Some(error.to_owned());
        }
        Ok(())
    }

    async fn defer(
        &self,
        job_id: Uuid,
        worker: &str,
        not_before: DateTime<Utc>,
    ) -> Result<(), CoreError> {
        let mut rows = self.rows.write().await;
        if let Some(r) = rows.iter_mut().find(|r| {
            r.job.id == job_id && !r.completed && r.job.leased_by.as_deref() == Some(worker)
        }) {
            r.job.leased_until = Some(not_before);
            r.job.leased_by = None;
            r.job.attempts = r.job.attempts.saturating_sub(1);
            r.job.deferred_since.get_or_insert_with(Utc::now);
            r.job.last_error = Some(format!(
                "deferred since {}",
                r.job.deferred_since.unwrap_or_else(Utc::now).to_rfc3339()
            ));
        }
        Ok(())
    }

    async fn exhausted(&self, max_attempts: u32, n: u32) -> Result<Vec<ScanJob>, CoreError> {
        let now = Utc::now();
        Ok(self
            .rows
            .read()
            .await
            .iter()
            .filter(|r| {
                !r.completed
                    && r.job.attempts >= max_attempts
                    && r.job.leased_until.is_none_or(|u| u < now)
            })
            .take(n as usize)
            .map(|r| r.job.clone())
            .collect())
    }

    async fn try_lead(&self, _key: i64) -> Result<bool, CoreError> {
        // One process, by construction.
        Ok(true)
    }

    async fn queued(&self) -> Result<Vec<QueuedCount>, CoreError> {
        let now = Utc::now();
        let mut counts: HashMap<(String, ScanTrigger), (u64, u64)> = HashMap::new();
        for r in self.rows.read().await.iter().filter(|r| !r.completed) {
            let e = counts
                .entry((r.job.package.registry.clone(), r.job.trigger))
                .or_default();
            match r.job.leased_until {
                None => e.0 += 1,
                Some(u) if u < now => e.0 += 1,
                Some(_) if r.job.leased_by.is_none() => e.1 += 1,
                Some(_) => {}
            }
        }
        Ok(counts
            .into_iter()
            .map(|((registry, trigger), (count, deferred))| QueuedCount {
                registry,
                trigger,
                count,
                deferred,
            })
            .collect())
    }
}

#[derive(Default)]
pub struct InMemoryWorkerRegistry {
    seen: RwLock<HashMap<String, DateTime<Utc>>>,
}

impl InMemoryWorkerRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl WorkerRegistry for InMemoryWorkerRegistry {
    async fn heartbeat(&self, worker_id: &str, _: &[String]) -> Result<(), CoreError> {
        self.seen
            .write()
            .await
            .insert(worker_id.to_owned(), Utc::now());
        Ok(())
    }
    async fn live_count(&self, within_secs: u64) -> Result<u64, CoreError> {
        let cutoff = Utc::now() - Duration::seconds(within_secs as i64);
        Ok(self
            .seen
            .read()
            .await
            .values()
            .filter(|t| **t > cutoff)
            .count() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(v: &str) -> PackageId {
        PackageId::new("r", "p", v)
    }

    /// The queue's contract, on the in-memory store: idempotent enqueue,
    /// priority order, a lease another worker cannot take, and an expired
    /// lease returning to the queue.
    #[tokio::test]
    async fn the_queue_leases_by_priority_and_returns_expired_leases() {
        let q = InMemoryScanQueue::new();
        assert!(q
            .enqueue(&pkg("1"), None, ScanTrigger::Backfill)
            .await
            .unwrap());
        assert!(q
            .enqueue(&pkg("2"), None, ScanTrigger::FirstSeen)
            .await
            .unwrap());
        assert!(
            !q.enqueue(&pkg("2"), None, ScanTrigger::Rescan)
                .await
                .unwrap(),
            "one open job per coordinate"
        );

        let first = q.lease("w1", &[], 1, 60, 3).await.unwrap();
        assert_eq!(first[0].package.version, "2", "FirstSeen before Backfill");
        assert_eq!(first[0].attempts, 1);
        let second = q.lease("w2", &[], 5, 60, 3).await.unwrap();
        assert_eq!(second.len(), 1, "the leased job is not handed out twice");
        assert_eq!(second[0].package.version, "1");

        // Expire the first lease by hand and it comes back.
        q.fail(first[0].id, "w1", "crashed").await.unwrap();
        let again = q.lease("w3", &[], 5, 60, 3).await.unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].package.version, "2");
        assert_eq!(again[0].attempts, 2);
        assert_eq!(q.last_error(first[0].id).await.as_deref(), Some("crashed"));

        q.complete(again[0].id, Some("w3")).await.unwrap();
        q.complete(second[0].id, Some("w2")).await.unwrap();
        assert!(q.open_jobs().await.is_empty());
    }

    #[tokio::test]
    async fn exhausted_attempts_stop_being_leased_and_are_reported() {
        let q = InMemoryScanQueue::new();
        q.enqueue(&pkg("1"), None, ScanTrigger::FirstSeen)
            .await
            .unwrap();
        for _ in 0..2 {
            let j = q.lease("w", &[], 1, 60, 2).await.unwrap();
            q.fail(j[0].id, "w", "boom").await.unwrap();
        }
        assert!(q.lease("w", &[], 1, 60, 2).await.unwrap().is_empty());
        let spent = q.exhausted(2, 10).await.unwrap();
        assert_eq!(spent.len(), 1);
    }

    #[tokio::test]
    async fn worker_scoping_and_counts() {
        let q = InMemoryScanQueue::new();
        q.enqueue(&PackageId::new("a", "p", "1"), None, ScanTrigger::FirstSeen)
            .await
            .unwrap();
        q.enqueue(&PackageId::new("b", "p", "1"), None, ScanTrigger::FirstSeen)
            .await
            .unwrap();
        let only_b = q.lease("w", &["b".to_owned()], 5, 60, 3).await.unwrap();
        assert_eq!(only_b.len(), 1);
        assert_eq!(only_b[0].package.registry, "b");
        let counts = q.queued().await.unwrap();
        assert_eq!(counts.len(), 2);

        let w = InMemoryWorkerRegistry::new();
        assert_eq!(w.live_count(60).await.unwrap(), 0);
        w.heartbeat("w1", &[]).await.unwrap();
        assert_eq!(w.live_count(60).await.unwrap(), 1);
    }
}
