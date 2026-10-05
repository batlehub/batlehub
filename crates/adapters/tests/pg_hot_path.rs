//! The Postgres statements on the artifact-read path, each of which replaced
//! two round trips with one (`tests/heavy/db_calls.sh` counts them):
//!
//! - `covering_block` — the artifact's own block, else its bare version's;
//! - `record_access` — the audit row and the first-seen status row together;
//! - `can_publish` — no owners, a user owner, a group owner: three questions,
//!   one statement (on the publish path).
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test pg_hot_path

use chrono::Utc;
use uuid::Uuid;

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::{PgOwnershipStore, PgPackageRepository};
use batlehub_core::{
    entities::{AccessAction, AccessEvent, AccessResult, Identity, PackageId, PackageStatus, Role},
    ports::{OwnerEntry, OwnershipPort, PackageRepository},
};

async fn repo() -> Option<(PgPackageRepository, String)> {
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
    // Unique per run: the database outlives it.
    Some((repo, format!("hot-{}", Uuid::new_v4().simple())))
}

fn blocked(reason: &str) -> PackageStatus {
    PackageStatus::Blocked {
        reason: reason.to_owned(),
        blocked_by: "admin".to_owned(),
        blocked_at: Utc::now(),
    }
}

#[tokio::test]
async fn covering_block_prefers_the_artifact_then_widens_to_the_version() {
    let Some((repo, reg)) = repo().await else {
        return;
    };
    let file = PackageId::new(&reg, "pkg", "1.0.0").with_artifact("tarball");
    let version = PackageId::new(&reg, "pkg", "1.0.0");

    assert_eq!(repo.covering_block(&file).await.unwrap(), None);

    repo.set_status(&version, blocked("version")).await.unwrap();
    assert_eq!(
        repo.covering_block(&file).await.unwrap().as_deref(),
        Some("version")
    );
    assert_eq!(
        repo.covering_block(&version).await.unwrap().as_deref(),
        Some("version")
    );

    repo.set_status(&file, blocked("artifact")).await.unwrap();
    assert_eq!(
        repo.covering_block(&file).await.unwrap().as_deref(),
        Some("artifact")
    );

    // A block on one file does not cover the version's other files.
    repo.set_status(&version, PackageStatus::Available)
        .await
        .unwrap();
    let other = PackageId::new(&reg, "pkg", "1.0.0").with_artifact("sig");
    assert_eq!(repo.covering_block(&other).await.unwrap(), None);
    // Nor another version.
    let next = PackageId::new(&reg, "pkg", "2.0.0").with_artifact("tarball");
    assert_eq!(repo.covering_block(&next).await.unwrap(), None);
}

fn event(pkg: &PackageId, result: AccessResult) -> AccessEvent {
    AccessEvent {
        id: Uuid::new_v4(),
        user_id: None,
        user_role: Role::Anonymous,
        package_id: Some(pkg.clone()),
        action: AccessAction::Download,
        result,
        timestamp: Utc::now(),
        ip_address: None,
        user_agent: None,
        throttled_count: None,
        detail: None,
    }
}

#[tokio::test]
async fn record_access_writes_the_status_row_only_for_an_allowed_read() {
    let Some((repo, reg)) = repo().await else {
        return;
    };
    let denied = PackageId::new(&reg, "denied", "1.0.0");
    let allowed = PackageId::new(&reg, "allowed", "1.0.0");

    repo.record_access(event(
        &denied,
        AccessResult::Denied {
            reason: "no".into(),
        },
    ))
    .await
    .unwrap();
    repo.record_access(event(&allowed, AccessResult::Allowed))
        .await
        .unwrap();
    // The second read of a coordinate is a no-op on the status row, not a conflict.
    repo.record_access(event(&allowed, AccessResult::Allowed))
        .await
        .unwrap();

    let names: Vec<String> = sqlx::query_scalar(
        "SELECT package_name FROM package_statuses WHERE registry = $1 ORDER BY package_name",
    )
    .bind(&reg)
    .fetch_all(&repo.pool())
    .await
    .unwrap();
    assert_eq!(names, vec!["allowed".to_owned()]);

    // An existing block survives the read that would have created the row.
    let blocked_pkg = PackageId::new(&reg, "blocked", "1.0.0");
    repo.set_status(&blocked_pkg, blocked("held"))
        .await
        .unwrap();
    repo.record_access(event(&blocked_pkg, AccessResult::Allowed))
        .await
        .unwrap();
    assert!(matches!(
        repo.get_status(&blocked_pkg).await.unwrap(),
        PackageStatus::Blocked { .. }
    ));
}

fn who(user: Option<&str>, groups: &[&str]) -> Identity {
    Identity {
        user_id: user.map(str::to_owned),
        role: Role::User,
        auth_provider: None,
        groups: groups.iter().map(|g| (*g).to_owned()).collect(),
    }
}

fn owner(kind: &str, id: &str) -> OwnerEntry {
    OwnerEntry {
        principal_type: kind.to_owned(),
        principal_id: id.to_owned(),
        role: "maintainer".to_owned(),
        granted_by: None,
    }
}

#[tokio::test]
async fn can_publish_answers_the_three_ownership_questions_in_one() {
    let Some((repo, reg)) = repo().await else {
        return;
    };
    let owners = PgOwnershipStore::new(repo.pool());
    let (alice, bob, nobody) = (
        who(Some("alice"), &[]),
        who(Some("bob"), &["team"]),
        who(None, &[]),
    );

    // No owners yet: anyone may publish, even with no user id.
    for id in [&alice, &bob, &nobody] {
        assert!(owners.can_publish(&reg, "pkg", id).await.unwrap());
    }

    owners
        .add_owner(&reg, "pkg", owner("user", "alice"))
        .await
        .unwrap();
    assert!(owners.can_publish(&reg, "pkg", &alice).await.unwrap());
    assert!(!owners.can_publish(&reg, "pkg", &bob).await.unwrap());
    assert!(
        !owners.can_publish(&reg, "pkg", &nobody).await.unwrap(),
        "a NULL user id owns nothing"
    );

    owners
        .add_owner(&reg, "pkg", owner("group", "team"))
        .await
        .unwrap();
    assert!(
        owners.can_publish(&reg, "pkg", &bob).await.unwrap(),
        "through the group"
    );
    assert!(!owners
        .can_publish(&reg, "pkg", &who(Some("carol"), &["other"]))
        .await
        .unwrap());
    // Ownership is per package.
    assert!(owners.can_publish(&reg, "other-pkg", &bob).await.unwrap());
}
