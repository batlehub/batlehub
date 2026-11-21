//! Integration tests for the facts the catalog grades a package's *resolution
//! state* from (`PgPackageRepository::explore_packages`).
//!
//! **These cannot be written against the in-memory harness**, for the same
//! reason `pg_explore_visibility.rs` cannot: `PackageRepository::explore_packages`
//! has a default trait impl returning `Ok(vec![])`, so the in-memory backend
//! behind `crates/web/tests/explore.rs` returns nothing for every query and
//! would pass whether or not these columns are joined at all.
//!
//! The grading itself — which of the six states a set of facts means — is pure
//! and lives in `batlehub_core::entities::explore::resolve_state`, with unit
//! tests beside it. What is only observable against real SQL is whether the
//! facts arrive: that `artifact_cache_meta` is joined per package, that a yank
//! and a block travel in separate columns, and that "newest" means the version
//! most recently obtained rather than the greatest string.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   task test:pg-explore                             # starts Postgres via Podman automatically
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test pg_explore_resolution

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use sqlx::PgPool;

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::{PgArtifactMetaRepository, PgPackageRepository};
use batlehub_adapters::local_registry::PostgresLocalRegistry;
use batlehub_core::{
    entities::{
        ExploreEntry, ExploreFilter, ExploreSortBy, ExploreViewer, PublishedPackage, Visibility,
    },
    ports::{ArtifactCacheMeta, ArtifactMetaRecord, LocalRegistryBackend, PackageRepository},
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    repo: PgPackageRepository,
    local: PostgresLocalRegistry,
    meta: PgArtifactMetaRepository,
    pool: PgPool,
    /// Unique per test so parallel runs cannot see each other's rows.
    registry: String,
}

async fn fixture(url: &str) -> Fixture {
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
    Fixture {
        local: PostgresLocalRegistry::new(pool.clone()),
        meta: PgArtifactMetaRepository::new(pool.clone()),
        pool,
        repo,
        registry: format!("res-{pid}-{id}"),
    }
}

