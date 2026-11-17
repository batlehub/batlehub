//! `scanner_status` on a real database (RFC 0050 §6.2): two workers writing
//! out of order never move a timestamp back, and the error class follows the
//! newer error.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_scanner_status

use chrono::{Duration, Utc};
use sqlx::PgPool;

use batlehub_adapters::db::PgScannerStatusRepository;
use batlehub_core::ports::{ScannerStatus, ScannerStatusRepository};

#[tokio::test]
async fn timestamps_only_move_forward() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    let repo = PgScannerStatusRepository::new(pool);
    let name = format!("osv-t{}", std::process::id());
    // Microsecond precision: what TIMESTAMPTZ keeps.
    let now = Utc::now().with_timezone(&Utc);
    let now = now - Duration::nanoseconds(i64::from(now.timestamp_subsec_nanos() % 1000));
    let row = |ok, err, class: Option<&str>| ScannerStatus {
        scanner: name.clone(),
        last_ok_at: ok,
        last_error_at: err,
        last_error_class: class.map(str::to_owned),
        unavailable_until: None,
    };
    repo.record(&row(Some(now), Some(now), Some("timeout")))
        .await
        .unwrap();
    let older = now - Duration::minutes(5);
    repo.record(&row(Some(older), Some(older), Some("upstream")))
        .await
        .unwrap();
    // A success alone keeps the last error.
    repo.record(&row(Some(older), None, None)).await.unwrap();
    let got = repo
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.scanner == name)
        .expect("the row");
    assert_eq!(got.last_ok_at, Some(now));
    assert_eq!(got.last_error_at, Some(now));
    assert_eq!(got.last_error_class.as_deref(), Some("timeout"));
}

/// Several workers share the row, and writes land out of order: an open
/// breaker only extends, an error without one leaves it, a newer success
/// closes it, and an error older than that success does not reopen it.
#[tokio::test]
async fn the_breaker_only_extends_until_a_success() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    let repo = PgScannerStatusRepository::new(pool);
    let name = format!("osv-b{}", std::process::id());
    let now = Utc::now();
    let now = now - Duration::nanoseconds(i64::from(now.timestamp_subsec_nanos() % 1000));
    let row =
        |ok: bool, at: chrono::DateTime<Utc>, until: Option<chrono::DateTime<Utc>>| ScannerStatus {
            scanner: name.clone(),
            last_ok_at: ok.then_some(at),
            last_error_at: (!ok).then_some(at),
            last_error_class: (!ok).then(|| "unavailable".to_owned()),
            unavailable_until: until,
        };
    let later = now + Duration::minutes(5);
    repo.record(&row(false, now, Some(later))).await.unwrap();
    repo.record(&row(false, now, Some(now + Duration::minutes(1))))
        .await
        .unwrap();
    repo.record(&row(false, now, None)).await.unwrap();
    assert_eq!(until(&repo, &name).await, Some(later));
    let after = now + Duration::seconds(1);
    repo.record(&row(true, after, None)).await.unwrap();
    assert_eq!(until(&repo, &name).await, None);
    repo.record(&row(false, now, Some(later))).await.unwrap();
    assert_eq!(until(&repo, &name).await, None);
}

async fn until(repo: &PgScannerStatusRepository, name: &str) -> Option<chrono::DateTime<Utc>> {
    repo.list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.scanner == name)
        .expect("the row")
        .unavailable_until
}
