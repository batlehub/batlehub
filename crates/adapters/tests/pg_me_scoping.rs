//! Integration tests for the two Postgres queries that decide what
//! `/api/v1/me/*` shows a caller (RFC 0004 §6.2, §7).
//!
//! These are worth a real database rather than an in-memory double: both are
//! hand-written SQL whose `WHERE` clause *is* the security boundary, and an
//! in-memory implementation agreeing with itself proves nothing about the
//! statement that actually runs in production.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   task test:pg-me-scoping
//!   DATABASE_URL=postgresql://postgres:pass@localhost/postgres \
//!     cargo test -p batlehub-adapters --test pg_me_scoping

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::{PgOwnershipStore, PgPackageRepository};
use batlehub_core::{
    entities::{AccessAction, AccessEvent, AccessResult, Identity, PackageId, Role},
    ports::{OwnerEntry, OwnershipPort, PackageRepository},
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

struct TestCtx {
    repo: PgPackageRepository,
    ownership: PgOwnershipStore,
    prefix: String,
}

impl TestCtx {
    /// A registry name unique to this test, so parallel runs never collide.
    fn reg(&self) -> String {
        format!("npm-{}", self.prefix)
    }
    /// A user id unique to this test.
    fn user(&self, who: &str) -> String {
        format!("{who}-{}", self.prefix)
    }
    fn group(&self, name: &str) -> String {
        format!("{name}-{}", self.prefix)
    }
}

/// Unique per *run* as well as per test: `access_events` is append-only, so a
/// user id reused by a later run against the same database still owns the
/// earlier run's downloads and the exact-match assertions below see both. The
/// process id supplies that, as it does in `pg_explore_resolution.rs`.
async fn make_ctx(url: &str) -> TestCtx {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let repo = PgPackageRepository::new(
        url,
        PoolOptions {
            max_connections: 4,
            min_connections: 1,
            acquire_timeout_secs: 10,
        },
    )
    .await
    .expect("connect to postgres");
    repo.run_migrations().await.expect("run migrations");
    let pool: PgPool = repo.pool();
    TestCtx {
        ownership: PgOwnershipStore::new(pool),
        repo,
        prefix: format!("t{pid}-{id}"),
    }
}

async fn record_download(
    ctx: &TestCtx,
    user: &str,
    name: &str,
    version: &str,
    ago: Duration,
    result: AccessResult,
    action: AccessAction,
) {
    ctx.repo
        .record_access(AccessEvent {
            id: Uuid::new_v4(),
            user_id: Some(user.to_owned()),
            user_role: Role::User,
            package_id: Some(PackageId::new(ctx.reg(), name, version)),
            action,
            result,
            timestamp: Utc::now() - ago,
            ip_address: None,
            user_agent: None,
            throttled_count: None,
            detail: None,
        })
        .await
        .unwrap();
}

async fn pull(ctx: &TestCtx, user: &str, name: &str, version: &str, ago: Duration) {
    record_download(
        ctx,
        user,
        name,
        version,
        ago,
        AccessResult::Allowed,
        AccessAction::Download,
    )
    .await;
}

// ── list_own_downloads ────────────────────────────────────────────────────────

#[tokio::test]
async fn list_own_downloads_returns_no_other_users_rows() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let ctx = make_ctx(&url).await;
    let (alice, bob) = (ctx.user("alice"), ctx.user("bob"));

    pull(&ctx, &alice, "alice-dep", "1.0.0", Duration::minutes(1)).await;
    pull(&ctx, &bob, "bob-dep", "2.0.0", Duration::minutes(1)).await;

    let rows = ctx
        .repo
        .list_own_downloads(&alice, Utc::now() - Duration::days(1), 50)
        .await
        .unwrap();

    let names: Vec<String> = rows
        .iter()
        .map(|e| e.package_id.as_ref().unwrap().name.clone())
        .collect();
    assert!(
        !names.iter().any(|n| n == "bob-dep"),
        "the WHERE clause is the security boundary; got {names:?}"
    );
    assert_eq!(names, vec!["alice-dep".to_owned()]);
}

