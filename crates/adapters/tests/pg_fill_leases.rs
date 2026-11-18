//! RFC 0048-bis §6.3 on a real database: `PgFillLeases` — take, conflict,
//! expiry and takeover, the fencing of every holder write (T1), progress
//! reaching a follower by `NOTIFY` and by polling, and the sweep (T6).
//!
//! Its own test binary: the listener and its `live` flag are process-global,
//! so the one test that needs them runs the polling half before starting it.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_fill_leases

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;

use batlehub_adapters::db::packages::{PgPackageRepository, PoolOptions};
use batlehub_adapters::db::read_cache::{self, CACHES};
use batlehub_adapters::db::{PgAdvisoryLeader, PgFillLeases};
use batlehub_adapters::in_memory::InMemoryStorageBackend;
use batlehub_core::ports::{
    fill_staging_key, FillError, FillLeases, LeaderLock, Phase, StorageBackend, Taken,
    FILL_SEGMENT_BYTES,
};
use batlehub_core::services::proxy::{sweep_fills, FILL_SWEEP_LEADER_KEY};

/// `sweep_fills` claims every expired lease in the table, not only its own:
/// run beside a test whose lease is expired on purpose, it took that lease
/// first (`reap_claims_only_a_still_expired_lease` read `None`, main CI,
/// 2026-11-18). The sweep test holds this exclusively, every other one shared.
static SWEEP: std::sync::LazyLock<tokio::sync::RwLock<()>> =
    std::sync::LazyLock::new(|| tokio::sync::RwLock::new(()));

async fn repo() -> Option<PgPackageRepository> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        if std::env::var("BATLEHUB_REQUIRE_DB").is_ok_and(|v| v == "1") {
            panic!("DATABASE_URL is not set and BATLEHUB_REQUIRE_DB=1: refusing to skip");
        }
        eprintln!("skipping: DATABASE_URL is not set");
        return None;
    };
    let repo = PgPackageRepository::new(
        &url,
        PoolOptions {
            max_connections: 8,
            min_connections: 0,
            acquire_timeout_secs: 10,
        },
    )
    .await
    .expect("connect");
    repo.run_migrations().await.expect("migrations");
    Some(repo)
}

fn key() -> String {
    format!("artifact:fill-test/{}/1.0", uuid::Uuid::new_v4().simple())
}

fn started(t: Taken) -> (uuid::Uuid, Option<(uuid::Uuid, u32)>) {
    match t {
        Taken::Started { fill, previous } => (fill, previous),
        Taken::Running(s) => panic!("expected to take the lease, found {s:?}"),
    }
}

/// T1 against Postgres: a live lease is followed, an expired one is taken
/// over with what it may have written, and the stale holder's every write
/// matches no row.
#[tokio::test]
async fn a_stale_holder_cannot_commit_into_a_taken_over_fill() {
    let Some(repo) = repo().await else { return };
    let _shared = SWEEP.read().await;
    let leases = PgFillLeases::with_ttl(repo.pool(), Duration::from_millis(400));
    let k = key();

    let (old, previous) = started(leases.take(&k, "a").await.unwrap());
    assert_eq!(previous, None);
    assert!(leases.commit(&k, old, 3).await.unwrap());
    match leases.take(&k, "b").await.unwrap() {
        Taken::Running(s) => {
            assert_eq!((s.fill, s.holder.as_str(), s.committed), (old, "a", 3));
            assert_eq!(s.phase, Phase::Filling);
        }
        other => panic!("a live lease is not taken over: {other:?}"),
    }
    assert!(leases.heartbeat(&k, old).await.unwrap());

    tokio::time::sleep(Duration::from_millis(600)).await;
    let listed = leases.expired(Duration::from_millis(400)).await.unwrap();
    assert!(listed.contains(&(k.clone(), old, 4)), "{listed:?}");
    let (new, previous) = started(leases.take(&k, "b").await.unwrap());
    assert_eq!(
        previous,
        Some((old, 4)),
        "the takeover learns what to delete"
    );

    assert!(!leases.commit(&k, old, 4).await.unwrap(), "fenced commit");
    assert!(
        !leases.heartbeat(&k, old).await.unwrap(),
        "fenced heartbeat"
    );
    assert!(
        !leases.finish(&k, old, Phase::Done).await.unwrap(),
        "fenced finish"
    );
    leases.release(&k, old).await.unwrap();
    assert!(
        leases.commit(&k, new, 1).await.unwrap(),
        "the stale release left the new lease"
    );

    // A failed fill is taken over at once, without waiting for its expiry.
    let failed = Phase::Failed(FillError::other("x".repeat(4096)));
    assert!(leases.finish(&k, new, failed).await.unwrap());
    assert!(
        !leases.commit(&k, new, 2).await.unwrap(),
        "no commit after an end"
    );
    let (_, previous) = started(leases.take(&k, "c").await.unwrap());
    assert_eq!(previous, None, "an ended fill deleted its own segments");
}

