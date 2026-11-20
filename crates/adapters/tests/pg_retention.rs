//! Integration tests for the retention run against a real database and a real
//! store (RFC 0016 §10).
//!
//! # What is here, and what is deliberately not
//!
//! *Which versions survive* is arithmetic over a policy and three facts about a
//! version, and it is asserted exhaustively in
//! `crates/core/src/services/retention/tests.rs` against in-memory doubles.
//! Repeating any of it here would buy nothing, so this file does not: there is no
//! `keep_for` test, no `keep_yanked` test, no floor-date test, and no
//! "refuses without a download signal" test.
//!
//! What is here is the set of claims the doubles **cannot** make, because each one
//! is a statement about SQL rather than about the decision:
//!
//! | Claim | Why a double cannot make it |
//! | --- | --- |
//! | The download signal round-trips | `last_downloads` filters `action = 'download' AND outcome = 'allowed'` — string literals that have to agree with what `action_to_str` and `record_access` *write*. The core double reimplements that filter in Rust (its own comment says "the same three constraints the Postgres query applies"), so the two can drift and every core test stays green while retention reads nothing. |
//! | The sidecar split survives the round-trip | Same seam, from the other side: `is_verification_sidecar` picks `ViewMetadata`, which becomes `'view_metadata'` in a column the query excludes by name. |
//! | `DISTINCT ON` returns the *newest* pull | `ORDER BY package_version, created_at DESC` is real SQL. Reversed, it returns a version's oldest pull, and an actively-used version reads as stale. The double uses a max over a `HashMap` and cannot have this bug. |
//! | `keep_versions` keeps the newest N | The weakest entry here, and it is kept deliberately. Both halves are already guarded separately — `local_registry.rs`'s `get_versions_returns_in_published_at_order` pins the adapter's `ORDER BY published_at ASC`, and the core suite pins the `total - 1 - i` rank arithmetic against its own double. Nothing joins them: the link is a comment in `retention/mod.rs` saying "`get_versions` is `published_at ASC`; rank 0 must be the newest". A deliberate reordering that updated the adapter's own test would leave retention silently deleting the newest N, so this is the test that records the dependency. |
//! | The pin is durable | `set_retention_keep` is an `UPDATE` and `retention_keep` is a column `get_versions` decodes. §13.8 calls the pin "phase 3's most important safety property"; nothing exercised its SQL. |
//! | `dry_run` writes nothing | §10 asks for this "by counting rows *and stored objects* before and after, then running live and comparing against the report". The core test counts rows in a `Vec` against a storage double whose `delete` returns `Ok(true)` without doing anything. |
//!
//! Tombstone mechanics — that a reclamation spends the coordinate, that the row
//! survives, that a republish is refused — belong to `delete_version` and are
//! asserted in `pg_tombstones.rs`. This file asserts that the *run* reaches them.
//!
//!   task test:pg-retention
//!   DATABASE_URL=postgresql://user:pass@localhost/db \
//!     cargo test -p batlehub-adapters --test pg_retention

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};
use sqlx::{PgPool, Row};

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::PgPackageRepository;
use batlehub_adapters::local_registry::PostgresLocalRegistry;
use batlehub_adapters::storage::FilesystemStorageBackend;
use batlehub_core::entities::{
    AccessAction, AccessEvent, EventFilter, Identity, PackageId, PublishedPackage, Role, Visibility,
};
use batlehub_core::ports::{PackageRepository, StorageBackend, StorageMeta};
use batlehub_core::services::{
    artifact_storage_key, new_hot_lock, HotConfig, LocalRegistryService, RetentionReport,
    RetentionRunPolicy, RetentionService,
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

fn admin() -> Identity {
    Identity {
        user_id: Some("admin-1".to_owned()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}

fn days(n: u64) -> Duration {
    Duration::from_secs(n * 86_400)
}

/// The policy every test starts from: nothing configured, so nothing reclaimed.
///
/// The floor sits far enough back that it never fires unless a test moves it,
/// for the reason §13.9 gives — it is consulted only when `keep_if_pulled` is
/// set, and a floor that fired by accident would hide a missing download signal
/// behind a keep.
fn policy() -> RetentionRunPolicy {
    RetentionRunPolicy {
        keep_versions: None,
        keep_for: None,
        keep_if_pulled: None,
        keep_yanked: true,
        download_signal_floor: Utc::now() - ChronoDuration::days(10_000),
        reclaim_delay: Duration::ZERO,
        dry_run: true,
    }
}

struct TestEstate {
    local: Arc<LocalRegistryService>,
    repo: Arc<PgPackageRepository>,
    pool: PgPool,
    storage: Arc<FilesystemStorageBackend>,
    registry: String,
}

/// A registry name unique per *run*, for the reason `pg_tombstones.rs` gives:
/// these tests write and delete by `(registry, name, version)`, so a second run
/// against the same database would read the previous run's rows. Pids are
/// recycled, so the fixture also clears anything a long-dead namesake left —
/// from `access_events` as well as `local_packages`, because a stale download
/// row is exactly the kind of thing that would keep a version this run expects
/// to see reclaimed.
async fn make_estate(url: &str) -> TestEstate {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let registry = format!("ret-t{}-{id}", std::process::id());

    let pool = PgPool::connect(url).await.expect("connect to postgres");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("run migrations");
    for stmt in [
        "DELETE FROM local_packages WHERE registry = $1",
        "DELETE FROM access_events WHERE registry = $1",
    ] {
        sqlx::query(stmt)
            .bind(&registry)
            .execute(&pool)
            .await
            .expect("clear rows from a previous run under this registry name");
    }

    let repo = Arc::new(
        PgPackageRepository::new(
            url,
            PoolOptions {
                max_connections: 4,
                min_connections: 1,
                acquire_timeout_secs: 10,
            },
        )
        .await
        .expect("package repository"),
    );

    // A real store rather than a double: §10 asks for the dry-run assertion to
    // count stored objects, and a `delete` that returns `Ok(true)` without
    // removing anything would satisfy a double perfectly.
    let dir = std::env::temp_dir().join(format!("batlehub-retention-{}-{id}", std::process::id()));
    let storage = Arc::new(
        FilesystemStorageBackend::new(dir)
            .await
            .expect("filesystem store"),
    );

    let local = Arc::new(LocalRegistryService {
        backend: Arc::new(PostgresLocalRegistry::new(pool.clone())),
        storage: storage.clone(),
        hot: new_hot_lock(HotConfig::default()),
        quota: None,
        ownership: None,
        team_namespace: None,
        sbom: None,
        explore_cache: None,
        // Wired, so a reclamation records its `retention_reclaim` event
        // exactly as the server does. That is also a quiet assertion in its own
        // right: the row lands in the same table `last_downloads` reads, and a
        // query that filtered on the wrong column would start counting it.
        package_repo: Some(repo.clone()),
        readme: None,
    });

    TestEstate {
        local,
        repo,
        pool,
        storage,
        registry,
    }
}

impl TestEstate {
    /// Publish a version `age_days` old, with bytes in the store at the key
    /// `delete_version` will go looking for.
    async fn publish(&self, name: &str, version: &str, age_days: i64) {
        let pkg = PublishedPackage {
            registry: self.registry.clone(),
            name: name.to_owned(),
            version: version.to_owned(),
            checksum: format!("sum-{name}-{version}"),
            yanked: false,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata: serde_json::json!({}),
            published_at: Utc::now() - ChronoDuration::days(age_days),
            published_by: Some("publisher-1".to_owned()),
            signature_bytes: None,
            signature_type: None,
            visibility: Visibility::Public,
            retention_keep: false,
        };
        self.local.backend.publish(pkg).await.unwrap();
        self.local
            .backend
            .commit_publish(&self.registry, name, version)
            .await
            .unwrap();
        self.storage
            .store(
                &self.key(name, version),
                Bytes::from_static(b"artifact bytes"),
                StorageMeta {
                    content_type: None,
                    size: Some(14),
                    checksum: None,
                },
            )
            .await
            .unwrap();
    }

    fn key(&self, name: &str, version: &str) -> String {
        artifact_storage_key(&self.registry, name, version)
    }

    /// Record a read through [`AccessEvent::allowed_read`] — the one function
    /// that draws the sidecar split — and write it with the real
    /// `record_access`, so the whole chain from `is_verification_sidecar` to the
    /// column `last_downloads` filters on is exercised.
    ///
    /// `artifact` is what makes the coordinate a sidecar or not: `None` is the
    /// artifact itself, `Some("x.jar.sha1")` is a checksum beside it.
    async fn record_read(&self, name: &str, version: &str, artifact: Option<&str>, age_days: i64) {
        let mut pkg = PackageId::new(&self.registry, name, version);
        if let Some(a) = artifact {
            pkg = pkg.with_artifact(a);
        }
        let mut event = AccessEvent::allowed_read(pkg, Some("consumer-1".to_owned()), Role::User);
        // The constructors stamp `Utc::now()`; a test about windows has to place
        // the event in time itself. `record_access` binds this to `created_at`,
        // which is the column the query orders and the run compares.
        event.timestamp = Utc::now() - ChronoDuration::days(age_days);
        self.repo.record_access(event).await.unwrap();
    }

    /// A refused download, for the §13.9 line that a denial is not use.
    async fn record_denied(&self, name: &str, version: &str, age_days: i64) {
        let mut event = AccessEvent::denied_download(
            PackageId::new(&self.registry, name, version),
            Some("consumer-1".to_owned()),
            Role::User,
            "blocked".to_owned(),
        );
        event.timestamp = Utc::now() - ChronoDuration::days(age_days);
        self.repo.record_access(event).await.unwrap();
    }

    async fn run(&self, policy: &RetentionRunPolicy) -> RetentionReport {
        RetentionService::new(self.local.clone(), Some(self.repo.clone()))
            .run(&self.registry, policy, &admin())
            .await
            .expect("the run must not error")
    }

    /// Live rows, by coordinate. What a resolver would still see.
    async fn live_versions(&self, name: &str) -> Vec<String> {
        self.local
            .backend
            .get_versions(&self.registry, name)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.version)
            .collect()
    }

    /// Every row for this registry, tombstones included — the count that says
    /// whether a dry run wrote anything at all.
    async fn total_rows(&self) -> i64 {
        sqlx::query("SELECT COUNT(*) AS n FROM local_packages WHERE registry = $1")
            .bind(&self.registry)
            .fetch_one(&self.pool)
            .await
            .unwrap()
            .get::<i64, _>("n")
    }

    async fn tombstone_count(&self) -> i64 {
        sqlx::query(
            "SELECT COUNT(*) AS n FROM local_packages \
             WHERE registry = $1 AND deleted_at IS NOT NULL",
        )
        .bind(&self.registry)
        .fetch_one(&self.pool)
        .await
        .unwrap()
        .get::<i64, _>("n")
    }

    async fn stored(&self, name: &str, version: &str) -> bool {
        self.storage.exists(&self.key(name, version)).await.unwrap()
    }
}

/// Every test starts the same way, and skipping is not failing.
macro_rules! estate {
    () => {{
        let Some(url) = db_url() else {
            eprintln!("skipping: DATABASE_URL not set");
            return;
        };
        make_estate(&url).await
    }};
}

// ── The download signal, through the query that actually reads it ─────────────

/// **The seam this file exists for.** `keep_if_pulled` is the veto §4.3 calls
/// "the rule that makes retention safe to switch on", and it is the only keep
/// condition whose evidence lives in another table, written by another code
/// path, and read back by hand-written SQL.
///
/// A recent pull must keep a version that every other condition would reclaim —
/// here it is the *oldest* version and the policy is `keep_versions = 1`, so
/// nothing but the download signal can save it.
#[tokio::test]
async fn a_recent_pull_recorded_through_record_access_keeps_a_version() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;
    e.record_read("p", "1.0.0", None, 3).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.keep_if_pulled = Some(days(90));

    let report = e.run(&p).await;
    assert_eq!(
        report.reclaimed, 0,
        "the pull must reach the run: {:?}",
        report.reclaimed_coordinates
    );
    assert_eq!(report.kept, 2);
}

/// The other direction, and the one that proves the test above is not passing
/// for an unrelated reason: with the same shape and *no* recorded pull, the old
/// version goes.
#[tokio::test]
async fn the_same_shape_without_a_pull_reclaims() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.keep_if_pulled = Some(days(90));

    let report = e.run(&p).await;
    assert_eq!(report.reclaimed_coordinates, vec!["p@1.0.0"]);
}

