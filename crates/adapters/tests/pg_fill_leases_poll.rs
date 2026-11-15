//! RFC 0048-bis R10 for a follower, on a real database: a polling follower
//! survives a failed row read and keeps following the fill.
//!
//! Its own test binary: it takes `fill_leases` away from every connection
//! for a moment, and needs the invalidation listener never started (the
//! listener is process-global; `pg_fill_leases` starts it).
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_fill_leases_poll

use std::time::Duration;

use futures::StreamExt;

use batlehub_adapters::db::packages::{PgPackageRepository, PoolOptions};
use batlehub_adapters::db::read_cache::CACHES;
use batlehub_adapters::db::PgFillLeases;
use batlehub_core::ports::{FillLeases, Phase, Taken};

/// A database outage shorter than the lease costs the follower nothing: its
/// failed polls are retried, its stream neither ends nor errors, and it
/// reports the holder's next commit and the end once the table is back.
#[tokio::test]
async fn a_polling_follower_survives_a_failed_row_read() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        if std::env::var("BATLEHUB_REQUIRE_DB").is_ok_and(|v| v == "1") {
            panic!("DATABASE_URL is not set and BATLEHUB_REQUIRE_DB=1: refusing to skip");
        }
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
    let pool = repo.pool();
    let ttl = Duration::from_secs(10);
    let holder = PgFillLeases::with_ttl(pool.clone(), ttl);
    let follower = PgFillLeases::with_ttl(pool.clone(), ttl);
    let k = format!("artifact:fill-poll/{}/1.0", uuid::Uuid::new_v4().simple());
    let Taken::Started { fill, .. } = holder.take(&k, "a").await.unwrap() else {
        panic!("a fresh key is taken")
    };
    assert!(holder.commit(&k, fill, 1).await.unwrap());

    assert!(!CACHES.live(), "the follower polls");
    let mut follow = follower.follow(&k, fill);
    assert_eq!(follow.next().await.unwrap().unwrap().committed, 1);

    // Every read of the row fails for ~2.5 s: two or three of the follower's
    // one-a-second polls, well inside its 10 s lease.
    sqlx::query("ALTER TABLE fill_leases RENAME TO fill_leases_off")
        .execute(&pool)
        .await
        .unwrap();
    let during = tokio::time::timeout(Duration::from_millis(2500), follow.next()).await;
    sqlx::query("ALTER TABLE fill_leases_off RENAME TO fill_leases")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        during.is_err(),
        "the follow neither ended nor errored on a failed read: {during:?}"
    );

    assert!(holder.commit(&k, fill, 2).await.unwrap());
    let s = tokio::time::timeout(Duration::from_secs(5), follow.next())
        .await
        .expect("the next poll reads the row again")
        .unwrap()
        .unwrap();
    assert_eq!((s.committed, s.phase), (2, Phase::Filling));
    assert!(holder.finish(&k, fill, Phase::Done).await.unwrap());
    let s = tokio::time::timeout(Duration::from_secs(5), follow.next())
        .await
        .expect("the end is read too")
        .unwrap()
        .unwrap();
    assert_eq!(s.phase, Phase::Done);
}
