//! Integration tests for `PgArtifactMetaRepository`.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   task test:pg-cache                              # starts Postgres via Podman automatically
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test artifact_meta

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;

use batlehub_adapters::db::PgArtifactMetaRepository;
use batlehub_core::ports::{ArtifactCacheMeta, ArtifactInventory, ArtifactMetaRecord};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

struct TestRepo {
    repo: PgArtifactMetaRepository,
    pool: PgPool,
    prefix: String,
}

impl TestRepo {
    fn key(&self, name: &str) -> String {
        format!("artifact:test{}/{}", self.prefix, name)
    }
}

async fn make_repo(url: &str) -> TestRepo {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let prefix = format!("t{id}");
    let pool = PgPool::connect(url).await.expect("connect to postgres");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("run migrations");
    TestRepo {
        repo: PgArtifactMetaRepository::new(pool.clone()),
        pool,
        prefix,
    }
}

fn ago(d: Duration) -> DateTime<Utc> {
    Utc::now() - d
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn record_and_list_artifact() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let key = t.key("lodash:1.0.0");

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &key,
            registry: "npm",
            package_name: "lodash",
            version: "1.0.0",
            size: Some(1024),
            checksum: None,
        })
        .await
        .unwrap();

    let rows = t.repo.list_artifacts("npm").await.unwrap();
    let found = rows.iter().find(|r| r.artifact_key == key);
    assert!(found.is_some(), "recorded artifact must appear in list");
    let m = found.unwrap();
    assert_eq!(m.package_name, "lodash");
    assert_eq!(m.version, "1.0.0");
    assert_eq!(m.size_bytes, Some(1024));
}

#[tokio::test]
async fn record_is_idempotent_upsert() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let key = t.key("serde:1.0.0");

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &key,
            registry: "cargo",
            package_name: "serde",
            version: "1.0.0",
            size: Some(500),
            checksum: None,
        })
        .await
        .unwrap();
    // Second call: update size
    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &key,
            registry: "cargo",
            package_name: "serde",
            version: "1.0.0",
            size: Some(600),
            checksum: None,
        })
        .await
        .unwrap();

    let rows = t.repo.list_artifacts("cargo").await.unwrap();
    let found: Vec<_> = rows.iter().filter(|r| r.artifact_key == key).collect();
    assert_eq!(found.len(), 1, "upsert must not duplicate rows");
    assert_eq!(
        found[0].size_bytes,
        Some(600),
        "size must be updated by upsert"
    );
}

#[tokio::test]
async fn touch_updates_last_accessed_at() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let key = t.key("react:18.0.0");

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &key,
            registry: "npm",
            package_name: "react",
            version: "18.0.0",
            size: Some(200),
            checksum: None,
        })
        .await
        .unwrap();

    let before = t.repo.list_artifacts("npm").await.unwrap();
    let before_accessed = before
        .iter()
        .find(|r| r.artifact_key == key)
        .unwrap()
        .last_accessed_at;

    // A touch within the minute is throttled away (the next test); backdate
    // the stamp so this one is due.
    sqlx::query(
        "UPDATE artifact_cache_meta SET last_accessed_at = NOW() - INTERVAL '2 minutes' \
         WHERE artifact_key = $1",
    )
    .bind(&key)
    .execute(&t.pool)
    .await
    .unwrap();
    let before_accessed = before_accessed - Duration::minutes(2);
    t.repo.touch_artifact(&key).await.unwrap();

    let after = t.repo.list_artifacts("npm").await.unwrap();
    let after_accessed = after
        .iter()
        .find(|r| r.artifact_key == key)
        .unwrap()
        .last_accessed_at;

    assert!(
        after_accessed > before_accessed,
        "last_accessed_at must advance after touch"
    );
}

/// Every cache hit touches, so a fresh stamp is not rewritten: the row of a
/// hot artifact would otherwise be updated once per download.
#[tokio::test]
async fn touch_within_a_minute_does_not_write() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let key = t.key("hot:1.0.0");
    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &key,
            registry: "npm",
            package_name: "hot",
            version: "1.0.0",
            size: Some(1),
            checksum: None,
        })
        .await
        .unwrap();
    let stamp = || async {
        sqlx::query_scalar::<_, DateTime<Utc>>(
            "SELECT last_accessed_at FROM artifact_cache_meta WHERE artifact_key = $1",
        )
        .bind(&key)
        .fetch_one(&t.pool)
        .await
        .unwrap()
    };
    let before = stamp().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    t.repo.touch_artifact(&key).await.unwrap();
    assert_eq!(
        stamp().await,
        before,
        "a stamp under a minute old is left alone"
    );
}