/// §4.3's sidecar split, across the round-trip. A `.sha1` is recorded — the
/// audit trail keeps it — as `ViewMetadata`, and the query must not count it.
///
/// The half §4.3 says is "subtle enough to be *broken* by someone widening the
/// sidecar match", asserted where the widening would actually show.
#[tokio::test]
async fn a_checksum_fetch_does_not_reach_the_download_query() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;
    e.record_read("p", "1.0.0", Some("p-1.0.0.jar.sha1"), 1)
        .await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.keep_if_pulled = Some(days(90));

    let report = e.run(&p).await;
    assert_eq!(
        report.reclaimed_coordinates,
        vec!["p@1.0.0"],
        "a checksum fetch is not a download and must not defend a version"
    );

    // …and the fetch was still recorded. The split is about what counts as use,
    // not about dropping events on the floor.
    let recorded: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM access_events \
         WHERE registry = $1 AND package_name = 'p' AND action = 'view_metadata'",
    )
    .bind(&e.registry)
    .fetch_one(&e.pool)
    .await
    .unwrap()
    .get("n");
    assert_eq!(
        recorded, 1,
        "the sidecar fetch is audited, just not counted"
    );
}

/// The other half of the split: a `.pom` is a file a build consumes, so it is a
/// download and it does defend the version. §4.3 asks for both halves because
/// widening `is_verification_sidecar` to "anything that is not the primary
/// artifact" would break exactly this one.
#[tokio::test]
async fn a_pom_fetch_reaches_the_download_query() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;
    e.record_read("p", "1.0.0", Some("p-1.0.0.pom"), 1).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.keep_if_pulled = Some(days(90));

    assert_eq!(
        e.run(&p).await.reclaimed,
        0,
        "a .pom is resolved by a real build and counts as use"
    );
}

