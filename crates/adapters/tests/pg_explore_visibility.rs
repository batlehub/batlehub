//! Integration tests for per-package visibility filtering in the explore
//! queries (`PgPackageRepository::explore_packages` / `count_explore_packages`).
//!
//! **These cannot be written against the in-memory harness.**
//! `PackageRepository::explore_packages` has a default trait impl returning
//! `Ok(vec![])`, so `InMemoryPackageRepository` — the backend behind
//! `crates/web/tests/explore.rs` — returns an empty list for every query and
//! would pass whether or not the filter exists. The leak this guards against is
//! only observable against real SQL.
//!
//! Requires a running PostgreSQL instance. Set `DATABASE_URL` to opt in:
//!
//!   task test:pg-explore                             # starts Postgres via Podman automatically
//!   DATABASE_URL=postgresql://batlehub:changeme@localhost/batlehub \
//!     cargo test -p batlehub-adapters --test pg_explore_visibility

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use sqlx::PgPool;

use batlehub_adapters::db::packages::PoolOptions;
use batlehub_adapters::db::{PgPackageRepository, PgTeamNamespaceStore};
use batlehub_adapters::local_registry::PostgresLocalRegistry;
use batlehub_core::{
    entities::{
        AccessEvent, ExploreFilter, ExploreSortBy, ExploreViewer, PackageId, PublishedPackage,
        Role, TeamNamespace, Visibility,
    },
    ports::{LocalRegistryBackend, PackageRepository, TeamNamespacePort},
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    repo: PgPackageRepository,
    local: PostgresLocalRegistry,
    namespaces: PgTeamNamespaceStore,
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
        namespaces: PgTeamNamespaceStore::new(pool),
        repo,
        registry: format!("vis-{pid}-{id}"),
    }
}

impl Fixture {
    async fn publish(&self, name: &str, visibility: Visibility) {
        let pkg = PublishedPackage {
            registry: self.registry.clone(),
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            checksum: format!("{:064x}", 0u64),
            yanked: false,
            deprecated: false,
            deprecation_message: None,
            unlisted: false,
            index_metadata: serde_json::json!({ "name": name, "version": "1.0.0" }),
            published_at: Utc::now(),
            published_by: Some("test-user".to_owned()),
            signature_bytes: None,
            signature_type: None,
            visibility,
            retention_keep: false,
        };
        // `publish` inserts in the *pending* state, which the explore queries
        // filter out via `status = 'published'`; `commit_publish` is what makes
        // the row visible. Skipping it would make every one of these tests pass
        // vacuously against an empty result set.
        self.local.publish(pkg).await.expect("publish");
        self.local
            .commit_publish(&self.registry, name, "1.0.0")
            .await
            .expect("commit publish");
    }

    /// Write the `package_statuses` row that `record_access` writes on every
    /// allowed read — including a *local* one.
    ///
    /// This is the mechanism behind survey finding 12: the row lands in a table
    /// with no visibility column, and the catalogue's `proxied` CTE used to read
    /// that table with no gate. So a private package became listable the first
    /// time somebody entitled to it pulled it.
    async fn record_download(&self, name: &str) {
        self.repo
            .record_access(AccessEvent::allowed_download(
                PackageId::new(&self.registry, name, "1.0.0"),
                Some("test-user".to_owned()),
                Role::User,
            ))
            .await
            .expect("record_access");
    }

    async fn claim(&self, prefix: &str, group_id: &str) {
        self.namespaces
            .claim_namespace(TeamNamespace {
                registry: self.registry.clone(),
                prefix: prefix.to_owned(),
                group_id: group_id.to_owned(),
                claimed_by: Some("test-admin".to_owned()),
                separator: '/',
            })
            .await
            .expect("claim namespace");
    }

    fn filter(&self, viewer: ExploreViewer) -> ExploreFilter {
        ExploreFilter {
            registry: Some(self.registry.clone()),
            registries: vec![],
            name_contains: None,
            name_in: vec![],
            sort_by: ExploreSortBy::Name,
            limit: 100,
            offset: 0,
            viewer,
        }
    }