/// An ended row stays readable until it expires, so a follower that polls
/// learns how the fill ended (not "it vanished"); `take` replaces it at once
/// and reports no takeover.
#[tokio::test]
async fn an_ended_row_tells_a_late_follower_and_is_taken_over_at_once() {
    let Some(repo) = repo().await else { return };
    let _shared = SWEEP.read().await;
    let leases = PgFillLeases::new(repo.pool());
    let k = key();
    let (fill, _) = started(leases.take(&k, "a").await.unwrap());
    assert!(leases.commit(&k, fill, 2).await.unwrap());
    let failed = Phase::Failed(FillError::other("upstream: 404"));
    assert!(leases.finish(&k, fill, failed.clone()).await.unwrap());
    let mut follow = leases.follow(&k, fill);
    let s = tokio::time::timeout(Duration::from_secs(5), follow.next())
        .await
        .expect("the first read is a query")
        .unwrap()
        .unwrap();
    assert_eq!(s.phase, failed);
    assert!(follow.next().await.is_none());
    let (_, previous) = started(leases.take(&k, "b").await.unwrap());
    assert_eq!(previous, None, "an ended fill is not a takeover");

    let k = key();
    let (fill, _) = started(leases.take(&k, "a").await.unwrap());
    assert!(leases.finish(&k, fill, Phase::Done).await.unwrap());
    let (_, previous) = started(leases.take(&k, "b").await.unwrap());
    assert_eq!(previous, None, "a done row is replaced at once");
}

/// The sweep's claim is fenced on expiry: a holder that heartbeated since
/// the listing keeps its row; a still-expired one is claimed once.
#[tokio::test]
async fn reap_claims_only_a_still_expired_lease() {
    let Some(repo) = repo().await else { return };
    let _shared = SWEEP.read().await;
    let ttl = Duration::from_millis(300);
    let leases = PgFillLeases::with_ttl(repo.pool(), ttl);
    let k = key();
    let (fill, _) = started(leases.take(&k, "a").await.unwrap());
    assert!(leases.commit(&k, fill, 2).await.unwrap());
    tokio::time::sleep(ttl * 2).await;
    assert!(
        leases.heartbeat(&k, fill).await.unwrap(),
        "back from a pause"
    );
    assert_eq!(leases.reap(&k, fill).await.unwrap(), None, "alive again");
    tokio::time::sleep(ttl * 2).await;
    assert_eq!(leases.reap(&k, fill).await.unwrap(), Some(3));
    assert_eq!(leases.reap(&k, fill).await.unwrap(), None, "claimed once");
}

/// Two concurrent takes of one key: exactly one starts, the other follows it.
#[tokio::test]
async fn concurrent_takes_start_one_fill() {
    let Some(repo) = repo().await else { return };
    let _shared = SWEEP.read().await;
    let leases = Arc::new(PgFillLeases::new(repo.pool()));
    let k = key();
    let takes = futures::future::join_all((0..8).map(|i| {
        let (leases, k) = (Arc::clone(&leases), k.clone());
        async move { leases.take(&k, &format!("r{i}")).await.unwrap() }
    }))
    .await;
    let fills: Vec<_> = takes
        .iter()
        .filter_map(|t| match t {
            Taken::Started { fill, .. } => Some(*fill),
            Taken::Running(_) => None,
        })
        .collect();
    assert_eq!(fills.len(), 1, "{takes:?}");
    for t in &takes {
        if let Taken::Running(s) = t {
            assert_eq!(s.fill, fills[0], "every loser follows the winner");
        }
    }
}

