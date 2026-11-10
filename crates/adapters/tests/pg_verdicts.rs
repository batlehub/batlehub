//! Integration tests for the RFC 0018 stores on a real database.
//!
//! Worth Postgres for the things the in-memory double proves nothing about:
//! `FOR UPDATE SKIP LOCKED` handing one job to one of two workers, the
//! partial unique index that makes `enqueue` idempotent on the open job, a
//! lease expiring by the database clock, and the findings replaced as a set
//! under one transaction.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_verdicts

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use sqlx::PgPool;

use batlehub_adapters::db::{PgScanQueue, PgVerdictRepository, PgWorkerRegistry};
use batlehub_core::{
    entities::{
        Finding, FindingKind, PackageId, ReasonCode, ScanTrigger, Severity, Verdict, VerdictState,
    },
    ports::{ScanQueue, VerdictRepository, WorkerRegistry},
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

/// A registry name unique per run, so a second run against the same
/// database does not read the first run's rows.
async fn fixture(url: &str) -> (PgPool, String) {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let registry = format!("sec-t{}-{id}", std::process::id());
    let pool = PgPool::connect(url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    for table in ["scan_jobs", "artifact_findings", "artifact_verdicts"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE registry = $1"
        )))
        .bind(&registry)
        .execute(&pool)
        .await
        .expect("clear");
    }
    (pool, registry)
}

#[tokio::test]
async fn two_workers_never_lease_the_same_job_and_priority_orders_them() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = PgScanQueue::new(pool.clone());

    let p1 = PackageId::new(&reg, "left-pad", "1.0.0");
    let p2 = PackageId::new(&reg, "left-pad", "2.0.0");
    assert!(q.enqueue(&p1, None, ScanTrigger::Backfill).await.unwrap());
    assert!(q.enqueue(&p2, None, ScanTrigger::FirstSeen).await.unwrap());
    assert!(
        !q.enqueue(&p2, None, ScanTrigger::Rescan).await.unwrap(),
        "one open job per coordinate — the partial unique index"
    );

    let a = q
        .lease("w-a", std::slice::from_ref(&reg), 1, 60, 3)
        .await
        .unwrap();
    let b = q
        .lease("w-b", std::slice::from_ref(&reg), 5, 60, 3)
        .await
        .unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].package.version, "2.0.0", "FirstSeen first");
    assert_eq!(
        b.len(),
        1,
        "the leased job was skipped, not handed out twice"
    );
    assert_eq!(b[0].package.version, "1.0.0");

    q.complete(a[0].id, Some("w-a")).await.unwrap();
    // Now the coordinate is free for a new job.
    assert!(q.enqueue(&p2, None, ScanTrigger::Rescan).await.unwrap());
    let counts = q.queued().await.unwrap();
    let mine: Vec<_> = counts.iter().filter(|c| c.registry == reg).collect();
    assert_eq!(mine.len(), 2, "{counts:?}");
}

#[tokio::test]
async fn an_expired_lease_returns_to_the_queue_and_spent_attempts_are_reported() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = PgScanQueue::new(pool.clone());
    let p = PackageId::new(&reg, "pkg", "1.0.0");
    q.enqueue(&p, Some(Utc::now()), ScanTrigger::FirstSeen)
        .await
        .unwrap();

    // Lease for one second and let it expire by the database clock.
    let first = q
        .lease("w", std::slice::from_ref(&reg), 1, 1, 2)
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert!(q
        .lease("w2", std::slice::from_ref(&reg), 1, 1, 2)
        .await
        .unwrap()
        .is_empty());
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let second = q
        .lease("w2", std::slice::from_ref(&reg), 1, 60, 2)
        .await
        .unwrap();
    assert_eq!(second.len(), 1, "the expired lease came back");
    assert_eq!(second[0].attempts, 2);
    assert!(second[0].published_at.is_some());

    // Heartbeat extends it; a third lease is refused until it expires again.
    q.heartbeat(second[0].id, 60).await.unwrap();
    assert!(q
        .lease("w3", std::slice::from_ref(&reg), 1, 60, 2)
        .await
        .unwrap()
        .is_empty());
    q.fail(second[0].id, "w2", "crashed").await.unwrap();
    // attempts == max_attempts: no longer leasable, reported as exhausted.
    assert!(q
        .lease("w4", std::slice::from_ref(&reg), 1, 60, 2)
        .await
        .unwrap()
        .is_empty());
    let spent = q.exhausted(2, 10).await.unwrap();
    assert!(spent.iter().any(|j| j.package == p));
    q.complete(second[0].id, None).await.unwrap();
    assert!(q
        .exhausted(2, 10)
        .await
        .unwrap()
        .iter()
        .all(|j| j.package != p));
}