    /// Package names this viewer can see, sorted. Also asserts the paired count
    /// query agrees — a total that disagrees with the page is its own bug.
    async fn visible_to(&self, viewer: ExploreViewer) -> Vec<String> {
        let entries = self
            .repo
            .explore_packages(self.filter(viewer.clone()))
            .await
            .expect("explore_packages");
        let count = self
            .repo
            .count_explore_packages(self.filter(viewer))
            .await
            .expect("count_explore_packages");

        let mut names: Vec<String> = entries.into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(
            count as usize,
            names.len(),
            "count query must apply the same visibility predicate as the list query"
        );
        names
    }
}

fn anonymous() -> ExploreViewer {
    ExploreViewer::default()
}

fn user(groups: &[&str]) -> ExploreViewer {
    ExploreViewer {
        is_admin: false,
        is_authenticated: true,
        groups: groups.iter().map(|g| (*g).to_owned()).collect(),
    }
}

fn admin() -> ExploreViewer {
    ExploreViewer {
        is_admin: true,
        is_authenticated: true,
        groups: vec![],
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

#[tokio::test]
async fn public_is_visible_to_everyone_including_anonymous() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("open-pkg", Visibility::Public).await;

    assert_eq!(f.visible_to(anonymous()).await, vec!["open-pkg"]);
    assert_eq!(f.visible_to(user(&[])).await, vec!["open-pkg"]);
    assert_eq!(f.visible_to(admin()).await, vec!["open-pkg"]);
}

#[tokio::test]
async fn internal_is_hidden_from_anonymous_but_visible_once_authenticated() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("internal-pkg", Visibility::Internal).await;

    assert!(
        f.visible_to(anonymous()).await.is_empty(),
        "an internal package must not appear in an anonymous listing"
    );
    assert_eq!(f.visible_to(user(&[])).await, vec!["internal-pkg"]);
    assert_eq!(f.visible_to(admin()).await, vec!["internal-pkg"]);
}

#[tokio::test]
async fn team_is_visible_only_to_members_of_the_owning_group() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("team-a/secret", Visibility::Team).await;
    f.claim("team-a", "group-a").await;

    assert!(f.visible_to(anonymous()).await.is_empty());
    assert!(
        f.visible_to(user(&[])).await.is_empty(),
        "merely being authenticated must not reveal a team package"
    );
    assert!(
        f.visible_to(user(&["group-b"])).await.is_empty(),
        "membership of an unrelated group must not reveal a team package"
    );
    assert_eq!(
        f.visible_to(user(&["group-a"])).await,
        vec!["team-a/secret"]
    );
    assert_eq!(f.visible_to(admin()).await, vec!["team-a/secret"]);
}

/// Mirrors `check_team_visibility`'s `None` arm, which denies rather than
/// falling back to `Internal`. A deleted or never-created claim must not quietly
/// widen a team package to every authenticated user.
#[tokio::test]
async fn team_with_no_namespace_claim_is_visible_to_nobody_but_admins() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("unclaimed/pkg", Visibility::Team).await;
    // deliberately no claim_namespace call

    assert!(f.visible_to(anonymous()).await.is_empty());
    assert!(f.visible_to(user(&[])).await.is_empty());
    assert!(f.visible_to(user(&["group-a"])).await.is_empty());
    assert_eq!(f.visible_to(admin()).await, vec!["unclaimed/pkg"]);
}

/// The longest matching prefix wins **outright**: if the most specific claim
/// belongs to a group the viewer is not in, access is denied even though a
/// shorter claim they *are* in also matches. An `EXISTS` over all matching
/// claims would wrongly allow this.
#[tokio::test]
async fn longest_prefix_claim_wins_even_when_a_shorter_one_would_match() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("org/team-a/pkg", Visibility::Team).await;
    f.claim("org", "group-outer").await;
    f.claim("org/team-a", "group-inner").await;

    assert!(
        f.visible_to(user(&["group-outer"])).await.is_empty(),
        "the shorter 'org' claim must not grant access when 'org/team-a' is more specific"
    );
    assert_eq!(
        f.visible_to(user(&["group-inner"])).await,
        vec!["org/team-a/pkg"]
    );
}