/// Progress reaches a follower on another "replica": by polling while the
/// listener is down, then by `NOTIFY` once it is live, phases included.
#[tokio::test]
async fn a_follower_hears_progress_by_polling_then_by_notify() {
    let Some(repo) = repo().await else { return };
    let _shared = SWEEP.read().await;
    let holder = PgFillLeases::new(repo.pool());
    let follower = PgFillLeases::new(repo.pool());
    let k = key();
    let (fill, _) = started(holder.take(&k, "a").await.unwrap());

    assert!(!CACHES.live(), "polling half first");
    let mut follow = follower.follow(&k, fill);
    assert_eq!(follow.next().await.unwrap().unwrap().committed, 0);
    assert!(holder.commit(&k, fill, 1).await.unwrap());
    let s = tokio::time::timeout(Duration::from_secs(5), follow.next())
        .await
        .expect("a poll within a second or so")
        .unwrap()
        .unwrap();
    assert_eq!(s.committed, 1);
    drop(follow);

    read_cache::spawn_invalidation_listener(repo.pool(), None);
    for _ in 0..200 {
        if CACHES.live() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(CACHES.live(), "listener connected");
    let mut follow = follower.follow(&k, fill);
    assert_eq!(follow.next().await.unwrap().unwrap().committed, 1);
    let cached = format!("{k}#dedup");
    let ticket = CACHES.dedup.ticket();
    CACHES
        .dedup
        .put(ticket, cached.clone(), ("blob/x".into(), "default".into()));
    // Live: nothing polls before the 30 s lease, so a quick answer is a NOTIFY.
    let quick = Duration::from_secs(5);
    assert!(holder.commit(&k, fill, 2).await.unwrap());
    let s = tokio::time::timeout(quick, follow.next())
        .await
        .expect("woken by NOTIFY")
        .unwrap()
        .unwrap();
    assert_eq!((s.committed, s.phase), (2, Phase::Filling));
    let total = 2 * FILL_SEGMENT_BYTES as u64 + 5;
    assert!(holder
        .finish(&k, fill, Phase::Verified { total })
        .await
        .unwrap());
    let s = tokio::time::timeout(quick, follow.next())
        .await
        .expect("woken by NOTIFY")
        .unwrap()
        .unwrap();
    assert_eq!((s.committed, s.phase), (3, Phase::Verified { total }));
    let failed = Phase::Failed(FillError::other("upstream: 502"));
    assert!(holder.finish(&k, fill, failed.clone()).await.unwrap());
    holder.release(&k, fill).await.unwrap();
    let s = tokio::time::timeout(quick, follow.next())
        .await
        .expect("woken by NOTIFY")
        .unwrap()
        .unwrap();
    assert_eq!(s.phase, failed, "the failure itself, not a row read");
    assert!(
        follow.next().await.is_none(),
        "a terminal state ends the follow"
    );
    assert!(
        CACHES.dedup.get(&cached).is_some(),
        "a fill payload is not a table invalidation: the read cache was kept"
    );
}

/// T6: the elected replica's sweep deletes an expired fill's segments and
/// row, and leaves a live fill alone; a second leader is refused the lock.
#[tokio::test]
async fn the_sweep_deletes_an_expired_fills_segments_behind_the_leader() {
    let Some(repo) = repo().await else { return };
    let _alone = SWEEP.write().await;
    let ttl = Duration::from_millis(300);
    let leases = PgFillLeases::with_ttl(repo.pool(), ttl);
    let storage = InMemoryStorageBackend::new();
    let (dead_key, live_key) = (key(), key());
    let (dead, _) = started(leases.take(&dead_key, "dead").await.unwrap());
    let dead_staged = fill_staging_key(dead, &dead_key);
    storage
        .put_segment(&dead_staged, 0, Bytes::from_static(b"orphan"))
        .await
        .unwrap();
    assert!(leases.commit(&dead_key, dead, 1).await.unwrap());
    tokio::time::sleep(ttl * 2).await;
    let (live, _) = started(leases.take(&live_key, "live").await.unwrap());
    let live_staged = fill_staging_key(live, &live_key);
    storage
        .put_segment(&live_staged, 0, Bytes::from_static(b"kept"))
        .await
        .unwrap();

    // A key of this test's own: the server's sweep may hold the real one.
    let lock = FILL_SWEEP_LEADER_KEY ^ i64::from(std::process::id());
    let first = PgAdvisoryLeader::new(repo.pool(), lock);
    let second = PgAdvisoryLeader::new(repo.pool(), lock);
    assert!(first.try_lead().await.unwrap());
    assert!(!second.try_lead().await.unwrap(), "one sweeper at a time");

    sweep_fills(&leases, storage.as_ref(), ttl).await;
    assert!(storage
        .get_segment(&dead_staged, 0)
        .await
        .unwrap()
        .is_none());
    assert!(storage
        .get_segment(&live_staged, 0)
        .await
        .unwrap()
        .is_some());
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT storage_key FROM fill_leases WHERE storage_key = ANY($1)")
            .bind(vec![dead_key.clone(), live_key.clone()])
            .fetch_all(&repo.pool())
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![live_key],
        "the dead row is gone, the live one kept"
    );
}

/// A holder that ends while a take sits between its snapshot and its upsert
/// is not reported as taken over: the snapshot still says `filling`, but the
/// row was never expired.
#[tokio::test]
async fn an_end_racing_a_take_is_not_a_takeover() {
    let Some(repo) = repo().await else { return };
    let _shared = SWEEP.read().await;
    let pool = repo.pool();
    let leases = PgFillLeases::new(pool.clone());
    let k = key();
    let (a, _) = started(leases.take(&k, "a").await.unwrap());

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE fill_leases SET phase = 'done' WHERE storage_key = $1")
        .bind(&k)
        .execute(&mut *tx)
        .await
        .unwrap();
    let take = tokio::spawn({
        let (leases, k) = (PgFillLeases::new(pool.clone()), k.clone());
        async move { leases.take(&k, "b").await }
    });
    // The take's upsert now waits on the row lock with a `filling` snapshot.
    tokio::time::sleep(Duration::from_millis(300)).await;
    tx.commit().await.unwrap();

    let (_, previous) = started(take.await.unwrap().unwrap());
    assert_eq!(previous, None, "{a} ended; it was not taken over");
}