/// RFC 0042 §5.1: the batch reads behind a list page's scan statuses, and
/// decision 34's marker on an exhausted row.
#[tokio::test]
async fn jobs_for_and_get_many_answer_per_coordinate_in_order() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = PgScanQueue::new(pool.clone());
    let repo = PgVerdictRepository::new(pool.clone());
    let a = PackageId::new(&reg, "pkg", "1.0.0");
    let b = PackageId::new(&reg, "pkg", "2.0.0");
    let none = PackageId::new(&reg, "pkg", "3.0.0");

    // `a`: one failed-and-closed job, then a later successful one, then an
    // open one. `b`: one exhausted job.
    for _ in 0..2 {
        q.enqueue(&a, None, ScanTrigger::FirstSeen).await.unwrap();
        let j = q
            .lease("w", std::slice::from_ref(&reg), 1, 60, 3)
            .await
            .unwrap();
        q.complete(j[0].id, Some("w")).await.unwrap();
    }
    q.enqueue(&a, None, ScanTrigger::Rescan).await.unwrap();
    q.enqueue(&b, None, ScanTrigger::Rescan).await.unwrap();
    let jb = q
        .lease("w", std::slice::from_ref(&reg), 2, 60, 3)
        .await
        .unwrap();
    let jb = jb.iter().find(|j| j.package == b).unwrap();
    assert_eq!(jb.leased_by.as_deref(), Some("w"));
    q.complete_exhausted(jb.id, "exhausted: tar: unexpected EOF")
        .await
        .unwrap();

    let got = q
        .jobs_for(&[none.clone(), a.clone(), b.clone(), a.clone()])
        .await
        .unwrap();
    assert_eq!(got.len(), 4);
    assert!(got[0].0.is_none() && got[0].1.is_none());
    for (open, closed) in [&got[1], &got[3]] {
        assert_eq!(open.as_ref().unwrap().package, a);
        assert!(closed.as_ref().unwrap().last_error.is_none());
    }
    assert!(got[2].0.is_none());
    assert_eq!(
        got[2].1.as_ref().unwrap().last_error.as_deref(),
        Some("exhausted: tar: unexpected EOF")
    );

    let v = Verdict {
        package: b.clone(),
        state: VerdictState::Allowed,
        reason_codes: vec![],
        findings: vec![],
        policy_ref: format!("{reg}/default"),
        available_at: None,
        evaluated_at: Utc::now(),
        last_scanned_at: Some(Utc::now()),
        scanners_done: vec!["osv".into()],
    };
    repo.upsert(&v).await.unwrap();
    let many = repo.get_many(&[a.clone(), b.clone()]).await.unwrap();
    assert!(many[0].is_none());
    assert_eq!(many[1].as_ref().unwrap().state, VerdictState::Allowed);
}