/// `check_team_visibility` compares group ids with spaces stripped from both
/// sides; the SQL predicate has to do the same or a group named "Team A" would
/// match in one place and not the other.
#[tokio::test]
async fn group_comparison_ignores_spaces_on_both_sides() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("spaced/pkg", Visibility::Team).await;
    f.claim("spaced", "Team A").await;

    assert_eq!(f.visible_to(user(&["TeamA"])).await, vec!["spaced/pkg"]);
    assert_eq!(f.visible_to(user(&["Team A"])).await, vec!["spaced/pkg"]);
}

/// A namespace prefix containing a SQL `LIKE` metacharacter must be matched
/// literally. `LIKE prefix || '/%'` would let `a_c` claim `abc/...`, making the
/// listing more permissive than the download path.
#[tokio::test]
async fn like_metacharacters_in_a_prefix_are_matched_literally() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("abc/pkg", Visibility::Team).await;
    f.claim("a_c", "group-wildcard").await;

    assert!(
        f.visible_to(user(&["group-wildcard"])).await.is_empty(),
        "'a_c' must not match 'abc' — the underscore is a literal, not a wildcard"
    );
}

#[tokio::test]
async fn mixed_visibilities_are_filtered_independently() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("pub-pkg", Visibility::Public).await;
    f.publish("int-pkg", Visibility::Internal).await;
    f.publish("team-a/secret", Visibility::Team).await;
    f.claim("team-a", "group-a").await;

    assert_eq!(f.visible_to(anonymous()).await, vec!["pub-pkg"]);
    assert_eq!(f.visible_to(user(&[])).await, vec!["int-pkg", "pub-pkg"]);
    assert_eq!(
        f.visible_to(user(&["group-a"])).await,
        vec!["int-pkg", "pub-pkg", "team-a/secret"]
    );
    assert_eq!(
        f.visible_to(admin()).await,
        vec!["int-pkg", "pub-pkg", "team-a/secret"]
    );
}

// ── The access log as a second door (survey finding 12) ──────────────────────

/// The finding itself: a `team`-visibility package that its own owner has
/// downloaded once must not thereby become listable to everyone.
///
/// The download is what makes this test different from the ones above — without
/// it the package has no `package_statuses` row, the `proxied` CTE never sees
/// it, and the assertion passes against the bug.
#[tokio::test]
async fn a_downloaded_team_package_does_not_leak_through_the_access_log() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.claim("secret-pkg", "team-a").await;
    f.publish("secret-pkg", Visibility::Team).await;

    // Control: hidden before anything is recorded, visible to a member.
    assert!(f.visible_to(anonymous()).await.is_empty());
    assert_eq!(f.visible_to(user(&["team-a"])).await, vec!["secret-pkg"]);

    f.record_download("secret-pkg").await;

    assert!(
        f.visible_to(anonymous()).await.is_empty(),
        "a team package became listable to an anonymous caller because a member downloaded it"
    );
    assert!(
        f.visible_to(user(&["other-team"])).await.is_empty(),
        "…and to a member of some other team"
    );
    assert_eq!(
        f.visible_to(user(&["team-a"])).await,
        vec!["secret-pkg"],
        "the members it belongs to must still see it"
    );
    assert_eq!(f.visible_to(admin()).await, vec!["secret-pkg"]);
}

/// The same gate must not over-block. A package known only through the access
/// log — proxied from upstream, never published here — has no `local_packages`
/// row and no visibility to check: its name came from a public registry and was
/// never a secret.
#[tokio::test]
async fn a_proxied_only_package_stays_listable() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.record_download("upstream-pkg").await;

    assert_eq!(f.visible_to(anonymous()).await, vec!["upstream-pkg"]);
    assert_eq!(f.visible_to(user(&[])).await, vec!["upstream-pkg"]);
}

/// A hybrid package — published here *and* proxied — keeps both halves of its
/// row for a viewer who may see it. The gate excludes the proxied contribution
/// only when the local package is one this viewer may not see.
#[tokio::test]
async fn a_public_local_package_keeps_its_proxied_row() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("both-pkg", Visibility::Public).await;
    f.record_download("both-pkg").await;

    assert_eq!(f.visible_to(anonymous()).await, vec!["both-pkg"]);
}