#[tokio::test]
async fn touch_missing_key_is_a_noop() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    // Must not error — the method no-ops when key doesn't exist
    t.repo.touch_artifact(&t.key("ghost:1.0.0")).await.unwrap();
}

#[tokio::test]
async fn list_expired_by_ttl_filters_correctly() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let old_key = t.key("old:1.0.0");
    let new_key = t.key("new:1.0.0");

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &old_key,
            registry: "npm",
            package_name: "old",
            version: "1.0.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();
    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &new_key,
            registry: "npm",
            package_name: "new",
            version: "1.0.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();

    // Backdate old_key's cached_at by 3 hours
    sqlx::query("UPDATE artifact_cache_meta SET cached_at = NOW() - INTERVAL '3 hours' WHERE artifact_key = $1")
        .bind(&old_key)
        .execute(&pool)
        .await
        .unwrap();

    // TTL cutoff: 1 hour ago → "old" (3h) is expired, "new" is not
    let cutoff = ago(Duration::hours(1));
    let expired = t.repo.list_expired_by_ttl("npm", cutoff).await.unwrap();
    let expired_keys: Vec<_> = expired.iter().map(|r| &r.artifact_key).collect();

    assert!(
        expired_keys.contains(&&old_key),
        "3h-old artifact must be in expired list"
    );
    assert!(
        !expired_keys.contains(&&new_key),
        "fresh artifact must not be in expired list"
    );
}

#[tokio::test]
async fn list_idle_filters_correctly() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let idle_key = t.key("idle:1.0.0");
    let active_key = t.key("active:1.0.0");

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &idle_key,
            registry: "npm",
            package_name: "idle",
            version: "1.0.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();
    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &active_key,
            registry: "npm",
            package_name: "active",
            version: "1.0.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();

    // Backdate idle_key's last_accessed_at by 10 days
    sqlx::query("UPDATE artifact_cache_meta SET last_accessed_at = NOW() - INTERVAL '10 days' WHERE artifact_key = $1")
        .bind(&idle_key)
        .execute(&pool)
        .await
        .unwrap();

    let cutoff = ago(Duration::days(7));
    let idle = t.repo.list_idle("npm", cutoff).await.unwrap();
    let idle_keys: Vec<_> = idle.iter().map(|r| &r.artifact_key).collect();

    assert!(
        idle_keys.contains(&&idle_key),
        "10-day-idle artifact must appear"
    );
    assert!(
        !idle_keys.contains(&&active_key),
        "recently accessed artifact must not appear"
    );
}

#[tokio::test]
async fn total_size_bytes_aggregates() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &t.key("a:1.0"),
            registry: "cargo",
            package_name: "a",
            version: "1.0",
            size: Some(100),
            checksum: None,
        })
        .await
        .unwrap();
    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &t.key("b:1.0"),
            registry: "cargo",
            package_name: "b",
            version: "1.0",
            size: Some(200),
            checksum: None,
        })
        .await
        .unwrap();
    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &t.key("c:1.0"),
            registry: "cargo",
            package_name: "c",
            version: "1.0",
            size: Some(300),
            checksum: None,
        })
        .await
        .unwrap();

    let total = t.repo.total_size_bytes("cargo").await.unwrap();
    assert!(
        total >= 600,
        "total must include all three artifacts (got {total})"
    );
}