/// RFC 0042 phase 2: the Scans page reads open jobs scanning first, and as
/// failed only a coordinate whose **latest** closed job is exhausted and that
/// has no open job.
#[tokio::test]
async fn list_jobs_reads_the_queue_and_rule_5_failures() {
    use batlehub_core::entities::ScanJobFilter;
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = PgScanQueue::new(pool.clone());
    let regs = std::slice::from_ref(&reg);
    let pkg = |n: &str| PackageId::new(&reg, n, "1");
    let close = |n: &'static str, err: Option<&'static str>| {
        let q = &q;
        let p = pkg(n);
        async move {
            q.enqueue(&p, None, ScanTrigger::FirstSeen).await.unwrap();
            let j = q.lease("w", regs, 1, 60, 3).await.unwrap().remove(0);
            match err {
                Some(e) => q.complete_exhausted(j.id, e).await.unwrap(),
                None => q.complete(j.id, Some("w")).await.unwrap(),
            }
        }
    };
    close("failed", Some("exhausted: tar: unexpected EOF")).await;
    // Failed, then succeeded: not failed.
    close("recovered", Some("exhausted: boom")).await;
    close("recovered", None).await;
    // Failed, then rescanning: on the queue, not under Failures.
    close("retrying", Some("exhausted: boom")).await;
    q.enqueue(&pkg("retrying"), None, ScanTrigger::Rescan)
        .await
        .unwrap();
    q.enqueue(&pkg("running"), None, ScanTrigger::FirstSeen)
        .await
        .unwrap();
    q.lease("w-2", regs, 1, 60, 3).await.unwrap();

    let (failed, total) = q
        .list_jobs(regs, ScanJobFilter::Failed, 50, 0)
        .await
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(failed[0].package, pkg("failed"));
    assert!(failed[0].completed_at.is_some());

    let (open, total) = q.list_jobs(regs, ScanJobFilter::Open, 50, 0).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(open[0].package, pkg("running"), "scanning first");
    assert_eq!(open[1].package, pkg("retrying"));
    let (queued, _) = q
        .list_jobs(regs, ScanJobFilter::Queued, 50, 0)
        .await
        .unwrap();
    assert_eq!(queued.len(), 1);
    let (page2, total) = q.list_jobs(regs, ScanJobFilter::Open, 1, 1).await.unwrap();
    assert_eq!((page2.len(), total), (1, 2));
    let (none, _) = q.list_jobs(&[], ScanJobFilter::Open, 50, 0).await.unwrap();
    assert!(none.is_empty());
}

#[tokio::test]
async fn a_verdict_round_trips_with_its_findings_replaced_as_a_set() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let repo = PgVerdictRepository::new(pool.clone());
    let pkg = PackageId::new(&reg, "left-pad", "1.3.1");
    assert!(repo.get(&pkg).await.unwrap().is_none());

    let mut v = Verdict {
        package: pkg.clone(),
        state: VerdictState::Denied,
        reason_codes: vec![ReasonCode::Vulnerability, ReasonCode::MinAgeNotMet],
        findings: vec![
            Finding::new(
                "osv",
                FindingKind::Vulnerability,
                ReasonCode::Vulnerability,
                Severity::High,
                "GHSA-1",
            )
            .with_reference("GHSA-1")
            .with_raw(serde_json::json!({"purl": "pkg:npm/left-pad@1.3.1"})),
            Finding::new(
                "age",
                FindingKind::Age,
                ReasonCode::MinAgeNotMet,
                Severity::High,
                "young",
            ),
        ],
        policy_ref: format!("{reg}/default"),
        available_at: Some(Utc::now() + chrono::Duration::hours(1)),
        evaluated_at: Utc::now(),
        last_scanned_at: Some(Utc::now()),
        scanners_done: vec!["osv".into()],
    };
    repo.upsert(&v).await.unwrap();
    let back = repo.get(&pkg).await.unwrap().unwrap();
    assert_eq!(back.state, VerdictState::Denied);
    assert_eq!(back.reason_codes, v.reason_codes);
    assert_eq!(back.findings.len(), 2);
    let osv = back.findings.iter().find(|f| f.scanner == "osv").unwrap();
    assert_eq!(osv.reference.as_deref(), Some("GHSA-1"));
    assert_eq!(osv.raw["purl"], "pkg:npm/left-pad@1.3.1");
    assert_eq!(back.scanners_done, vec!["osv"]);

    // A re-evaluation replaces the findings, never appends.
    v.findings.truncate(1);
    v.state = VerdictState::Allowed;
    v.reason_codes.clear();
    repo.upsert(&v).await.unwrap();
    let back = repo.get(&pkg).await.unwrap().unwrap();
    assert_eq!(back.findings.len(), 1);
    assert_eq!(back.state, VerdictState::Allowed);

    let listed = repo
        .list_by_state(&reg, VerdictState::Allowed, 10)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);

    let workers = PgWorkerRegistry::new(pool);
    let id = format!("w-{reg}");
    workers
        .heartbeat(&id, std::slice::from_ref(&reg))
        .await
        .unwrap();
    assert!(workers.live_count(60).await.unwrap() >= 1);
}