// ── RFC 0015 §4.4 — an aggregate is a listing that has been counted ──────────
//
// > Every one of those tiles is a query over packages, and it is filtered by the
// > caller's grants exactly like a version index, for the reason §4.4 rule 1
// > already gives: a number computed over rows the caller may not see is a
// > disclosure whether or not the rows themselves are returned.
//
// `registry_explore_stats` had **no** visibility predicate, so `package_count`
// and `total_downloads` were computed over `internal`, `team` and `private`
// packages alike — survey finding 12 one abstraction level up, on the surface
// §4.4 says it "will arrive a fourth time". Asserted against real Postgres for
// the reason §11.4 gives about the pagination tests: an in-memory repository
// agrees with an incorrect query, and here it agrees by returning nothing at
// all.

impl Fixture {
    /// The one registry's stats row for this viewer, or `None` when the
    /// aggregate reports nothing for it.
    async fn stats_for(
        &self,
        viewer: ExploreViewer,
    ) -> Option<batlehub_core::entities::RegistryStat> {
        self.repo
            .registry_explore_stats(std::slice::from_ref(&self.registry), &viewer)
            .await
            .expect("registry_explore_stats")
            .into_iter()
            .find(|s| s.registry == self.registry)
    }

    async fn package_count(&self, viewer: ExploreViewer) -> u64 {
        self.stats_for(viewer).await.map_or(0, |s| s.package_count)
    }
}

/// §11.4: *"a caller who can see 3 of 50 packages gets counts, sums and top-N
/// lists over those 3"*.
///
/// The count and the listing agree by construction, and the test asserts both so
/// a future change that fixes one and not the other is a failure rather than a
/// quiet disagreement between a page and the tile above it.
#[tokio::test]
async fn the_package_count_is_computed_over_what_the_viewer_may_see() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("open", Visibility::Public).await;
    f.publish("staff", Visibility::Internal).await;
    f.publish("secret", Visibility::Team).await;

    // The control: an admin sees all three, so a smaller number below is the
    // predicate and not an empty fixture.
    assert_eq!(f.package_count(admin()).await, 3);
    assert_eq!(f.visible_to(admin()).await.len(), 3);

    // An anonymous caller sees one, and the tile says one.
    assert_eq!(
        f.package_count(anonymous()).await,
        1,
        "a count over packages this caller cannot see discloses that they exist"
    );
    assert_eq!(f.visible_to(anonymous()).await, vec!["open"]);

    // An authenticated non-member sees the internal one too, and no more.
    assert_eq!(f.package_count(user(&[])).await, 2);
    assert_eq!(f.visible_to(user(&[])).await.len(), 2);
}

/// §11.4 asks for this *"on a `SUM` as well as a `count(*)`, because a sum is the
/// one that cannot be trimmed after the fact and so fails silently"*.
///
/// `total_downloads` is that sum. A download recorded against a package the
/// viewer may not see must not reach their number — and unlike a row set, there
/// is nothing about the total that says which events went into it.
#[tokio::test]
async fn the_download_sum_is_computed_over_what_the_viewer_may_see() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("open", Visibility::Public).await;
    f.publish("secret", Visibility::Team).await;

    f.record_download("open").await;
    for _ in 0..4 {
        f.record_download("secret").await;
    }

    let admin_total = f
        .stats_for(admin())
        .await
        .expect("the registry has rows")
        .total_downloads;
    assert_eq!(admin_total, 5, "the control: every event is recorded");

    let anon_total = f
        .stats_for(anonymous())
        .await
        .expect("the public package keeps the registry in the result")
        .total_downloads;
    assert_eq!(
        anon_total, 1,
        "a sum over a package this caller cannot see is the disclosure §4.4 calls \
         the one that fails silently; got {anon_total}"
    );
}

