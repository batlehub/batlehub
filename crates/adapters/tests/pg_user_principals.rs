//! The startup step that scopes legacy `user:<id>` grants and user owners to
//! their provider. Its own binary: it rewrites rows across every registry, and
//! `pg_grants.rs`'s tests running beside it would see their subjects change.
//!
//! Skipped when `DATABASE_URL` is unset, like the other `pg_*` suites.

use batlehub_adapters::db::governance::grants::{scope_bare_user_principals, BareUserPrincipals};

async fn pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect to DATABASE_URL");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    Some(pool)
}

async fn seed(pool: &sqlx::PgPool, reg: &str) {
    sqlx::query("DELETE FROM grants WHERE registry = $1")
        .bind(reg)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM package_owners WHERE registry = $1")
        .bind(reg)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM beta_channel_members WHERE registry = $1")
        .bind(reg)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO beta_channel_members (registry, principal_type, principal_id) \
         VALUES ($1, 'user', 'dave'), ($1, 'group', 'oidc:beta')",
    )
    .bind(reg)
    .execute(pool)
    .await
    .unwrap();
    for subject in [
        "user:alice",
        "user:corp:bob",
        "user:carol",
        "user:corp:carol",
    ] {
        sqlx::query(
            "INSERT INTO grants (registry, node_kind, node_key, subject, actions) \
             VALUES ($1, 'package', 'pkg', $2, ARRAY['releases:read'])",
        )
        .bind(reg)
        .bind(subject)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO package_owners (registry, package_name, principal_type, principal_id, role) \
         VALUES ($1, 'pkg', 'user', 'alice', 'admin'), ($1, 'pkg', 'group', 'eng', 'maintainer')",
    )
    .bind(reg)
    .execute(pool)
    .await
    .unwrap();
}

async fn subjects(pool: &sqlx::PgPool, reg: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT subject FROM grants WHERE registry = $1 ORDER BY subject")
        .bind(reg)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn one_provider_scopes_bare_rows_and_several_leave_them_inert() {
    let Some(pool) = pool().await else {
        eprintln!("DATABASE_URL unset: skipping");
        return;
    };
    let reg = "user-principals-test";

    // Several providers: nothing is rewritten, the bare rows are counted.
    seed(&pool, reg).await;
    let two = vec!["corp".to_owned(), "signup".to_owned()];
    let r = scope_bare_user_principals(&pool, &two).await.unwrap();
    assert!(r.left >= 4, "{r:?}"); // alice, carol, the alice owner, the dave beta member
    assert_eq!(r.scoped, 0);
    assert!(subjects(&pool, reg)
        .await
        .contains(&"user:alice".to_owned()));

    // One provider: bare rows become `<provider>:<id>`; a bare row whose
    // scoped twin exists is left (and counted) rather than colliding.
    let one = vec!["corp".to_owned()];
    let r = scope_bare_user_principals(&pool, &one).await.unwrap();
    assert!(r.scoped >= 3, "{r:?}");
    assert_eq!(
        subjects(&pool, reg).await,
        vec![
            "user:carol",
            "user:corp:alice",
            "user:corp:bob",
            "user:corp:carol"
        ]
    );
    let owners: Vec<String> = sqlx::query_scalar(
        "SELECT principal_id FROM package_owners WHERE registry = $1 ORDER BY principal_id",
    )
    .bind(reg)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(owners, vec!["corp:alice", "eng"]);
    let beta: Vec<String> = sqlx::query_scalar(
        "SELECT principal_id FROM beta_channel_members WHERE registry = $1 ORDER BY principal_id",
    )
    .bind(reg)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        beta,
        vec!["corp:dave", "oidc:beta"],
        "group members are untouched"
    );

    // Idempotent.
    let again = scope_bare_user_principals(&pool, &one).await.unwrap();
    assert_eq!(again.scoped, 0);
    let _ = BareUserPrincipals::default();
}