/// §13.9: a **denied** download is not use, or a blocked package would defend
/// itself from reclamation by being repeatedly refused. The rule is the
/// `outcome = 'allowed'` predicate, and this is the only test that runs it.
#[tokio::test]
async fn a_denied_download_does_not_reach_the_download_query() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;
    e.record_denied("p", "1.0.0", 1).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.keep_if_pulled = Some(days(90));

    assert_eq!(
        e.run(&p).await.reclaimed_coordinates,
        vec!["p@1.0.0"],
        "being refused is not being used"
    );
}

/// `DISTINCT ON (package_version) … ORDER BY package_version, created_at DESC`
/// must return a version's **newest** pull.
///
/// Reversed, the query returns the oldest, and a version downloaded every day
/// for a year reads as last-pulled-a-year-ago — reclaimed while in active use,
/// which is the failure `keep_if_pulled` exists to prevent. The in-memory double
/// takes a max over a map and structurally cannot have this bug.
#[tokio::test]
async fn the_newest_pull_wins_when_a_version_has_several() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    // Pulled long ago, and again yesterday. Only the second one saves it.
    e.record_read("p", "1.0.0", None, 500).await;
    e.record_read("p", "1.0.0", None, 1).await;
    e.record_read("p", "1.0.0", None, 300).await;

    let mut p = policy();
    p.keep_if_pulled = Some(days(30));

    assert_eq!(
        e.run(&p).await.reclaimed,
        0,
        "the run must see the most recent pull, not an arbitrary one"
    );
}

