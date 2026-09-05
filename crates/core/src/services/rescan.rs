//! The rescan scheduler (RFC 0018 §4.2 *Rescan*, phase 4): on a registry
//! whose `[registries.security.rescan] interval_secs` is set, every verdict
//! whose last scan is older than the interval is queued again, at `Rescan`
//! priority — below a user waiting on `FirstSeen` and a SOC's `Webhook`,
//! above a `Backfill`.
//!
//! One timer per estate, not one per process: the leader is whichever
//! process holds the PostgreSQL advisory lock (`ScanQueue::try_lead`, RFC
//! 0018 §6.3), and every other process's tick is a no-op. Enqueue is
//! idempotent on the open job, so even a lock that changed hands mid-tick
//! queues nothing twice.
//!
//! The queued job carries the version's `published_at` from the metadata
//! cache when the cache still has it: the worker's age gate reads it, and a
//! rescan that forgot the date would re-judge a dated version as
//! `TIMESTAMP_MISSING`. When the cache no longer has it the worker resolves
//! it from upstream before judging (`ScanWorker::run_job`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::entities::{PackageId, ScanTrigger};
use crate::error::CoreError;
use crate::ports::{CacheStore, ScanQueue, VerdictRepository};
use crate::services::HotConfigLock;

/// The advisory-lock key the scheduler's leadership is taken under. One
/// key for the estate: there is one rescan timer, whatever the number of
/// registries.
pub const RESCAN_LEADER_KEY: i64 = 0x0018_7e5c_a000;

/// How often the scheduler looks, whatever the registries' intervals: a
/// registry due every six hours is queued within a minute of being due.
pub const RESCAN_TICK: Duration = Duration::from_secs(60);

/// What one tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RescanReport {
    /// Whether this process held the lock; nothing below is attempted
    /// without it.
    pub leader: bool,
    /// Verdicts past their interval, per registry.
    pub due: HashMap<String, usize>,
    /// Jobs actually created (an open job already queued is not counted).
    pub queued: usize,
}

pub struct RescanScheduler {
    pub verdicts: Arc<dyn VerdictRepository>,
    pub queue: Arc<dyn ScanQueue>,
    /// Which registries rescan, and how often (`SecurityPolicy::rescan_interval`).
    pub hot: HotConfigLock,
    /// The metadata cache, for the version's date. `None` queues undated.
    pub cache: Option<Arc<dyn CacheStore>>,
    /// Due rows taken per registry per tick, so a first tick on a large
    /// registry does not queue the whole table at once.
    pub batch: u64,
}

