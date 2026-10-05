//! RFC 0036 §6.3–6.4 against a real PostgreSQL: the seal chain, a row edited
//! with SQL behind its back, and every lifecycle change — pseudonymisation,
//! retention, a purge, an erasure — leaving the chain verifiable.
//!
//! The chain is one per database, so the test makes a database of its own
//! rather than sharing one a previous run sealed.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test pg_audit_lifecycle

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::{PgAdvisoryLeader, PgPackageRepository};
use batlehub_core::entities::{
    AccessAction, AccessEvent, AccessResult, CallerNet, Identity, PackageId, Role, SealKind,
};
use batlehub_core::ports::{AuditTrailStore, PackageRepository};
use batlehub_core::services::audit_trail::{AuditPolicy, AuditTrailService};
use batlehub_core::services::signature::VsxSigningKey;

const SEED: &str = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";
const STEP: i64 = 300;

/// A fresh database next to `DATABASE_URL`'s, and its URL.
async fn fresh_database() -> Option<(String, String)> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let name = format!("audit_{}", Uuid::new_v4().simple());
    let admin = sqlx::PgPool::connect(&url)
        .await
        .expect("connect to postgres");
    // `name` is a UUID this test generated: nothing outside it reaches the SQL.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("create a database for the test");
    let base = url.rsplit_once('/').map_or(url.as_str(), |(b, _)| b);
    Some((format!("{base}/{name}"), name))
}

fn at(secs: i64) -> DateTime<Utc> {
    let t0 = 1_790_000_100 - 1_790_000_100 % STEP;
    Utc.timestamp_opt(t0 + secs, 0).unwrap()
}

fn row(action: AccessAction, user: &str, secs: i64) -> AccessEvent {
    let mut e = AccessEvent::about_identity(
        action,
        Some(user.into()),
        Role::User,
        AccessResult::Allowed,
        CallerNet {
            ip: Some("203.0.113.77".into()),
            user_agent: Some("npm/10.9.0".into()),
        },
        None,
    );
    if action == AccessAction::Download {
        e.package_id = Some(PackageId::new("npm", "left-pad", "1.3.0"));
    }
    e.timestamp = at(secs);
    e
}

/// Each service gets its own lock key: an advisory lock is per session, so
/// two services sharing one in one process would each refuse the other.
fn service(repo: &Arc<PgPackageRepository>, policy: AuditPolicy, key: i64) -> AuditTrailService {
    AuditTrailService::new(
        repo.clone(),
        repo.clone(),
        Arc::new(PgAdvisoryLeader::new(repo.pool(), key)),
        policy,
    )
}

fn sealing() -> AuditPolicy {
    AuditPolicy {
        sealing: Some((
            Duration::from_secs(STEP as u64),
            VsxSigningKey::from_seed_hex(SEED, None).unwrap(),
        )),
        erasure_key: Some("an-erasure-key-that-is-long-enough-0123".into()),
        ..AuditPolicy::default()
    }
}

fn admin() -> Identity {
    Identity {
        user_id: Some("admin".into()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}

#[tokio::test]
async fn the_chain_survives_the_lifecycle_and_catches_an_sql_edit() {
    let Some((url, name)) = fresh_database().await else {
        return;
    };
    let repo = Arc::new(
        PgPackageRepository::new(
            &url,
            PoolOptions {
                max_connections: 4,
                min_connections: 0,
                acquire_timeout_secs: 5,
            },
        )
        .await
        .expect("connect to the test database"),
    );
    repo.run_migrations().await.expect("run migrations");

    for (action, user, secs) in [
        (AccessAction::Download, "alice", 2 * STEP + 5),
        (AccessAction::SignIn, "alice", 2 * STEP + 6),
        (AccessAction::Download, "bob", 3 * STEP + 5),
        (AccessAction::Download, "alice", 4 * STEP + 5),
    ] {
        repo.record_access(row(action, user, secs)).await.unwrap();
    }

    // Seal: the first tick seals the newest closed window, the next ones the rest.
    let svc = service(&repo, sealing(), 0x0036_5ea1);
    assert_eq!(svc.seal_tick(at(4 * STEP)).await.unwrap().len(), 1);
    assert_eq!(svc.seal_tick(at(6 * STEP)).await.unwrap().len(), 2);
    let report = svc.verify(None, None, None).await.unwrap();
    assert!(report.ok, "{:?}", report.failures);
    assert_eq!(report.windows_checked, 3);

    // The lifecycle, sealed windows and all: pseudonymise, then erase, then purge.
    let lifecycle = service(
        &repo,
        AuditPolicy {
            pseudonymise_after: Some(chrono::Duration::seconds(1)),
            ..sealing()
        },
        0x0036_5ea2,
    );
    assert!(
        lifecycle
            .lifecycle_tick(at(6 * STEP))
            .await
            .unwrap()
            .pseudonymised
            >= 3
    );
    let erased = svc
        .erase("alice", false, &admin(), CallerNet::unknown())
        .await
        .unwrap();
    assert!(erased.audit_rows >= 3);
    svc.purge_access_before(at(3 * STEP), &admin(), CallerNet::unknown())
        .await
        .unwrap();
    let kinds: Vec<SealKind> = repo
        .seal_records()
        .await
        .unwrap()
        .iter()
        .map(|r| r.kind)
        .collect();
    assert!(
        kinds.contains(&SealKind::Amend) && kinds.contains(&SealKind::Expire),
        "{kinds:?}"
    );
    let report = svc.verify(None, None, None).await.unwrap();
    assert!(
        report.ok,
        "the lifecycle broke the chain: {:?}",
        report.failures
    );
    let head = report.head.unwrap();

    // The edit an attacker with SQL access makes.
    sqlx::query(
        "UPDATE access_events SET user_id = 'mallory' WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(at(3 * STEP))
    .bind(at(4 * STEP))
    .execute(&repo.pool())
    .await
    .unwrap();
    let report = svc.verify(None, None, Some(&head)).await.unwrap();
    assert!(!report.ok);
    assert_eq!(report.failures[0].window_start, Some(at(3 * STEP)));

    // Undo the edit — the window held bob's download — so what fails next is
    // the truncation alone.
    sqlx::query("UPDATE access_events SET user_id = 'bob' WHERE user_id = 'mallory'")
        .execute(&repo.pool())
        .await
        .unwrap();
    assert!(
        svc.verify(None, None, Some(&head)).await.unwrap().ok,
        "the edit is undone"
    );

    // A truncated tail. Deleting a lifecycle record is caught without help —
    // its window's rows then match no record — so the case §5.3 is about is a
    // tail of plain seals: two more (empty) windows, and the newest dropped.
    assert_eq!(svc.seal_tick(at(8 * STEP)).await.unwrap().len(), 2);
    let head = svc.verify(None, None, None).await.unwrap().head.unwrap();
    sqlx::query("DELETE FROM audit_seals WHERE seq = (SELECT MAX(seq) FROM audit_seals)")
        .execute(&repo.pool())
        .await
        .unwrap();
    assert!(
        svc.verify(None, None, None).await.unwrap().ok,
        "a shorter chain is valid alone"
    );
    let report = svc.verify(None, None, Some(&head)).await.unwrap();
    assert!(!report.ok);
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.reason.contains("truncated")),
        "{:?}",
        report.failures
    );

    drop(repo);
    if let Ok(url) = std::env::var("DATABASE_URL") {
        if let Ok(admin) = sqlx::PgPool::connect(&url).await {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE {name} WITH (FORCE)"
            )))
            .execute(&admin)
            .await;
        }
    }
}