// ── Ranking, against the ordering the adapter actually returns ────────────────

/// `keep_versions` keeps the newest N, where "newest" is decided by
/// `ORDER BY published_at ASC` in `get_versions` and the `total - 1 - i`
/// arithmetic in the run.
///
/// Each half is guarded on its own already: `local_registry.rs`'s
/// `get_versions_returns_in_published_at_order` pins the adapter's ordering, and
/// the core suite pins the ranking against its own double. What neither says is
/// that *retention depends on the two agreeing* — the link is a comment. This is
/// the test that fails if someone reorders `get_versions` deliberately, updates
/// its own test to match, and does not think about the caller that turns
/// position into "newest". The consequence there is reclaiming the newest N
/// instead of keeping them, in the direction that destroys the only copy.
#[tokio::test]
async fn keep_versions_keeps_the_newest_by_the_adapters_own_ordering() {
    let e = estate!();
    // Published out of version order on purpose: the rank is by date, and a
    // comparison on `version` would pick a different pair (1.0.0 and 2.0.0).
    // The highest stable version is the newest here, so the veto that always
    // keeps it is not what decides this test.
    e.publish("p", "1.0.0", 10).await;
    e.publish("p", "2.0.0", 500).await;
    e.publish("p", "9.0.0", 1).await;
    e.publish("p", "3.0.0", 300).await;

    let mut p = policy();
    p.keep_versions = Some(2);
    p.dry_run = false;

    let report = e.run(&p).await;
    assert_eq!(
        report.reclaimed_coordinates,
        vec!["p@2.0.0", "p@3.0.0"],
        "the two oldest by publish date go, whatever their numbers say"
    );
    let mut live = e.live_versions("p").await;
    live.sort();
    assert_eq!(live, vec!["1.0.0", "9.0.0"]);
}