impl RescanScheduler {
    /// One tick: take the lock (or keep it), then queue what is due.
    pub async fn run_once(&self, now: DateTime<Utc>) -> Result<RescanReport, CoreError> {
        let mut report = RescanReport::default();
        if !self.queue.try_lead(RESCAN_LEADER_KEY).await? {
            return Ok(report);
        }
        report.leader = true;
        let intervals: Vec<(String, Duration)> = {
            let hot = self.hot.read().await;
            hot.security
                .iter()
                .filter_map(|(name, p)| p.rescan_interval.map(|i| (name.clone(), i)))
                .collect()
        };
        for (registry, interval) in intervals {
            let before = now - chrono::Duration::from_std(interval).unwrap_or_default();
            let due = self
                .verdicts
                .list_due_for_rescan(&registry, before, self.batch)
                .await?;
            report.due.insert(registry.clone(), due.len());
            for id in due {
                let published_at = self.published_at(&id).await;
                match self
                    .queue
                    .enqueue(&id, published_at, ScanTrigger::Rescan)
                    .await
                {
                    Ok(true) => report.queued += 1,
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(package = %id, error = %e, "rescan: could not queue")
                    }
                }
            }
        }
        Ok(report)
    }

    /// The version's date, from the metadata the proxy cached when it
    /// served it. Stale is fine: a date does not change.
    async fn published_at(&self, id: &PackageId) -> Option<DateTime<Utc>> {
        let cache = self.cache.as_ref()?;
        let key = crate::services::proxy::proxy_meta_key(id);
        match cache.get_stale(&key).await {
            Ok(Some(entry)) => entry.metadata.published_at,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{PackageId, SecurityPolicy, Verdict, VerdictState};
    use crate::services::HotConfig;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tokio::sync::RwLock;

    #[derive(Default)]
    struct FakeVerdicts {
        due: Mutex<Vec<PackageId>>,
        asked: Mutex<Vec<(String, DateTime<Utc>, u64)>>,
    }

    #[async_trait]
    impl VerdictRepository for FakeVerdicts {
        async fn upsert(&self, _: &Verdict) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get(&self, _: &PackageId) -> Result<Option<Verdict>, CoreError> {
            Ok(None)
        }
        async fn list_for_package(&self, _: &str, _: &str) -> Result<Vec<Verdict>, CoreError> {
            Ok(vec![])
        }
        async fn list_by_state(
            &self,
            _: &str,
            _: VerdictState,
            _: u64,
        ) -> Result<Vec<Verdict>, CoreError> {
            Ok(vec![])
        }
        async fn list_due_for_rescan(
            &self,
            registry: &str,
            before: DateTime<Utc>,
            limit: u64,
        ) -> Result<Vec<PackageId>, CoreError> {
            self.asked
                .lock()
                .unwrap()
                .push((registry.to_owned(), before, limit));
            Ok(self
                .due
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.registry == registry)
                .cloned()
                .collect())
        }
    }

    #[derive(Default)]
    struct FakeQueue {
        lead: Mutex<bool>,
        jobs: Mutex<Vec<(PackageId, ScanTrigger)>>,
    }

    #[async_trait]
    impl ScanQueue for FakeQueue {
        async fn enqueue(
            &self,
            package: &PackageId,
            _: Option<DateTime<Utc>>,
            trigger: ScanTrigger,
        ) -> Result<bool, CoreError> {
            let mut jobs = self.jobs.lock().unwrap();
            if jobs.iter().any(|(p, _)| p == package) {
                return Ok(false);
            }
            jobs.push((package.clone(), trigger));
            Ok(true)
        }
        async fn lease(
            &self,
            _: &str,
            _: &[String],
            _: u32,
            _: u64,
            _: u32,
        ) -> Result<Vec<crate::entities::ScanJob>, CoreError> {
            Ok(vec![])
        }
        async fn heartbeat(&self, _: uuid::Uuid, _: u64) -> Result<(), CoreError> {
            Ok(())
        }
        async fn complete(&self, _: uuid::Uuid) -> Result<(), CoreError> {
            Ok(())
        }
        async fn fail(&self, _: uuid::Uuid, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        async fn exhausted(
            &self,
            _: u32,
            _: u32,
        ) -> Result<Vec<crate::entities::ScanJob>, CoreError> {
            Ok(vec![])
        }
        async fn queued(&self) -> Result<Vec<crate::ports::QueuedCount>, CoreError> {
            Ok(vec![])
        }
        async fn try_lead(&self, _: i64) -> Result<bool, CoreError> {
            Ok(*self.lead.lock().unwrap())
        }
    }

    fn scheduler(lead: bool) -> (RescanScheduler, Arc<FakeVerdicts>, Arc<FakeQueue>) {
        let verdicts = Arc::new(FakeVerdicts::default());
        let queue = Arc::new(FakeQueue {
            lead: Mutex::new(lead),
            ..Default::default()
        });
        let mut hot = HotConfig::default();
        let mut timed = SecurityPolicy::defaults_for("timed");
        timed.rescan_interval = Some(Duration::from_secs(3600));
        hot.security.insert("timed".into(), timed);
        hot.security
            .insert("untimed".into(), SecurityPolicy::defaults_for("untimed"));
        let s = RescanScheduler {
            verdicts: Arc::clone(&verdicts) as Arc<dyn VerdictRepository>,
            queue: Arc::clone(&queue) as Arc<dyn ScanQueue>,
            hot: Arc::new(RwLock::new(hot)),
            cache: None,
            batch: 50,
        };
        (s, verdicts, queue)
    }

    #[tokio::test]
    async fn the_leader_queues_what_is_due_at_rescan_priority_and_only_for_timed_registries() {
        let (s, verdicts, queue) = scheduler(true);
        verdicts.due.lock().unwrap().extend([
            PackageId::new("timed", "a", "1"),
            PackageId::new("timed", "b", "1"),
            PackageId::new("untimed", "c", "1"),
        ]);
        let now = Utc::now();
        let r = s.run_once(now).await.unwrap();
        assert!(r.leader);
        assert_eq!(r.queued, 2);
        assert_eq!(r.due.get("timed"), Some(&2));
        assert!(!r.due.contains_key("untimed"), "no interval, never asked");
        let asked = verdicts.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, "timed");
        assert_eq!(asked[0].1, now - chrono::Duration::seconds(3600));
        assert_eq!(asked[0].2, 50);
        let jobs = queue.jobs.lock().unwrap().clone();
        assert!(jobs.iter().all(|(_, t)| *t == ScanTrigger::Rescan));

        // Still due on the next tick (the fake never scans): the open job
        // makes the second enqueue a no-op, not a duplicate.
        let r = s.run_once(now).await.unwrap();
        assert_eq!(r.queued, 0);
        assert_eq!(queue.jobs.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_process_without_the_lock_does_nothing() {
        let (s, verdicts, queue) = scheduler(false);
        verdicts
            .due
            .lock()
            .unwrap()
            .push(PackageId::new("timed", "a", "1"));
        let r = s.run_once(Utc::now()).await.unwrap();
        assert_eq!(r, RescanReport::default());
        assert!(verdicts.asked.lock().unwrap().is_empty(), "not even asked");
        assert!(queue.jobs.lock().unwrap().is_empty());
    }
}
