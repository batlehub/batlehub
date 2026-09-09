//! The `upstream_status` store on a real database (RFC 0014 §10): upsert
//! semantics, the `''` sentinel round-tripping to `Option<String>`,
//! `disappeared_keys`, and filter pagination.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_upstream_status

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use sqlx::PgPool;

use batlehub_adapters::db::PgUpstreamStatusStore;
use batlehub_core::{
    entities::{
        MissObservation, UpstreamKey, UpstreamState, UpstreamStatusFilter, LAST_ERROR_MAX_BYTES,
    },
    ports::UpstreamStatusPort,
};

static TEST_ID: AtomicU64 = AtomicU64::new(0);

async fn fixture() -> Option<(PgPool, String)> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let registry = format!("audit-t{}-{id}", std::process::id());
    let pool = PgPool::connect(&url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    sqlx::query("DELETE FROM upstream_status WHERE registry = $1")
        .bind(&registry)
        .execute(&pool)
        .await
        .expect("clear");
    Some((pool, registry))
}

#[tokio::test]
async fn a_miss_upserts_and_the_package_sentinel_round_trips_as_none() {
    let Some((pool, reg)) = fixture().await else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let store = PgUpstreamStatusStore::new(pool);
    let now = Utc::now();
    let pkg = UpstreamKey::package(&reg, "left-pad");
    let ver = UpstreamKey::version(&reg, "left-pad", "1.3.1");

    let long = "x".repeat(LAST_ERROR_MAX_BYTES + 100);
    let first = store
        .record_miss(MissObservation {
            key: pkg,
            at: now,
            error: Some(&long),
        })
        .await
        .unwrap();
    assert_eq!(first.version, None, "'' comes back as the package row");
    assert_eq!(first.consecutive_misses, 1);
    assert_eq!(
        first.last_error.as_deref().map(str::len),
        Some(LAST_ERROR_MAX_BYTES)
    );

    let second = store
        .record_miss(MissObservation {
            key: pkg,
            at: now,
            error: None,
        })
        .await
        .unwrap();
    assert_eq!(second.consecutive_misses, 2);
    assert!(
        second.last_error.is_some(),
        "a miss without an error keeps the last one"
    );

    store
        .record_miss(MissObservation {
            key: ver,
            at: now,
            error: None,
        })
        .await
        .unwrap();
    let got = store.get(&ver).await.unwrap().unwrap();
    assert_eq!(got.version.as_deref(), Some("1.3.1"));
    assert_eq!(got.state, UpstreamState::Missing);

    // Confirm one, list by state, hold keys.
    store.confirm(&ver, now).await.unwrap();
    store.confirm(&ver, now).await.unwrap();
    let confirmed = store.get(&ver).await.unwrap().unwrap();
    assert_eq!(confirmed.state, UpstreamState::Disappeared);
    assert!(confirmed.confirmed_at.is_some());
    assert!(store
        .confirm(&UpstreamKey::version(&reg, "nobody", "1"), now)
        .await
        .is_err());

    let disappeared = store
        .list(UpstreamStatusFilter {
            registry: Some(reg.clone()),
            state: Some(UpstreamState::Disappeared),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(disappeared.len(), 1);
    assert_eq!(
        store.disappeared_keys(&reg).await.unwrap(),
        HashSet::from(["left-pad@1.3.1".to_owned()])
    );
    assert_eq!(
        store
            .count(UpstreamStatusFilter {
                registry: Some(reg.clone()),
                ..Default::default()
            })
            .await
            .unwrap(),
        2
    );

    // Pagination.
    let page = store
        .list(UpstreamStatusFilter {
            registry: Some(reg.clone()),
            state: None,
            limit: 1,
            offset: 1,
        })
        .await
        .unwrap();
    assert_eq!(page.len(), 1);

    // Clear reports what was there.
    let gone = store.clear(&pkg).await.unwrap().unwrap();
    assert_eq!(gone.consecutive_misses, 2);
    assert!(store.clear(&pkg).await.unwrap().is_none());
}
