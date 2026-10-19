//! `PgClusterState` against a real Postgres: the counters and the replicas'
//! config reports.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test pg_cluster_state

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::{PgClusterState, PgPackageRepository};
use batlehub_core::ports::ClusterStatePort;
use uuid::Uuid;

async fn state() -> Option<PgClusterState> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let repo = PgPackageRepository::new(
        &url,
        PoolOptions {
            max_connections: 2,
            min_connections: 0,
            acquire_timeout_secs: 5,
        },
    )
    .await
    .expect("connect to postgres");
    repo.run_migrations().await.expect("run migrations");
    Some(PgClusterState::new(repo.pool()))
}

#[tokio::test]
async fn counters_and_config_reports_round_trip() {
    let Some(state) = state().await else {
        return;
    };

    // Counters, under a prefix of this run only. `_` must not act as a pattern.
    let run = Uuid::new_v4().simple().to_string();
    let prefix = format!("t_{run}:");
    state.bump(&format!("{prefix}npm")).await.unwrap();
    state.bump(&format!("{prefix}npm")).await.unwrap();
    state.bump(&format!("{prefix}pypi")).await.unwrap();
    state.bump(&format!("tX{run}:other")).await.unwrap();
    let mut got = state.generations(&prefix).await.unwrap();
    got.sort();
    assert_eq!(
        got,
        [(format!("{prefix}npm"), 2), (format!("{prefix}pypi"), 1)]
    );

    // Config reports: the latest per replica, and only the recent ones.
    let (a, b) = (format!("{run}-a"), format!("{run}-b"));
    let since = chrono::Utc::now() - chrono::Duration::seconds(5);
    state.report_config(&a, "x").await.unwrap();
    state.report_config(&a, "y").await.unwrap();
    state.report_config(&b, "x").await.unwrap();
    let mine: Vec<_> = state
        .config_reports(since)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.replica.starts_with(&run))
        .map(|r| (r.replica, r.fingerprint))
        .collect();
    assert_eq!(
        mine,
        [(a.clone(), "y".to_owned()), (b.clone(), "x".to_owned())]
    );
    let later = chrono::Utc::now() + chrono::Duration::seconds(5);
    assert!(state
        .config_reports(later)
        .await
        .unwrap()
        .iter()
        .all(|r| !r.replica.starts_with(&run)));

    // Not left behind.
    sqlx::query("DELETE FROM system_kv WHERE starts_with(key, $1) OR starts_with(key, $2)")
        .bind(format!("t_{run}"))
        .bind(format!("config:replica:{run}"))
        .execute(&state_pool().await)
        .await
        .unwrap();
}

async fn state_pool() -> sqlx::PgPool {
    sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap()
}
