//! The read cache (`db/read_cache.rs`) on a real database: migration 066's
//! triggers, the `LISTEN` loop, and what happens when it drops.
//!
//! A write here is raw SQL on its own connection — another replica, whose
//! change this process learns of only by notification.
//!
//! Its own test binary, and one test: the cache is process-global.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_read_cache

use std::time::Duration;

use sqlx::PgPool;

use batlehub_adapters::db::packages::{PgPackageRepository, PoolOptions};
use batlehub_adapters::db::read_cache::{self, CACHES};
use batlehub_adapters::db::PgUserBlockRepository;
use batlehub_core::entities::PackageId;
use batlehub_core::ports::{PackageRepository, UserBlockRepository};

fn db_url() -> Option<String> {
    let url = std::env::var("DATABASE_URL").ok();
    if url.is_none() && std::env::var("BATLEHUB_REQUIRE_DB").is_ok_and(|v| v == "1") {
        panic!("DATABASE_URL is not set and BATLEHUB_REQUIRE_DB=1: refusing to skip");
    }
    url
}

async fn until(what: &str, mut cond: impl AsyncFnMut() -> bool) {
    for _ in 0..100 {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test]
async fn another_replicas_write_reaches_the_cache_and_a_dropped_listener_bypasses_it() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let repo = PgPackageRepository::new(
        &url,
        PoolOptions {
            max_connections: 4,
            min_connections: 0,
            acquire_timeout_secs: 10,
        },
    )
    .await
    .expect("connect");
    repo.run_migrations().await.expect("migrations");
    let other = PgPool::connect(&url).await.expect("second connection");
    let reg = format!("rc-{}", std::process::id());
    let pkg = PackageId::new(&reg, "left-pad", "1.0.0");
    let key = (
        reg.clone(),
        "left-pad".to_string(),
        "1.0.0".to_string(),
        None,
    );
    let users = PgUserBlockRepository::new(repo.pool());
    let user = format!("rc-user-{}", std::process::id());

    // Not listening yet: nothing is cached.
    assert_eq!(repo.covering_block(&pkg).await.unwrap(), None);
    assert_eq!(CACHES.blocks.get(&key), None);

    read_cache::spawn_invalidation_listener(repo.pool(), None);
    until("listener connected", async || CACHES.live()).await;

    assert_eq!(repo.covering_block(&pkg).await.unwrap(), None);
    assert_eq!(CACHES.blocks.get(&key), Some(None), "the miss is cached");
    assert!(!users.is_blocked(&user).await.unwrap());

    // The access-event write's `available` row is not a block: no notification.
    sqlx::query(
        "INSERT INTO package_statuses (id, registry, package_name, package_version, status)
         VALUES (gen_random_uuid(), $1, 'left-pad', '1.0.0', 'available')",
    )
    .bind(&reg)
    .execute(&other)
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        CACHES.blocks.get(&key),
        Some(None),
        "an available row clears nothing"
    );

    // Another replica blocks it: this one refuses without restarting.
    sqlx::query(
        "UPDATE package_statuses SET status = 'blocked', block_reason = 'malware'
         WHERE registry = $1 AND package_name = 'left-pad'",
    )
    .bind(&reg)
    .execute(&other)
    .await
    .unwrap();
    until("block seen through the cache", async || {
        repo.covering_block(&pkg).await.unwrap().as_deref() == Some("malware")
    })
    .await;

    sqlx::query("INSERT INTO user_blocks (user_id, blocked_by) VALUES ($1, 'test')")
        .bind(&user)
        .execute(&other)
        .await
        .unwrap();
    until("user block seen through the cache", async || {
        users.is_blocked(&user).await.unwrap()
    })
    .await;

    // The batch prefetch fills the cache with what `covering_block` answers,
    // the artifact covered by its bare version's block included.
    let tar = PackageId {
        artifact: Some("left-pad-1.0.0.tgz".into()),
        ..pkg.clone()
    };
    let free = PackageId::new(&reg, "is-even", "2.0.0");
    CACHES.blocks.clear();
    repo.prefetch_covering_blocks(&[tar.clone(), free.clone()])
        .await;
    let k = |p: &PackageId| {
        (
            p.registry.clone(),
            p.name.clone(),
            p.version.clone(),
            p.artifact.clone(),
        )
    };
    assert_eq!(CACHES.blocks.get(&k(&tar)), Some(Some("malware".into())));
    assert_eq!(CACHES.blocks.get(&k(&free)), Some(None));

    // The listener's connection dies: the cache stops serving at once and
    // comes back, emptied, once it reconnects.
    sqlx::query(
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
         WHERE query ILIKE 'LISTEN%batlehub_invalidate%' AND pid <> pg_backend_pid()",
    )
    .execute(&other)
    .await
    .unwrap();
    until("listener down", async || !CACHES.live()).await;
    assert_eq!(CACHES.blocks.get(&key), None, "nothing served while down");
    // A change made while it is down must not be hidden by a stale entry.
    sqlx::query("DELETE FROM user_blocks WHERE user_id = $1")
        .bind(&user)
        .execute(&other)
        .await
        .unwrap();
    until("listener back", async || CACHES.live()).await;
    assert!(!users.is_blocked(&user).await.unwrap());

    sqlx::query("DELETE FROM package_statuses WHERE registry = $1")
        .bind(&reg)
        .execute(&other)
        .await
        .unwrap();
}