// ── The version pin, as a column ──────────────────────────────────────────────

/// §13.8 calls the pin "phase 3's most important safety property": the escape an
/// operator has for the one release that matters. It is a column and an
/// `UPDATE`, and nothing else in the suite writes or reads it against Postgres.
#[tokio::test]
async fn a_pin_written_by_the_service_survives_a_live_run() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 800).await;
    e.publish("p", "3.0.0", 700).await;

    let set = e
        .local
        .set_retention_pin(&e.registry, "p", "1.0.0", true, &admin())
        .await
        .unwrap();
    assert!(set, "pinning a live version changes the row");

    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = false;

    let report = e.run(&p).await;
    assert_eq!(
        report.reclaimed_coordinates,
        vec!["p@2.0.0"],
        "the pinned oldest version survives a run that reclaims everything else"
    );

    let mut live = e.live_versions("p").await;
    live.sort();
    assert_eq!(live, vec!["1.0.0", "3.0.0"]);
    assert!(
        e.stored("p", "1.0.0").await,
        "a pinned version keeps its bytes"
    );
}

/// Releasing the pin lets the policy apply again — the read path picking the
/// column back up, not just the write path setting it.
#[tokio::test]
async fn releasing_a_pin_lets_the_policy_reclaim() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;

    e.local
        .set_retention_pin(&e.registry, "p", "1.0.0", true, &admin())
        .await
        .unwrap();
    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = false;
    assert_eq!(e.run(&p).await.reclaimed, 0, "pinned, so nothing goes");

    let released = e
        .local
        .set_retention_pin(&e.registry, "p", "1.0.0", false, &admin())
        .await
        .unwrap();
    assert!(released, "releasing a set pin changes the row");

    assert_eq!(
        e.run(&p).await.reclaimed_coordinates,
        vec!["p@1.0.0"],
        "with the pin gone the policy applies again"
    );
}

// ── dry_run, counted in rows and in objects ───────────────────────────────────

/// §10, in full: "asserted by counting rows *and stored objects* before and
/// after, then running live and comparing against the report".
///
/// The stored-objects half is why this needs a real store. A double's `delete`
/// that returns `Ok(true)` without removing anything passes the in-memory
/// version of this test perfectly.
#[tokio::test]
async fn a_dry_run_writes_nothing_to_the_database_or_the_store() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 800).await;
    e.publish("p", "3.0.0", 1).await;

    let rows_before = e.total_rows().await;
    let live_before = e.live_versions("p").await;

    let mut p = policy();
    p.keep_versions = Some(1);
    let preview = e.run(&p).await;

    assert!(preview.dry_run);
    assert_eq!(preview.reclaimed, 2);
    assert_eq!(e.total_rows().await, rows_before, "no row was written");
    assert_eq!(e.tombstone_count().await, 0, "no tombstone was created");
    assert_eq!(e.live_versions("p").await, live_before);
    for v in ["1.0.0", "2.0.0", "3.0.0"] {
        assert!(e.stored("p", v).await, "{v}: no object was removed");
    }

    // Now live, and the report must have been telling the truth.
    p.dry_run = false;
    let live = e.run(&p).await;
    assert!(!live.dry_run);
    assert_eq!(
        live.reclaimed_coordinates, preview.reclaimed_coordinates,
        "the live run reclaimed exactly what the preview named"
    );

    assert_eq!(
        e.total_rows().await,
        rows_before,
        "reclaiming tombstones rather than deleting: the rows are still there"
    );
    assert_eq!(e.tombstone_count().await, 2);
    assert_eq!(e.live_versions("p").await, vec!["3.0.0"]);
    assert!(!e.stored("p", "1.0.0").await, "the bytes are gone");
    assert!(!e.stored("p", "2.0.0").await, "the bytes are gone");
    assert!(e.stored("p", "3.0.0").await, "the kept one is intact");
}

/// A second run finds nothing, because `get_versions` no longer returns what the
/// first one tombstoned.
///
/// The point is the funnel: `deleted_at IS NULL` is what stops a run from
/// examining its own leftovers, re-reporting them, and attempting a delete on a
/// coordinate that is already spent.
#[tokio::test]
async fn a_second_run_finds_nothing_left_to_reclaim() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = false;

    assert_eq!(e.run(&p).await.reclaimed, 1);

    let second = e.run(&p).await;
    assert_eq!(second.reclaimed, 0, "the tombstone is not a candidate");
    assert_eq!(
        second.examined, 1,
        "and it is not even examined — the run sees one live version"
    );
}