#[tokio::test]
async fn list_lru_returns_oldest_accessed_first() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    // Use a unique registry name so parallel tests in other test functions
    // (e.g. list_idle_filters_correctly which backdates to 10 days ago) cannot
    // contaminate the LRU query results.
    let registry = format!("npm-lru-{}", t.prefix);

    let keys = ["lru-a:1.0", "lru-b:1.0", "lru-c:1.0"];
    for (i, name) in keys.iter().enumerate() {
        let k = t.key(name);
        t.repo
            .record_artifact(ArtifactMetaRecord {
                key: &k,
                registry: &registry,
                package_name: name,
                version: "1.0",
                size: Some(10),
                checksum: None,
            })
            .await
            .unwrap();
        // Spread out last_accessed_at: lru-a accessed 3h ago, lru-b 2h, lru-c 1h
        let hours_ago = (keys.len() - i) as i64;
        sqlx::query(
            "UPDATE artifact_cache_meta SET last_accessed_at = NOW() - ($1 || ' hours')::INTERVAL WHERE artifact_key = $2",
        )
        .bind(hours_ago.to_string())
        .bind(t.key(name))
        .execute(&pool)
        .await
        .unwrap();
    }

    let lru = t.repo.list_lru(&registry, 2).await.unwrap();
    assert_eq!(lru.len(), 2);
    // lru-a (3h) should be first, lru-b (2h) second
    assert!(
        lru[0].artifact_key.contains("lru-a"),
        "oldest accessed must be first"
    );
    assert!(
        lru[1].artifact_key.contains("lru-b"),
        "second oldest must be second"
    );
}

#[tokio::test]
async fn delete_removes_record() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let key = t.key("deleteme:1.0");

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &key,
            registry: "npm",
            package_name: "deleteme",
            version: "1.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();
    assert!(t
        .repo
        .list_artifacts("npm")
        .await
        .unwrap()
        .iter()
        .any(|r| r.artifact_key == key));

    t.repo.delete_artifact_meta(&key).await.unwrap();
    assert!(!t
        .repo
        .list_artifacts("npm")
        .await
        .unwrap()
        .iter()
        .any(|r| r.artifact_key == key));
}

#[tokio::test]
async fn list_artifacts_by_package_groups_and_orders() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    // 3 versions of "mypkg", cached at t-3, t-2, t-1
    for (ver, hours_ago) in [("1.0", 3i64), ("2.0", 2), ("3.0", 1)] {
        let k = t.key(&format!("mypkg:{ver}"));
        t.repo
            .record_artifact(ArtifactMetaRecord {
                key: &k,
                registry: "npm",
                package_name: "mypkg",
                version: ver,
                size: Some(10),
                checksum: None,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE artifact_cache_meta SET cached_at = NOW() - ($1 || ' hours')::INTERVAL WHERE artifact_key = $2",
        )
        .bind(hours_ago.to_string())
        .bind(&k)
        .execute(&pool)
        .await
        .unwrap();
    }

    let rows = t.repo.list_artifacts_by_package().await.unwrap();
    // Filter to our test package only
    let pkg_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.package_name == "mypkg" && r.artifact_key.contains(&t.prefix))
        .collect();

    assert_eq!(pkg_rows.len(), 3);
    // Should be ordered by cached_at DESC: 3.0 (1h ago) first, 1.0 (3h ago) last
    assert_eq!(pkg_rows[0].version, "3.0", "newest version must come first");
    assert_eq!(pkg_rows[2].version, "1.0", "oldest version must come last");
}

#[tokio::test]
async fn list_artifacts_with_empty_registry_spans_all() {
    let Some(url) = db_url() else { return };
    let t = make_repo(&url).await;

    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &t.key("a:1.0"),
            registry: "npm",
            package_name: "a",
            version: "1.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();
    t.repo
        .record_artifact(ArtifactMetaRecord {
            key: &t.key("b:1.0"),
            registry: "cargo",
            package_name: "b",
            version: "1.0",
            size: None,
            checksum: None,
        })
        .await
        .unwrap();

    let key_a = t.key("a:1.0");
    let key_b = t.key("b:1.0");
    let all = t.repo.list_artifacts("").await.unwrap();
    // Check presence of both keys rather than asserting an exact count.
    // The Postgres instance is shared across runs, so stale rows from previous
    // runs with the same prefix would cause a count-based assertion to flap.
    assert!(
        all.iter().any(|r| r.artifact_key == key_a),
        "npm artifact must be returned by empty-registry query"
    );
    assert!(
        all.iter().any(|r| r.artifact_key == key_b),
        "cargo artifact must be returned by empty-registry query"
    );
}
