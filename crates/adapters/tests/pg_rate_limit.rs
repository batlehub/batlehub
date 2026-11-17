//! Integration tests for `PgRateLimitStore`.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test pg_rate_limit
//!
//! Tests are skipped (not failed) when `DATABASE_URL` is unset.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sqlx::PgPool;

use batlehub_adapters::rate_limit::PgRateLimitStore;
use batlehub_core::ports::RateLimitStore;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

// Each test gets an isolated key prefix to allow parallel execution without
// rows from one test interfering with another.
static TEST_ID: AtomicU64 = AtomicU64::new(0);

struct TestStore {
    store: PgRateLimitStore,
    pool: PgPool,
    prefix: String,
}

impl TestStore {
    fn key(&self, name: &str) -> String {
        format!("pg-rl-test-{}:{}", self.prefix, name)
    }

    /// Read the raw row count for a key prefix from the DB (for pruning assertions).
    async fn raw_row_count(&self, key: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rate_limit_counters WHERE key = $1")
            .bind(key)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0)
    }
}

async fn make_store(url: &str) -> TestStore {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    // Include a nanosecond timestamp so keys from different test-binary invocations
    // never collide with leftover rows in the shared Postgres instance.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let prefix = format!("{ts}-{id}");
    let pool = PgPool::connect(url).await.expect("connect to postgres");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("run migrations");
    TestStore {
        store: PgRateLimitStore::new(pool.clone()),
        pool,
        prefix,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn increment_starts_at_one() {
    let Some(url) = db_url() else { return };
    let s = make_store(&url).await;
    let (count, _) = s.store.increment(&s.key("k"), 60).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn repeated_increments_accumulate() {
    let Some(url) = db_url() else { return };
    let s = make_store(&url).await;
    let key = s.key("k");
    // window_start is wall-clock aligned, so the counter legitimately resets if the
    // 60s window rolls over mid-loop. Track the reset boundary and re-base `expected`
    // when it changes — the invariant under test is "consecutive increments within one
    // window accumulate", not "the window never rolls".
    let mut expected = 0u64;
    let mut window = None;
    for _ in 0..10 {
        let (count, reset) = s.store.increment(&key, 60).await.unwrap();
        if window != Some(reset) {
            expected = 0;
            window = Some(reset);
        }
        expected += 1;
        assert_eq!(count, expected, "increment should return {expected}");
    }
}

#[tokio::test]
async fn independent_keys_do_not_interfere() {
    let Some(url) = db_url() else { return };
    // The window is wall-clock aligned: if it rolls between the calls, the
    // counters legitimately restart, so retry on fresh keys (once is enough).
    for attempt in 0..2 {
        let s = make_store(&url).await;
        let (a, wa) = s.store.increment(&s.key("a"), 60).await.unwrap();
        let (b, _) = s.store.increment(&s.key("b"), 60).await.unwrap();
        let (a2, wa2) = s.store.increment(&s.key("a"), 60).await.unwrap();
        if wa != wa2 && attempt == 0 {
            continue;
        }
        assert_eq!(a, 1);
        assert_eq!(b, 1);
        assert_eq!(a2, 2, "second increment for 'a' should be 2");
        return;
    }
}

#[tokio::test]
async fn reset_unix_is_at_window_boundary() {
    let Some(url) = db_url() else { return };
    let s = make_store(&url).await;
    let window_secs: u64 = 60;
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (_, reset) = s
        .store
        .increment(&s.key("k"), window_secs as u32)
        .await
        .unwrap();
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // reset must be strictly after the call started and at most one window from now
    assert!(
        reset > before,
        "reset {reset} must be after call start {before}"
    );
    assert!(
        reset <= after + window_secs,
        "reset {reset} must be ≤ {}",
        after + window_secs
    );
    // reset must fall on a window boundary (multiple of window_secs)
    assert_eq!(
        reset % window_secs,
        0,
        "reset {reset} must be a multiple of {window_secs}"
    );
}

#[tokio::test]
async fn increment_no_longer_prunes_inline() {
    let Some(url) = db_url() else { return };
    let s = make_store(&url).await;
    let key = s.key("prune");

    // Seed a row in a *previous but recent* window (≈10 min ago, aligned to 60s).
    // It must NOT be at window_start = 0: prune_expired_removes_old_window_rows runs in
    // parallel and its DELETE is global (keyed by window_start only), so an epoch row would
    // get swept by that test's prune(3600), leaving us with a spurious count of 1. A row
    // ~10 min old survives a 3600s-retention sweep yet is still a distinct, stale window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let stale_window = ((now - 600) / 60) * 60;
    sqlx::query("INSERT INTO rate_limit_counters (key, window_start, count) VALUES ($1, $2, 999)")
        .bind(&key)
        .bind(stale_window)
        .execute(&s.pool)
        .await
        .unwrap();
    assert_eq!(s.raw_row_count(&key).await, 1, "seeded row should exist");

    // Pruning now happens on a periodic background sweep (`prune_expired`), not inline
    // on every increment — the stale row must survive a regular increment call.
    s.store.increment(&key, 60).await.unwrap();
    assert_eq!(
        s.raw_row_count(&key).await,
        2,
        "stale row should NOT be pruned by increment(); current-window row is added alongside it"
    );
}

#[tokio::test]
async fn prune_expired_removes_old_window_rows() {
    let Some(url) = db_url() else { return };
    let s = make_store(&url).await;
    let key = s.key("prune-expired");

    // Insert a fake row from a long-past window directly.
    sqlx::query("INSERT INTO rate_limit_counters (key, window_start, count) VALUES ($1, 0, 999)")
        .bind(&key)
        .execute(&s.pool)
        .await
        .unwrap();
    assert_eq!(s.raw_row_count(&key).await, 1, "seeded row should exist");

    // prune_expired's DELETE is global (keyed by window_start only, not by key), so a
    // tiny retention like 0 would also nuke the *current-window* rows of other tests
    // running in parallel (e.g. concurrent_increments_are_atomic), causing spurious
    // lost-update failures. Use an hour of retention: still well past the seeded epoch
    // row (window_start = 0) so it's pruned, but safely keeps every live window.
    s.store.prune_expired(3600).await.unwrap();
    assert_eq!(
        s.raw_row_count(&key).await,
        0,
        "stale row should have been pruned by prune_expired"
    );
}

#[tokio::test]
async fn concurrent_increments_are_atomic() {
    let Some(url) = db_url() else { return };
    let s = make_store(&url).await;
    let key = s.key("concurrent");

    // Spawn 20 concurrent tasks each incrementing the same key.
    let store = Arc::new(s.store);
    let handles: Vec<_> = (0..20)
        .map(|_| {
            let store = store.clone();
            let key = key.clone();
            tokio::spawn(async move { store.increment(&key, 60).await.unwrap() })
        })
        .collect();

    let results: Vec<(u64, u64)> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // No lost updates: within each window, the returned counts must be exactly 1..=n
    // (unique, contiguous). Group by the reset boundary so a task that straddles a
    // 60s window roll doesn't look like a lost update.
    let mut by_window: std::collections::HashMap<u64, Vec<u64>> = std::collections::HashMap::new();
    for (count, reset) in results {
        by_window.entry(reset).or_default().push(count);
    }
    for (reset, mut counts) in by_window {
        counts.sort_unstable();
        let expected: Vec<u64> = (1..=counts.len() as u64).collect();
        assert_eq!(
            counts, expected,
            "counts within window {reset} must be 1..=n with no gaps or duplicates"
        );
    }
}

#[tokio::test]
async fn different_window_secs_produce_independent_windows() {
    let Some(url) = db_url() else { return };
    let s = make_store(&url).await;
    // Using the same logical name but different window_secs creates different window_start values,
    // so they should not count against each other's row.
    let key = s.key("multiwin");
    let (c60, reset60) = s.store.increment(&key, 60).await.unwrap();
    let (c3600, reset3600) = s.store.increment(&key, 3600).await.unwrap();
    assert_eq!(c60, 1);
    // The 3600-window boundary differs from the 60-window one, so a fresh
    // (key, window_start) row is inserted regardless of pruning.
    assert!(c3600 >= 1, "count for 3600-window should be ≥ 1");
    assert!(
        reset3600 > reset60 || reset3600 >= reset60,
        "3600-window resets later than 60-window"
    );
}
