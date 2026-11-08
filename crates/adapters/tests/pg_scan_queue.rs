//! RFC 0041 decision 44 on a real database: a deferred scan job.
//!
//! What the in-memory double cannot prove: the `leased_by` guard against a
//! stale worker, the attempt given back, the `last_error` anchor surviving a
//! re-lease, `ON CONFLICT … DO UPDATE` promoting a deferred row, and
//! `queued()` leaving it out of the gauge the worker HPA scales on.
//!
//! `task test:pg-scan-queue` sets `BATLEHUB_REQUIRE_DB=1`, so there a missing
//! `DATABASE_URL` fails the suite instead of skipping it green.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_scan_queue

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use sqlx::PgPool;

use batlehub_adapters::db::PgScanQueue;
use batlehub_core::{
    entities::{PackageId, ScanTrigger},
    ports::ScanQueue,
};

fn db_url() -> Option<String> {
    let url = std::env::var("DATABASE_URL").ok();
    if url.is_none() && std::env::var("BATLEHUB_REQUIRE_DB").is_ok_and(|v| v == "1") {
        panic!("DATABASE_URL is not set and BATLEHUB_REQUIRE_DB=1: refusing to skip");
    }
    url
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

async fn fixture(url: &str) -> (PgPool, String) {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let registry = format!("defer-t{}-{id}", std::process::id());
    let pool = PgPool::connect(url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    sqlx::query("DELETE FROM scan_jobs WHERE registry = $1")
        .bind(&registry)
        .execute(&pool)
        .await
        .expect("clear");
    (pool, registry)
}

#[tokio::test]
async fn a_deferred_job_gives_its_attempt_back_and_waits_out_its_not_before() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = PgScanQueue::new(pool.clone());
    let regs = std::slice::from_ref(&reg);
    let p = PackageId::new(&reg, "pkg", "1.0.0");
    assert!(q.enqueue(&p, None, ScanTrigger::FirstSeen).await.unwrap());

    let job = q.lease("w1", regs, 1, 60, 3).await.unwrap().remove(0);
    assert_eq!(job.attempts, 1);
    assert!(job.deferred_since.is_none());

    // A worker that does not hold the lease changes nothing.
    q.defer(job.id, "stale", Utc::now() + Duration::minutes(5))
        .await
        .unwrap();
    let (attempts, by): (i32, Option<String>) =
        sqlx::query_as("SELECT attempts, leased_by FROM scan_jobs WHERE id = $1")
            .bind(job.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((attempts, by.as_deref()), (1, Some("w1")));

    q.defer(job.id, "w1", Utc::now() + Duration::minutes(5))
        .await
        .unwrap();
    assert!(
        q.lease("w2", regs, 1, 60, 3).await.unwrap().is_empty(),
        "not leasable before not_before"
    );
    let counts = q.queued().await.unwrap();
    let mine = counts.iter().find(|c| c.registry == reg).expect("counted");
    assert_eq!((mine.count, mine.deferred), (0, 1), "{counts:?}");

    // A stale worker cannot complete or fail it either.
    q.complete(job.id, Some("w1")).await.unwrap();
    q.fail(job.id, "w1", "late").await.unwrap();
    assert_eq!(
        q.queued()
            .await
            .unwrap()
            .iter()
            .find(|c| c.registry == reg)
            .unwrap()
            .deferred,
        1
    );

    // The not-before passes: leased again, the attempt was given back, and
    // the anchor is still the first deferral's.
    sqlx::query("UPDATE scan_jobs SET leased_until = NOW() - INTERVAL '1 second' WHERE id = $1")
        .bind(job.id)
        .execute(&pool)
        .await
        .unwrap();
    let again = q.lease("w2", regs, 1, 60, 3).await.unwrap().remove(0);
    assert_eq!(again.attempts, 1, "the deferred attempt was given back");
    let anchor = again.deferred_since.expect("anchored");
    assert!((Utc::now() - anchor).num_seconds() < 60);

    q.defer(again.id, "w2", Utc::now() + Duration::minutes(5))
        .await
        .unwrap();
    sqlx::query("UPDATE scan_jobs SET leased_until = NOW() - INTERVAL '1 second' WHERE id = $1")
        .bind(job.id)
        .execute(&pool)
        .await
        .unwrap();
    let third = q.lease("w3", regs, 1, 60, 3).await.unwrap().remove(0);
    assert_eq!(
        third.deferred_since,
        Some(anchor),
        "a second deferral keeps the anchor"
    );
    q.complete(third.id, Some("w3")).await.unwrap();
    assert!(q.lease("w4", regs, 1, 60, 3).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_outranking_enqueue_promotes_a_deferred_job() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let q = PgScanQueue::new(pool);
    let regs = std::slice::from_ref(&reg);
    let p = PackageId::new(&reg, "pkg", "1.0.0");
    q.enqueue(&p, None, ScanTrigger::Rescan).await.unwrap();
    let job = q.lease("w1", regs, 1, 60, 3).await.unwrap().remove(0);
    q.defer(job.id, "w1", Utc::now() + Duration::hours(1))
        .await
        .unwrap();

    // A lower-priority enqueue changes nothing.
    assert!(!q.enqueue(&p, None, ScanTrigger::Backfill).await.unwrap());
    assert!(q.lease("w2", regs, 1, 60, 3).await.unwrap().is_empty());

    // A webhook outranks the rescan: one row, promoted and leasable now.
    assert!(!q.enqueue(&p, None, ScanTrigger::Webhook).await.unwrap());
    let promoted = q.lease("w2", regs, 1, 60, 3).await.unwrap();
    assert_eq!(promoted.len(), 1);
    assert_eq!(promoted[0].id, job.id);
    assert_eq!(promoted[0].trigger, ScanTrigger::Webhook);

    // A running lease is not cleared by a promotion.
    assert!(!q.enqueue(&p, None, ScanTrigger::FirstSeen).await.unwrap());
    assert!(q.lease("w3", regs, 1, 60, 3).await.unwrap().is_empty());
}

/// RFC 0041 decision 48 (M16): a database release N has migrated refuses a
/// binary built without N's marker migration — a skipped N or a rollback
/// below it fails at boot, not one refused version at a time.
#[tokio::test]
async fn a_binary_without_the_release_marker_refuses_a_database_that_has_it() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, _) = fixture(&url).await;
    let current = batlehub_adapters::migrations::embedded_migrator();
    assert!(
        current.migrations.iter().any(|m| m.version == 65),
        "the marker is embedded"
    );
    let older = sqlx::migrate::Migrator::with_migrations(
        current
            .migrations
            .iter()
            .filter(|m| m.version < 65)
            .cloned()
            .collect(),
    );
    let err = older
        .run(&pool)
        .await
        .expect_err("an older binary must not start");
    assert!(
        err.to_string()
            .contains("previously applied but is missing"),
        "{err}"
    );
}
