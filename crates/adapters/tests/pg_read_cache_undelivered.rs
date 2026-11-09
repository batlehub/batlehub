//! The read cache never turns on when notifications do not reach its listener
//! — what a transaction-mode pooler (PgBouncer) does to `LISTEN`.
//!
//! Simulated with `listen_url` on another database of the same server:
//! NOTIFY is per database, so the heartbeat sent through the pool never
//! arrives, exactly as behind the pooler. Its own binary: the cache is
//! process-global.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_read_cache_undelivered

use std::time::Duration;

use batlehub_adapters::db::packages::{PgPackageRepository, PoolOptions};
use batlehub_adapters::db::read_cache::{self, CACHES};

#[tokio::test]
async fn a_listener_that_never_hears_its_heartbeat_keeps_the_cache_off() {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) if std::env::var("BATLEHUB_REQUIRE_DB").is_ok_and(|v| v == "1") => {
            panic!("DATABASE_URL is not set and BATLEHUB_REQUIRE_DB=1: refusing to skip")
        }
        Err(_) => {
            eprintln!("skipping: DATABASE_URL is not set");
            return;
        }
    };
    let repo = PgPackageRepository::new(
        &url,
        PoolOptions {
            max_connections: 2,
            min_connections: 0,
            acquire_timeout_secs: 10,
        },
    )
    .await
    .expect("connect");
    // Same server and credentials, another database.
    let (base, _db) = url.rsplit_once('/').expect("a database in DATABASE_URL");
    let elsewhere = format!("{base}/template1");
    // Reachable: what keeps the cache off below is delivery, not connectivity.
    sqlx::query("SELECT 1")
        .execute(&sqlx::PgPool::connect(&elsewhere).await.expect("template1"))
        .await
        .expect("template1 answers");

    read_cache::spawn_invalidation_listener(repo.pool(), Some(elsewhere));
    // Past the first heartbeat's 10-second wait, and one retry.
    for _ in 0..30 {
        assert!(
            !CACHES.live(),
            "the cache went live without a delivered heartbeat"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