/// A team member's numbers include their own team's package and nobody else's.
///
/// The `team` arm is the one that needs a *group* rather than a role, and it is
/// the arm an `internal`-only test waves through.
#[tokio::test]
async fn a_team_members_aggregate_includes_their_own_namespace() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("acme/lib", Visibility::Team).await;
    f.publish("other/lib", Visibility::Team).await;
    f.claim("acme", "team-acme").await;
    f.claim("other", "team-other").await;
    f.record_download("acme/lib").await;
    f.record_download("other/lib").await;

    let member = f
        .stats_for(user(&["team-acme"]))
        .await
        .expect("their own package keeps the registry in the result");
    assert_eq!(member.package_count, 1);
    assert_eq!(member.total_downloads, 1);

    // A user in neither team sees nothing here, and the registry drops out of
    // the aggregate entirely rather than appearing with a wrong number.
    assert_eq!(f.package_count(user(&["team-nobody"])).await, 0);
}

/// **Survey finding 2, in the aggregate.** An empty scope is nothing, not
/// everything.
///
/// The query used to bind `NULL` for an empty `accessible_registries` and read
/// it as `$1 IS NULL OR …` — so a caller with no browsable registry at all was
/// handed every registry's counts by the one query whose job is to scope them.
/// *"A predicate that is vacuous rather than absent"* is the shape that finding
/// came in on, and the fix is to make the vacuous reading unrepresentable.
#[tokio::test]
async fn an_empty_registry_scope_aggregates_nothing() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.publish("open", Visibility::Public).await;

    // The control: named, the registry is there.
    assert_eq!(f.package_count(admin()).await, 1);

    let none = f
        .repo
        .registry_explore_stats(&[], &admin())
        .await
        .expect("registry_explore_stats");
    assert!(
        none.is_empty(),
        "an empty scope must aggregate nothing, not every registry; got {} rows",
        none.len()
    );
}

/// A package that only ever came from upstream stays counted.
///
/// The other direction of the same rule, and the reason the predicate is written
/// as *"no local row → visible"* rather than as a join: a proxied name came from
/// upstream and was never a secret, so hiding it would be a narrowing nobody
/// asked for — and would make an admin's own storage total wrong.
#[tokio::test]
async fn a_proxied_only_package_stays_in_the_aggregate() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.record_download("upstream-pkg").await;

    assert_eq!(f.package_count(anonymous()).await, 1);
    assert_eq!(
        f.stats_for(anonymous()).await.unwrap().total_downloads,
        1,
        "an upstream package's downloads belong in everyone's total"
    );
}

// ── `GET /api/v1/packages` — the same door through `list_packages` ───────────

impl Fixture {
    async fn listed_to(&self, viewer: Option<ExploreViewer>) -> (Vec<String>, u64) {
        let filter = batlehub_core::entities::PackageFilter {
            registry: Some(self.registry.clone()),
            limit: 50,
            viewer,
            ..Default::default()
        };
        let names = self
            .repo
            .list_packages(filter.clone())
            .await
            .expect("list_packages")
            .into_iter()
            .map(|p| p.package_id.name)
            .collect();
        let total = self.repo.count_packages(filter).await.expect("count");
        (names, total)
    }
}

/// Survey finding 12 again, on the front-office listing: a downloaded `team`
/// package has a `package_statuses` row, and `list_packages` read that table
/// with no visibility gate. `None` is the back office, which stays unfiltered.
#[tokio::test]
async fn the_package_listing_hides_a_downloaded_team_package_from_non_members() {
    let url = require_db!();
    let f = fixture(&url).await;
    f.claim("secret-pkg", "team-a").await;
    f.publish("secret-pkg", Visibility::Team).await;
    f.record_download("secret-pkg").await;
    f.record_download("upstream-pkg").await;

    let upstream_only = (vec!["upstream-pkg".to_owned()], 1);
    assert_eq!(f.listed_to(Some(anonymous())).await, upstream_only);
    assert_eq!(
        f.listed_to(Some(user(&["other-team"]))).await,
        upstream_only
    );

    let both = (vec!["secret-pkg".to_owned(), "upstream-pkg".to_owned()], 2);
    assert_eq!(f.listed_to(Some(user(&["team-a"]))).await, both);
    assert_eq!(f.listed_to(None).await, both, "the unfiltered back office");
}
