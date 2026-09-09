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

    q.complete(a[0].id).await.unwrap();
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
    q.fail(second[0].id, "crashed").await.unwrap();
    // attempts == max_attempts: no longer leasable, reported as exhausted.
    assert!(q
        .lease("w4", std::slice::from_ref(&reg), 1, 60, 2)
        .await
        .unwrap()
        .is_empty());
    let spent = q.exhausted(2, 10).await.unwrap();
    assert!(spent.iter().any(|j| j.package == p));
    q.complete(second[0].id).await.unwrap();
    assert!(q
        .exhausted(2, 10)
        .await
        .unwrap()
        .iter()
        .all(|j| j.package != p));
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
