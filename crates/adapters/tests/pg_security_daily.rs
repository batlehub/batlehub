//! `security_daily` and its source queries on a real database (RFC 0050
//! §5.2): the state counts, a day's refusals by code, NULL never overwriting
//! a stored number, and pruning.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_security_daily

use chrono::{Duration, Utc};
use sqlx::PgPool;

use batlehub_adapters::db::{PgSecurityDailyRepository, PgVerdictRepository};
use batlehub_core::entities::{PackageId, ReasonCode, Verdict, VerdictState};
use batlehub_core::ports::{DailyEvents, SecurityDailyRepository, VerdictRepository};

#[tokio::test]
async fn the_rollup_sql_counts_and_keeps_its_rows() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    let reg = format!("daily-t{}", std::process::id());
    for table in ["artifact_verdicts", "access_events", "security_daily"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE registry = $1"
        )))
        .bind(&reg)
        .execute(&pool)
        .await
        .expect("clear");
    }
    let verdicts = PgVerdictRepository::new(pool.clone());
    for (name, state) in [("a", VerdictState::Denied), ("b", VerdictState::Allowed)] {
        verdicts
            .upsert(&Verdict {
                package: PackageId::new(&reg, name, "1.0.0"),
                state,
                reason_codes: vec![ReasonCode::Vulnerability],
                findings: Vec::new(),
                policy_ref: format!("{reg}/default"),
                available_at: None,
                evaluated_at: Utc::now(),
                last_scanned_at: Some(Utc::now()),
                scanners_done: vec!["osv".into()],
            })
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO access_events (id, registry, package_name, package_version, action, outcome, reason_code, created_at)
         VALUES ($1, $2, 'a', '1.0.0', 'download', 'denied', 'VULNERABILITY', NOW()),
                ($3, $2, 'a', '1.0.0', 'download', 'denied', NULL, NOW())",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(&reg)
    .bind(uuid::Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();

    let repo = PgSecurityDailyRepository::new(pool);
    let state = repo.compute_state().await.unwrap();
    let mine = &state
        .iter()
        .find(|(r, _)| *r == reg)
        .expect("a state row")
        .1;
    assert_eq!((mine.denied, mine.allowed), (1, 1));

    let today = Utc::now().date_naive();
    let events = repo
        .compute_events(today, std::slice::from_ref(&reg))
        .await
        .unwrap();
    // A refusal without a code is not counted (§5.2's column definition).
    assert_eq!(events[0].refusals, Some(1));
    assert_eq!(
        events[0].refusals_by_code.as_ref().unwrap()["VULNERABILITY"],
        1
    );

    repo.upsert_events(&reg, today, &events[0]).await.unwrap();
    repo.upsert_state(&reg, today, mine).await.unwrap();
    // A later NULL never erases the stored number.
    repo.upsert_events(&reg, today, &DailyEvents::default())
        .await
        .unwrap();
    let rows = repo.range(today, today).await.unwrap();
    let row = rows.iter().find(|r| r.registry == reg).expect("the row");
    assert_eq!(row.events.refusals, Some(1));
    assert_eq!(row.state.as_ref().unwrap().denied, 1);

    // An old day is pruned.
    let old = today - Duration::days(400);
    repo.upsert_events(&reg, old, &events[0]).await.unwrap();
    assert!(repo.prune_before(today - Duration::days(30)).await.unwrap() >= 1);
    assert!(repo
        .range(old, old)
        .await
        .unwrap()
        .iter()
        .all(|r| r.registry != reg));
}

/// A purge removes the access class only: a surviving security-class row
/// does not make a purged day read as zero refusals, and a recompute of a
/// partly purged day never lowers the stored count.
#[tokio::test]
async fn a_purged_day_is_null_and_a_stored_count_never_shrinks() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    let reg = format!("daily-purge-t{}", std::process::id());
    for table in ["access_events", "security_daily"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE registry = $1"
        )))
        .bind(&reg)
        .execute(&pool)
        .await
        .expect("clear");
    }
    // Evidence the purge keeps, 3000 days old, and today's traffic (assumes
    // no download row that old in the test database: pg_retention's oldest
    // is ~500 days).
    sqlx::query(
        "INSERT INTO access_events (id, registry, package_name, action, outcome, created_at)
         VALUES ($1, $2, 'a', 'block', 'allowed', NOW() - INTERVAL '3000 days'),
                ($3, $2, 'a', 'download', 'allowed', NOW())",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(&reg)
    .bind(uuid::Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    let repo = PgSecurityDailyRepository::new(pool.clone());
    let purged = Utc::now().date_naive() - Duration::days(2500);
    let events = repo
        .compute_events(purged, std::slice::from_ref(&reg))
        .await
        .unwrap();
    assert_eq!(events[0].refusals, None, "beyond the access class: NULL");

    let day = Utc::now().date_naive() - Duration::days(1);
    let full = DailyEvents {
        refusals: Some(5),
        refusals_by_code: Some([("VULNERABILITY".to_owned(), 5)].into()),
        jobs_done: Some(4),
        ..Default::default()
    };
    let tail = DailyEvents {
        refusals: Some(2),
        refusals_by_code: Some([("VULNERABILITY".to_owned(), 2)].into()),
        jobs_done: Some(1),
        ..Default::default()
    };
    repo.upsert_events(&reg, day, &full).await.unwrap();
    repo.upsert_events(&reg, day, &tail).await.unwrap();
    let rows = repo.range(day, day).await.unwrap();
    let row = rows.iter().find(|r| r.registry == reg).expect("the row");
    assert_eq!(row.events.refusals, Some(5));
    assert_eq!(row.events.refusals_by_code, full.refusals_by_code);
    assert_eq!(row.events.jobs_done, Some(4));
    sqlx::query("DELETE FROM access_events WHERE registry = $1")
        .bind(&reg)
        .execute(&pool)
        .await
        .unwrap();
}