/// RFC 0041 decision 33: a row a newer release wrote — a reason code this
/// one does not know, a severity it cannot parse — is read, not a read
/// error: the code as `UNRECOGNISED`, the severity as `critical`, and a list
/// over the row still answers. And the release-N upgrade count sees it.
#[tokio::test]
async fn a_row_a_newer_release_wrote_is_read_leniently_and_counted() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let repo = PgVerdictRepository::new(pool.clone());
    let pkg = PackageId::new(&reg, "pkg", "1.0.0");
    repo.upsert(&Verdict {
        package: pkg.clone(),
        state: VerdictState::Denied,
        reason_codes: vec![ReasonCode::Vulnerability],
        findings: vec![
            Finding::new(
                "osv",
                FindingKind::Vulnerability,
                ReasonCode::Vulnerability,
                Severity::High,
                "GHSA-1",
            ),
            Finding::new(
                "osv",
                FindingKind::Vulnerability,
                ReasonCode::Vulnerability,
                Severity::Unknown,
                "MAL-2023-1000",
            ),
        ],
        policy_ref: format!("{reg}/default"),
        available_at: None,
        evaluated_at: Utc::now(),
        last_scanned_at: Some(Utc::now()),
        scanners_done: vec!["osv".into()],
    })
    .await
    .unwrap();
    let counts = repo
        .count_unrated_by_registry()
        .await
        .unwrap()
        .expect("not contended");
    assert!(counts.contains(&(reg.clone(), 1)), "{counts:?}");

    sqlx::query(
        "UPDATE artifact_verdicts SET reason_codes = ARRAY['FROM_THE_FUTURE']
         WHERE registry = $1",
    )
    .bind(&reg)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE artifact_findings SET severity = 'bogus', code = 'FROM_THE_FUTURE'
         WHERE registry = $1 AND summary = 'GHSA-1'",
    )
    .bind(&reg)
    .execute(&pool)
    .await
    .unwrap();

    let back = repo.get(&pkg).await.unwrap().expect("read, not an error");
    assert_eq!(back.reason_codes, vec![ReasonCode::Unrecognised]);
    let f = back
        .findings
        .iter()
        .find(|f| f.summary == "GHSA-1")
        .unwrap();
    assert_eq!(
        (f.code, f.severity),
        (ReasonCode::Unrecognised, Severity::Critical)
    );
    let listed = repo
        .list_by_state(&reg, VerdictState::Denied, 10)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
}

// ── RFC 0018 phase 4 ─────────────────────────────────────────────────────────

/// Due-selection picks the rows past the interval — and the never-scanned
/// ones — oldest first, and nothing scanned since.
#[tokio::test]
async fn due_selection_picks_only_rows_past_the_interval_oldest_first() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let repo = PgVerdictRepository::new(pool.clone());
    let now = Utc::now();
    let verdict = |name: &str, scanned: Option<chrono::DateTime<Utc>>| Verdict {
        package: PackageId::new(&reg, name, "1.0.0"),
        state: VerdictState::Allowed,
        reason_codes: vec![],
        findings: vec![],
        policy_ref: format!("{reg}/default"),
        available_at: None,
        evaluated_at: now,
        last_scanned_at: scanned,
        scanners_done: vec!["osv".into()],
    };
    repo.upsert(&verdict("stale", Some(now - chrono::Duration::hours(48))))
        .await
        .unwrap();
    repo.upsert(&verdict("older", Some(now - chrono::Duration::hours(72))))
        .await
        .unwrap();
    repo.upsert(&verdict("fresh", Some(now - chrono::Duration::hours(1))))
        .await
        .unwrap();
    repo.upsert(&verdict("never", None)).await.unwrap();

    let due = repo
        .list_due_for_rescan(&reg, now - chrono::Duration::hours(24), 10)
        .await
        .unwrap();
    let names: Vec<&str> = due.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["never", "older", "stale"], "{names:?}");

    let capped = repo
        .list_due_for_rescan(&reg, now - chrono::Duration::hours(24), 2)
        .await
        .unwrap();
    assert_eq!(capped.len(), 2);
    assert!(repo
        .list_due_for_rescan(&reg, now - chrono::Duration::days(365), 10)
        .await
        .unwrap()
        .iter()
        .all(|p| p.name == "never"));
}