/// A run is scoped to the registry it was asked for, against a store where every
/// other registry's rows are sitting in the same table.
#[tokio::test]
async fn a_run_does_not_reach_another_registrys_rows() {
    let e = estate!();
    let other = estate!();
    e.publish("p", "1.0.0", 900).await;
    other.publish("p", "1.0.0", 900).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = false;
    // Nothing survives `keep_versions = 1` here except the single newest, so a
    // query missing its `registry` predicate would reclaim the neighbour's row.
    e.publish("p", "2.0.0", 1).await;

    assert_eq!(e.run(&p).await.reclaimed_coordinates, vec!["p@1.0.0"]);
    assert_eq!(
        other.live_versions("p").await,
        vec!["1.0.0"],
        "the neighbouring registry is untouched"
    );
    assert!(other.stored("p", "1.0.0").await);
}

// ── The trail the run leaves, read back through the SQL that filters it ───────

/// The claim a double cannot make: `action = ANY($n)` is real SQL over a real
/// column, and the names it matches are what `action_to_str` *writes*. A new
/// action that round-trips in Rust and not through the column would leave the
/// audit filter answering "nothing was deleted" about a registry that was
/// emptied.
#[tokio::test]
async fn a_run_is_readable_back_through_the_action_filter() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = false;
    assert_eq!(e.run(&p).await.reclaimed_coordinates, vec!["p@1.0.0"]);

    let by_action = |action| EventFilter {
        registry: Some(e.registry.clone()),
        actions: vec![action],
        limit: 100,
        ..Default::default()
    };

    let reclaims = e
        .repo
        .list_events(by_action(AccessAction::RetentionReclaim))
        .await
        .unwrap();
    assert_eq!(reclaims.len(), 1, "one reclamation, one row");
    let coord = reclaims[0].package_id.as_ref().unwrap();
    assert_eq!(
        (coord.name.as_str(), coord.version.as_str()),
        ("p", "1.0.0")
    );
    assert_eq!(
        e.repo
            .count_events(by_action(AccessAction::RetentionReclaim))
            .await
            .unwrap(),
        1,
        "the count has to describe the same set the rows do"
    );

    assert!(
        e.repo
            .list_events(by_action(AccessAction::Delete))
            .await
            .unwrap()
            .is_empty(),
        "a policy reclamation must not be filed as a hand deletion"
    );
    assert_eq!(
        e.repo
            .list_events(by_action(AccessAction::RetentionRun))
            .await
            .unwrap()
            .len(),
        1,
        "and the run itself is one registry-scoped row"
    );
}

/// A dry run against a real database writes exactly one row and touches
/// nothing: the preview is on the record, the versions are not.
#[tokio::test]
async fn a_dry_run_records_only_itself() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.publish("p", "2.0.0", 1).await;

    let mut p = policy();
    p.keep_versions = Some(1);
    p.dry_run = true;
    assert_eq!(e.run(&p).await.reclaimed_coordinates, vec!["p@1.0.0"]);

    async fn count_of(e: &TestEstate, action: AccessAction) -> usize {
        e.repo
            .list_events(EventFilter {
                registry: Some(e.registry.clone()),
                actions: vec![action],
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap()
            .len()
    }
    assert_eq!(count_of(&e, AccessAction::RetentionDryRun).await, 1);
    assert_eq!(count_of(&e, AccessAction::RetentionRun).await, 0);
    assert_eq!(count_of(&e, AccessAction::RetentionReclaim).await, 0);
    assert_eq!(e.live_versions("p").await, vec!["1.0.0", "2.0.0"]);
}

/// An empty action set means *every* action, not none.
///
/// `action = ANY('{}')` is false for every row, so the obvious binding turns a
/// filter nobody asked for into an audit log that reads as empty. This is the
/// test for the `NULL` that avoids it.
#[tokio::test]
async fn an_empty_action_filter_returns_every_action() {
    let e = estate!();
    e.publish("p", "1.0.0", 900).await;
    e.record_read("p", "1.0.0", None, 1).await;
    e.record_read("p", "1.0.0", Some("p-1.0.0.jar.sha1"), 1)
        .await;

    let all = e
        .repo
        .list_events(EventFilter {
            registry: Some(e.registry.clone()),
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 2, "a download and a metadata view");
    assert!(all.iter().any(|ev| ev.action == AccessAction::Download));
    assert!(all.iter().any(|ev| ev.action == AccessAction::ViewMetadata));
}