#[tokio::test]
async fn list_own_downloads_excludes_denied_and_non_download_actions() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let ctx = make_ctx(&url).await;
    let alice = ctx.user("alice");

    pull(&ctx, &alice, "allowed", "1.0.0", Duration::minutes(3)).await;
    record_download(
        &ctx,
        &alice,
        "denied",
        "1.0.0",
        Duration::minutes(2),
        AccessResult::Denied {
            reason: "blocklisted".to_owned(),
        },
        AccessAction::Download,
    )
    .await;
    record_download(
        &ctx,
        &alice,
        "viewed",
        "1.0.0",
        Duration::minutes(1),
        AccessResult::Allowed,
        AccessAction::ViewMetadata,
    )
    .await;

    let rows = ctx
        .repo
        .list_own_downloads(&alice, Utc::now() - Duration::days(1), 50)
        .await
        .unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|e| e.package_id.as_ref().unwrap().name.clone())
        .collect();
    assert_eq!(
        names,
        vec!["allowed".to_owned()],
        "a refused pull is not a pull, and a metadata view is not a download"
    );
}

#[tokio::test]
async fn list_own_downloads_honours_window_and_limit_newest_first() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let ctx = make_ctx(&url).await;
    let alice = ctx.user("alice");

    pull(&ctx, &alice, "newest", "1.0.0", Duration::minutes(1)).await;
    pull(&ctx, &alice, "older", "1.0.0", Duration::hours(2)).await;
    pull(&ctx, &alice, "ancient", "1.0.0", Duration::days(10)).await;

    let since = Utc::now() - Duration::days(7);
    let rows = ctx
        .repo
        .list_own_downloads(&alice, since, 50)
        .await
        .unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|e| e.package_id.as_ref().unwrap().name.clone())
        .collect();
    assert_eq!(names, vec!["newest".to_owned(), "older".to_owned()]);

    let capped = ctx.repo.list_own_downloads(&alice, since, 1).await.unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].package_id.as_ref().unwrap().name, "newest");
}

// ── list_owned_by ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_owned_by_returns_no_other_principals_packages() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let ctx = make_ctx(&url).await;
    let (alice, bob) = (ctx.user("alice"), ctx.user("bob"));
    let reg = ctx.reg();

    ctx.ownership
        .initialize_owner(&reg, "alice-lib", &alice)
        .await
        .unwrap();
    ctx.ownership
        .initialize_owner(&reg, "bob-lib", &bob)
        .await
        .unwrap();

    let owned = ctx
        .ownership
        .list_owned_by(&Identity {
            user_id: Some(alice.clone()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        })
        .await
        .unwrap();

    assert_eq!(owned, vec![(reg, "alice-lib".to_owned())]);
}

#[tokio::test]
async fn list_owned_by_includes_group_owned_packages() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let ctx = make_ctx(&url).await;
    let (alice, team) = (ctx.user("alice"), ctx.group("team-a"));
    let reg = ctx.reg();

    ctx.ownership
        .add_owner(
            &reg,
            "team-lib",
            OwnerEntry {
                principal_type: "group".to_owned(),
                principal_id: team.clone(),
                role: "admin".to_owned(),
                granted_by: None,
            },
        )
        .await
        .unwrap();

    let member = Identity {
        user_id: Some(alice.clone()),
        role: Role::User,
        auth_provider: None,
        groups: vec![team],
    };
    assert_eq!(
        ctx.ownership.list_owned_by(&member).await.unwrap(),
        vec![(reg, "team-lib".to_owned())]
    );

    // Same user, no group membership: the row is not theirs.
    let outsider = Identity {
        user_id: Some(alice),
        role: Role::User,
        auth_provider: None,
        groups: vec![],
    };
    assert!(ctx
        .ownership
        .list_owned_by(&outsider)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn list_owned_by_is_empty_for_an_anonymous_caller() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let ctx = make_ctx(&url).await;
    ctx.ownership
        .initialize_owner(&ctx.reg(), "some-lib", &ctx.user("alice"))
        .await
        .unwrap();

    // No user id and no groups: the query must short-circuit rather than
    // compare `principal_id` against NULL for every row.
    let anonymous = Identity {
        user_id: None,
        role: Role::Anonymous,
        auth_provider: None,
        groups: vec![],
    };
    assert!(ctx
        .ownership
        .list_owned_by(&anonymous)
        .await
        .unwrap()
        .is_empty());
}