/// Under a flood of user-facing jobs, one slot per lease goes to the lowest
/// tier waiting (decision 16), so a backfill is not starved.
#[tokio::test]
async fn the_starvation_slot_lets_a_backfill_through_under_a_first_seen_flood() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = PgScanQueue::new(pool.clone());
    for i in 0..20 {
        q.enqueue(
            &PackageId::new(&reg, format!("hot-{i}"), "1"),
            None,
            ScanTrigger::FirstSeen,
        )
        .await
        .unwrap();
    }
    q.enqueue(
        &PackageId::new(&reg, "cold", "1"),
        None,
        ScanTrigger::Backfill,
    )
    .await
    .unwrap();

    let leased = q
        .lease("w1", std::slice::from_ref(&reg), 4, 60, 3)
        .await
        .unwrap();
    assert_eq!(leased.len(), 4);
    let triggers: Vec<ScanTrigger> = leased.iter().map(|j| j.trigger).collect();
    assert_eq!(
        triggers
            .iter()
            .filter(|t| **t == ScanTrigger::FirstSeen)
            .count(),
        3
    );
    assert!(
        triggers.contains(&ScanTrigger::Backfill),
        "the reserved slot took the backfill: {triggers:?}"
    );

    // One slot reserves nothing: strict priority.
    let one = q
        .lease("w1", std::slice::from_ref(&reg), 1, 60, 3)
        .await
        .unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].trigger, ScanTrigger::FirstSeen);

    // Nothing of a lower tier left: the slot falls back to the next in order.
    let more = q
        .lease("w1", std::slice::from_ref(&reg), 4, 60, 3)
        .await
        .unwrap();
    assert_eq!(more.len(), 4);
    assert!(more.iter().all(|j| j.trigger == ScanTrigger::FirstSeen));
}

/// An idle worker's wait ends when this process enqueues — the embedded
/// worker starts a scan at once however far its backoff has grown — and runs
/// its full length when nothing is enqueued. A duplicate enqueue creates no
/// row and wakes nobody.
#[tokio::test]
async fn an_enqueue_wakes_the_idle_wait_and_a_duplicate_does_not() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = std::sync::Arc::new(PgScanQueue::new(pool.clone()));
    let long = std::time::Duration::from_secs(10);
    let short = std::time::Duration::from_millis(200);

    let started = std::time::Instant::now();
    q.wait_for_work(short).await;
    assert!(started.elapsed() >= short, "no enqueue, the whole wait");

    let waiter = {
        let q = std::sync::Arc::clone(&q);
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            q.wait_for_work(long).await;
            started.elapsed()
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let pkg = PackageId::new(&reg, "wake", "1.0.0");
    assert!(q.enqueue(&pkg, None, ScanTrigger::FirstSeen).await.unwrap());
    assert!(
        waiter.await.unwrap() < long / 2,
        "the enqueue woke the wait"
    );

    assert!(!q.enqueue(&pkg, None, ScanTrigger::FirstSeen).await.unwrap());
    let started = std::time::Instant::now();
    q.wait_for_work(short).await;
    assert!(
        started.elapsed() >= short,
        "a duplicate leaves no permit behind"
    );
}

/// One rescan timer per estate: the advisory lock is held by the first
/// queue that asks, refused to a second over another connection, and
/// released when the holder is dropped.
#[tokio::test]
async fn leadership_is_one_process_at_a_time_and_released_on_drop() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, _reg) = fixture(&url).await;
    let key = 0x7e5c_0000 + i64::from(std::process::id());
    let first = PgScanQueue::new(pool.clone());
    let second = PgScanQueue::new(pool.clone());
    assert!(first.try_lead(key).await.unwrap());
    assert!(first.try_lead(key).await.unwrap(), "kept, not re-taken");
    assert!(!second.try_lead(key).await.unwrap(), "held elsewhere");
    drop(first);
    // The lock goes with the connection, which the pool closes on drop.
    for _ in 0..20 {
        if second.try_lead(key).await.unwrap() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the lock was not released after the holder was dropped");
}