impl Fixture {
    /// A proxied version: known to exist, but holding no bytes until
    /// [`Self::cache`] is called for it. The difference between those two is
    /// exactly what separates Pending from Cached.
    async fn proxied(&self, name: &str, version: &str) {
        sqlx::query(
            "INSERT INTO package_statuses (id, registry, package_name, package_version, status) \
             VALUES (gen_random_uuid(), $1, $2, $3, 'available')",
        )
        .bind(&self.registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .expect("insert package status");
    }

    async fn block(&self, name: &str, version: &str) {
        sqlx::query(
            "UPDATE package_statuses SET status = 'blocked', block_reason = 'test', \
             blocked_by = 'admin', blocked_at = NOW() \
             WHERE registry = $1 AND package_name = $2 AND package_version = $3",
        )
        .bind(&self.registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .expect("block version");
    }

    /// Record that we hold bytes for a version, which is what
    /// `artifact_cache_meta` means and what `package_statuses` does not.
    async fn cache(&self, name: &str, version: &str, size: Option<u64>) {
        self.meta
            .record_artifact(ArtifactMetaRecord {
                key: &format!("{}/{name}/{version}", self.registry),
                registry: &self.registry,
                package_name: name,
                version,
                size,
                checksum: None,
            })
            .await
            .expect("record artifact");
    }

    async fn publish(&self, name: &str, version: &str, yanked: bool) {
        let pkg = PublishedPackage {
            registry: self.registry.clone(),
            name: name.to_owned(),
            version: version.to_owned(),
            checksum: format!("{:064x}", 0u64),
            yanked: false,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata: serde_json::json!({ "name": name, "version": version }),
            published_at: Utc::now(),
            published_by: Some("test-user".to_owned()),
            signature_bytes: None,
            signature_type: None,
            visibility: Visibility::Public,
            retention_keep: false,
        };
        // `publish` inserts in the *pending* state, which the explore queries
        // filter out via `status = 'published'`; `commit_publish` is what makes
        // the row visible. Skipping it would make these tests pass vacuously.
        self.local.publish(pkg).await.expect("publish");
        self.local
            .commit_publish(&self.registry, name, version)
            .await
            .expect("commit publish");
        if yanked {
            self.local
                .yank(&self.registry, name, version)
                .await
                .expect("yank");
        }
    }

    async fn entry(&self, name: &str) -> ExploreEntry {
        let entries = self
            .repo
            .explore_packages(ExploreFilter {
                registry: Some(self.registry.clone()),
                registries: vec![],
                name_contains: None,
                name_in: vec![],
                sort_by: ExploreSortBy::Name,
                limit: 100,
                offset: 0,
                viewer: ExploreViewer {
                    is_admin: true,
                    is_authenticated: true,
                    groups: vec![],
                },
            })
            .await
            .expect("explore_packages");
        entries
            .into_iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("no explore entry for '{name}'"))
    }
}

macro_rules! require_db {
    () => {
        match db_url() {
            Some(url) => url,
            None => {
                eprintln!("skipping: DATABASE_URL not set");
                return;
            }
        }
    };
}

/// A package `package_statuses` knows about but no artifact has been stored
/// for. This is the Pending case, and it is the one that a `version_count`-based
/// inference would get wrong: the version count is 1, and we hold nothing.
#[tokio::test]
async fn a_known_version_with_no_stored_bytes_reports_nothing_held() {
    let url = require_db!();
    let fx = fixture(&url).await;
    fx.proxied("lonely", "1.0.0").await;

    let entry = fx.entry("lonely").await;
    assert_eq!(entry.version_count, 1, "the version is known");
    assert_eq!(entry.cached_versions, 0, "but no bytes are held");
    assert_eq!(entry.cached_bytes, None);
    assert_eq!(entry.last_fetched_at, None);
    assert_eq!(entry.newest_version, None);
}

#[tokio::test]
async fn stored_bytes_are_summed_across_versions_and_dated_by_the_newest() {
    let url = require_db!();
    let fx = fixture(&url).await;
    fx.proxied("summed", "1.0.0").await;
    fx.proxied("summed", "1.1.0").await;
    fx.cache("summed", "1.0.0", Some(1_000)).await;
    fx.cache("summed", "1.1.0", Some(2_500)).await;

    let entry = fx.entry("summed").await;
    assert_eq!(entry.cached_versions, 2);
    assert_eq!(entry.cached_bytes, Some(3_500));
    assert!(
        entry.last_fetched_at.is_some(),
        "holding bytes means we fetched them at some point"
    );
}

/// `SUM` over an all-NULL column is NULL, and that has to survive to the entry:
/// an artifact cached before migration 004 recorded no size, and reporting 0
/// would claim we hold an empty file.
#[tokio::test]
async fn an_unrecorded_size_stays_unknown_rather_than_becoming_zero() {
    let url = require_db!();
    let fx = fixture(&url).await;
    fx.proxied("sizeless", "1.0.0").await;
    fx.cache("sizeless", "1.0.0", None).await;

    let entry = fx.entry("sizeless").await;
    assert_eq!(entry.cached_versions, 1, "we do hold it");
    assert_eq!(
        entry.cached_bytes, None,
        "we just do not know how big it is"
    );
}

/// The ordering that a `MAX(version)` would get backwards: as text, `1.10.0`
/// sorts *before* `1.9.0`. "Newest" here means the one most recently obtained,
/// which is a timestamp comparison and always well-defined.
#[tokio::test]
async fn newest_version_is_the_one_most_recently_obtained_not_the_greatest_string() {
    let url = require_db!();
    let fx = fixture(&url).await;
    fx.proxied("ordering", "1.9.0").await;
    fx.proxied("ordering", "1.10.0").await;
    // Cached in this order, so 1.9.0 is the most recently obtained even though
    // 1.10.0 is the greater version and the later string.
    fx.cache("ordering", "1.10.0", Some(10)).await;
    fx.cache("ordering", "1.9.0", Some(10)).await;

    let entry = fx.entry("ordering").await;
    assert_eq!(entry.newest_version.as_deref(), Some("1.9.0"));
}

/// A block and a yank used to arrive in the same column, which made an
/// operator's refusal indistinguishable from an owner's withdrawal. They are
/// different facts with different remedies, and the catalog grades them as
/// different states.
#[tokio::test]
async fn a_block_and_a_yank_travel_in_separate_columns() {
    let url = require_db!();
    let fx = fixture(&url).await;

    fx.proxied("blocked-pkg", "1.0.0").await;
    fx.block("blocked-pkg", "1.0.0").await;
    fx.publish("yanked-pkg", "1.0.0", true).await;
    fx.publish("clean-pkg", "1.0.0", false).await;

    let blocked = fx.entry("blocked-pkg").await;
    assert!(blocked.has_blocked, "an admin blocked it");
    assert!(!blocked.has_yanked, "nobody yanked it");

    let yanked = fx.entry("yanked-pkg").await;
    assert!(yanked.has_yanked, "its owner withdrew it");
    assert!(
        !yanked.has_blocked,
        "a yank is not a block — this is the conflation the split removed"
    );

    let clean = fx.entry("clean-pkg").await;
    assert!(!clean.has_blocked);
    assert!(!clean.has_yanked);
}

/// A locally published version carries a publish date, which is the input the
/// release-age gate needs; a proxied one does not, and the gate has defined
/// behaviour for that. The catalog must not invent one.
#[tokio::test]
async fn a_local_publish_carries_a_publish_date_and_a_proxied_fetch_does_not() {
    let url = require_db!();
    let fx = fixture(&url).await;

    fx.publish("local-pkg", "1.0.0", false).await;
    fx.proxied("proxied-pkg", "1.0.0").await;
    fx.cache("proxied-pkg", "1.0.0", Some(10)).await;

    let local = fx.entry("local-pkg").await;
    assert_eq!(local.newest_version.as_deref(), Some("1.0.0"));
    assert!(
        local.newest_published_at.is_some(),
        "a local publish knows when it was published"
    );

    let proxied = fx.entry("proxied-pkg").await;
    assert_eq!(proxied.newest_version.as_deref(), Some("1.0.0"));
    assert_eq!(
        proxied.newest_published_at, None,
        "the upstream publish date is not persisted for proxied artifacts"
    );
}

/// The row answers for the newest version: one blocked old release of a local
/// package does not make it blocked, and blocking a version published here does
/// not count it twice or make the package "both".
#[tokio::test]
async fn an_older_blocked_local_version_leaves_the_newest_in_charge() {
    let url = require_db!();
    let fx = fixture(&url).await;

    fx.publish("two-local", "0.1.0", false).await;
    fx.publish("two-local", "0.2.0", false).await;
    // An admin block of a local version writes its `package_statuses` row;
    // the fixture's `block` updates one, so the row is made first.
    fx.proxied("two-local", "0.1.0").await;
    fx.block("two-local", "0.1.0").await;

    let e = fx.entry("two-local").await;
    assert_eq!(e.cached_versions, 2, "a version published here is held");
    assert!(e.has_blocked, "a version is blocked: the secondary fact");
    assert!(!e.newest_blocked, "0.2.0, the newest, is not");
    assert_eq!(
        e.version_count, 2,
        "a local version blocked is still one version"
    );
    assert!(
        matches!(e.source, batlehub_core::entities::PackageSource::Local),
        "only published here: not both, {:?}",
        e.source
    );

    fx.proxied("two-local", "0.2.0").await;
    fx.block("two-local", "0.2.0").await;
    assert!(
        fx.entry("two-local").await.newest_blocked,
        "now the newest is"
    );
}

/// A proxied version blocked before anything was fetched has no newest version
/// to grade: the package is blocked when every version it is known by is.
#[tokio::test]
async fn a_blocked_version_never_fetched_still_blocks_the_package() {
    let url = require_db!();
    let fx = fixture(&url).await;

    fx.proxied("never-fetched", "1.0.0").await;
    fx.block("never-fetched", "1.0.0").await;
    assert!(fx.entry("never-fetched").await.newest_blocked);

    fx.proxied("never-fetched", "1.1.0").await;
    assert!(
        !fx.entry("never-fetched").await.newest_blocked,
        "1.1.0 is not blocked"
    );
}
