use std::collections::HashMap;

use crate::db::DbResultExt;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

use crate::migrations::embedded_migrator;
use uuid::Uuid;

use batlehub_core::{
    entities::{
        AccessAction, AccessEvent, AccessResult, EventFilter, ExploreEntry, ExploreFilter,
        ExploreSortBy, ExploreViewer, PackageFilter, PackageId, PackageSource, PackageStatus,
        PackageSummary, RegistryStat, Role,
    },
    error::CoreError,
    ports::{PackageRepository, RecentErrorRecord},
};

pub mod crud;
pub mod explore;
pub mod health;

pub(super) fn prepare_registries_param(registries: &[String]) -> Option<Vec<String>> {
    if registries.is_empty() {
        None
    } else {
        Some(registries.to_vec())
    }
}

pub(super) fn map_package_status(r: &PgRow) -> PackageStatus {
    let status: String = r.get("status");
    if status == "blocked" {
        PackageStatus::Blocked {
            reason: r
                .get::<Option<String>, _>("block_reason")
                .unwrap_or_default(),
            blocked_by: r.get::<Option<String>, _>("blocked_by").unwrap_or_default(),
            blocked_at: r
                .get::<Option<DateTime<Utc>>, _>("blocked_at")
                .unwrap_or_else(Utc::now),
        }
    } else {
        PackageStatus::Available
    }
}

pub(super) fn map_package_summary(r: PgRow) -> PackageSummary {
    PackageSummary {
        id: r.get("id"),
        package_id: PackageId {
            registry: r.get("registry"),
            name: r.get("package_name"),
            version: r.get("package_version"),
            artifact: r.get("package_artifact"),
        },
        status: map_package_status(&r),
        last_accessed: r.get("last_accessed"),
        last_accessed_by: r.get("last_accessed_by"),
        access_count: r.get::<i64, _>("access_count") as u64,
    }
}

/// SQL predicate restricting `local_packages lp` to rows the viewer may see.
///
/// This is the listing-side counterpart of
/// `LocalRegistryService::check_visibility`, and it has to agree with it exactly
/// — a listing that is *more* permissive leaks the names and version counts of
/// packages the same caller would get a `403` for on download, which is the whole
/// bug this exists to fix.
///
/// The three rules, mirrored one for one:
///
/// | `visibility` | Rust (`check_visibility`)          | here                       |
/// |--------------|------------------------------------|----------------------------|
/// | `public`     | always allowed                     | `true`                     |
/// | `internal`   | `has_role_at_least(User)`          | viewer is authenticated    |
/// | `team`       | `check_team_visibility`            | longest-prefix claim match |
///
/// Admins bypass everything, as they do in `check_visibility`.
///
/// The `team` branch reproduces `find_namespace`'s matcher **character for
/// character** — `N = prefix` or `SUBSTRING(N, 1, LENGTH(prefix)+1) = prefix ||
/// '/'`, ordered by `LENGTH(prefix) DESC LIMIT 1`. Two details matter:
///
/// - `SUBSTRING(...)` rather than `LIKE prefix || '/%'`: a prefix containing `%`
///   or `_` would otherwise act as a wildcard here while staying literal in
///   Rust, making the listing more permissive than the download path.
/// - Longest prefix **wins outright**. If the most specific claim belongs to a
///   group the viewer is not in, access is denied even when a shorter claim would
///   have matched. An `EXISTS` over *all* matching claims would quietly widen it.
///
/// A `team` package with no claim at all is denied, matching
/// `check_team_visibility`'s `None` arm (which denies rather than falling back).
/// That follows here for free: the subquery yields no row.
///
/// # `private` is excluded, and the asymmetry with `check_visibility` is
/// deliberate
///
/// RFC 0015 §4.5's fourth value matches none of the arms below, so a `private`
/// row is invisible to every non-admin here. `check_visibility` is *wider*: it
/// admits a caller holding a read grant written on the package itself, which is
/// what `private` means — inherited grants do not apply, grants on the node do.
///
/// So this predicate refuses one caller the download path would serve. That is
/// the safe direction and the doc comment above says why the other direction is
/// not: a listing more permissive than the check discloses names the download
/// path would refuse. A listing *less* permissive hides a package from someone
/// entitled to it, which is a worse experience and not a disclosure.
///
/// Closing it is §6.3's "the SQL visibility predicate becomes a grant
/// predicate", which is a hierarchical join rather than a column comparison and
/// is the part of this design §11.7 measured separately before allowing it. It
/// is not attempted inline here: the arm is written out explicitly so the
/// exclusion is a decision in the SQL rather than a row that happened to match
/// nothing.
///
/// Placeholders: `$4` = is_admin (bool), `$5` = is_authenticated (bool),
/// `$6` = viewer's space-stripped group ids (text[]).
///
/// The numbering is today's, and it is only a default — see
/// [`local_visibility_predicate_at`], which is where the body lives so that a
/// query needing different positions gets the same rule rather than a second
/// copy of it.
pub(super) fn local_visibility_predicate() -> String {
    local_visibility_predicate_at("$4", "$5", "$6")
}

/// [`local_visibility_predicate`] at explicit placeholder positions.
///
/// Parameterised because the aggregate queries bind fewer things than the
/// listing ones and cannot spare `$1`–`$3`. The alternative was a second copy of
/// the rule with different numbers, and a visibility rule that exists twice is
/// one that will disagree with itself — which on this predicate means a listing
/// more permissive than the download gate, the exact defect its own doc comment
/// warns about.
pub(super) fn local_visibility_predicate_at(admin: &str, authed: &str, groups: &str) -> String {
    format!(
        r#"
            AND (
                {admin}::boolean
                -- RFC 0015 §4.5: `private` is deliberately absent from this
                -- list. Only grants written on the package admit a caller, and
                -- this predicate does not read grants. See the doc comment.
                OR lp.visibility = 'public'
                OR (lp.visibility = 'internal' AND {authed}::boolean)
                OR (
                    lp.visibility = 'team'
                    AND EXISTS (
                        SELECT 1 FROM (
                            SELECT tn.group_id
                            FROM team_namespaces tn
                            WHERE tn.registry = lp.registry
                              AND (lp.name = tn.prefix
                                   OR (LENGTH(lp.name) > LENGTH(tn.prefix)
                                       AND SUBSTRING(lp.name, 1, LENGTH(tn.prefix) + 1)
                                           = tn.prefix || tn.separator))
                            ORDER BY LENGTH(tn.prefix) DESC
                            LIMIT 1
                        ) claim
                        WHERE REPLACE(claim.group_id, ' ', '') = ANY({groups}::text[])
                    )
                )
            )"#
    )
}

/// The same gate as [`local_visibility_predicate`], for a row that reaches the
/// catalogue through `package_statuses` rather than through `local_packages`.
///
/// `record_access` writes a `package_statuses` row on **any allowed** download
/// or metadata read, including on the local path. So the first time an
/// authorised team member pulls a `team`-visibility package, that package
/// acquires a row in a table with no visibility column — and the `proxied` CTE,
/// which had no gate at all, then listed it to anyone who could browse the
/// registry (survey finding 12). The `newest_version` join already carried this
/// reasoning and its own copy of the predicate; the row that reached `agg` in
/// the first place did not.
///
/// Two subqueries rather than one `NOT EXISTS … NOT (…)`, so the rule reads the
/// way it is meant: **a package with no local row is proxied-only and stays
/// public** — its name came from upstream and was never a secret — and a package
/// with local rows is listed only if at least one of them is visible to this
/// viewer, which is the same test `local_pkgs` applies row by row.
///
/// Correlates on `ps`, so the CTE it is spliced into must alias
/// `package_statuses` as `ps`.
pub(super) fn proxied_visibility_predicate(visibility: &str) -> String {
    visible_package_predicate("ps.registry", "ps.package_name", visibility)
}

/// [`proxied_visibility_predicate`], for any table that names a `(registry,
/// package)` pair.
///
/// The rule is the same wherever it is applied and is stated once here: **a row
/// whose package has no local entry is proxied-only and stays visible** — its
/// name came from upstream and was never a secret — and a row whose package has
/// local entries is visible only if at least one of them is visible to this
/// viewer.
///
/// Generalised for the aggregates (RFC 0015 §4.4): `access_events` and
/// `artifact_cache_meta` each carry a `(registry, package_name)` pair and each
/// feeds a dashboard tile, so both need this rule and neither is
/// `package_statuses`. Writing it out per table would be three copies of a
/// disclosure boundary; §4.4's own warning is that a tile *reads* as presentation
/// and is a query.
///
/// Correlates on whatever `registry_col` and `name_col` name, so the caller's
/// query must alias the table it passes.
pub(super) fn visible_package_predicate(
    registry_col: &str,
    name_col: &str,
    visibility: &str,
) -> String {
    format!(
        r#"
            AND (
                NOT EXISTS (
                    SELECT 1 FROM local_packages lp
                    WHERE lp.registry = {registry_col}
                      AND lp.name = {name_col}
                      AND lp.status = 'published'
                )
                OR EXISTS (
                    SELECT 1 FROM local_packages lp
                    WHERE lp.registry = {registry_col}
                      AND lp.name = {name_col}
                      AND lp.status = 'published'
                      {visibility}
                )
            )"#
    )
}

pub(super) fn sort_order_for(sort_by: &ExploreSortBy) -> &'static str {
    match sort_by {
        ExploreSortBy::Name => "package_name ASC",
        ExploreSortBy::Downloads => "total_downloads DESC NULLS LAST",
        ExploreSortBy::Recent => "last_accessed DESC NULLS LAST",
        // The proof's catalog ordering: what this instance most recently
        // fetched from upstream, which is a different question from `Recent`
        // (what a client most recently downloaded from us).
        ExploreSortBy::Fetched => "last_fetched_at DESC NULLS LAST",
    }
}

pub(super) fn determine_package_source(has_proxied: bool, has_local: bool) -> PackageSource {
    match (has_proxied, has_local) {
        (true, true) => PackageSource::Both,
        (false, true) => PackageSource::Local,
        _ => PackageSource::Proxied,
    }
}

pub(super) fn map_explore_entry(r: PgRow) -> ExploreEntry {
    let has_proxied: bool = r.get("has_proxied");
    let has_local: bool = r.get("has_local");
    let source = determine_package_source(has_proxied, has_local);
    let downloads: i64 = r.get("total_downloads");
    // Postgres has no unsigned integers, so every count arrives as i64. A
    // negative one is impossible for COUNT/SUM, but `as u64` on a negative
    // would wrap to something enormous rather than fail, so the sizes clamp.
    let cached_bytes: Option<i64> = r.get("cached_bytes");
    ExploreEntry {
        registry: r.get("registry"),
        name: r.get("package_name"),
        version_count: r.get::<i64, _>("version_count") as u64,
        total_downloads: downloads as u64,
        last_accessed: r.get("last_accessed"),
        source,
        has_blocked: r.get("has_blocked"),
        has_yanked: r.get("has_yanked"),
        cached_versions: r.get::<i64, _>("cached_versions").max(0) as u64,
        cached_bytes: cached_bytes.map(|b| b.max(0) as u64),
        last_fetched_at: r.get("last_fetched_at"),
        newest_version: r.get("newest_version"),
        newest_published_at: r.get("newest_published_at"),
    }
}

// ── Helper conversions ────────────────────────────────────────────────────────

pub(super) fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::Anonymous => "anonymous",
        Role::User => "user",
        Role::Admin => "admin",
    }
}

pub(super) fn str_to_role(s: &str) -> Result<Role, CoreError> {
    s.parse()
        .map_err(|e| CoreError::Database(format!("invalid role in db: {e}")))
}

/// The `access_events.action` column value for an action.
///
/// The table itself lives on [`AccessAction`] in `core`, because the audit-log
/// `?action=` filter has to parse the same vocabulary this writes and a second
/// copy here is how the two spellings would drift apart.
pub(super) fn action_to_str(action: &AccessAction) -> &'static str {
    action.as_str()
}

pub(super) fn str_to_action(s: &str) -> Result<AccessAction, CoreError> {
    AccessAction::from_wire(s)
        .ok_or_else(|| CoreError::Database(format!("invalid access action in db: '{s}'")))
}

pub struct PgPackageRepository {
    pub(super) pool: PgPool,
}

/// Connection pool sizing, taken from `DatabaseConfig` (`crates/config/src/schema/server.rs`).
pub struct PoolOptions {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout_secs: u64,
}

impl PgPackageRepository {
    pub async fn new(database_url: &str, pool_options: PoolOptions) -> Result<Self, CoreError> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(pool_options.max_connections)
            .min_connections(pool_options.min_connections)
            .acquire_timeout(std::time::Duration::from_secs(
                pool_options.acquire_timeout_secs,
            ))
            .connect(database_url)
            .await
            .db_err()?;
        Ok(Self { pool })
    }

    pub async fn run_migrations(&self) -> Result<(), CoreError> {
        embedded_migrator()
            .run(&self.pool)
            .await
            .map_err(|e| CoreError::Database(format!("migration failed: {e}")))?;
        Ok(())
    }

    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }
}

#[async_trait]
impl PackageRepository for PgPackageRepository {
    async fn record_access(&self, event: AccessEvent) -> Result<(), CoreError> {
        crud::record_access_impl(&self.pool, event).await
    }

    async fn get_status(&self, pkg: &PackageId) -> Result<PackageStatus, CoreError> {
        crud::get_status_impl(&self.pool, pkg).await
    }

    async fn blocked_versions(&self, registry: &str, name: &str) -> Result<Vec<String>, CoreError> {
        crud::blocked_versions_impl(&self.pool, registry, name).await
    }

    async fn blocked_changed_at(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, CoreError> {
        crud::blocked_changed_at_impl(&self.pool, registry, name).await
    }

    async fn set_status(&self, pkg: &PackageId, status: PackageStatus) -> Result<(), CoreError> {
        crud::set_status_impl(&self.pool, pkg, status).await
    }

    async fn delete_package(&self, pkg: &PackageId) -> Result<bool, CoreError> {
        crud::delete_package_impl(&self.pool, pkg).await
    }

    async fn list_packages(&self, filter: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
        crud::list_packages_impl(&self.pool, filter).await
    }

    async fn count_packages(&self, filter: PackageFilter) -> Result<u64, CoreError> {
        crud::count_packages_impl(&self.pool, filter).await
    }

    async fn list_events(&self, filter: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        explore::list_events_impl(&self.pool, filter).await
    }

    async fn count_events(&self, filter: EventFilter) -> Result<u64, CoreError> {
        explore::count_events_impl(&self.pool, filter).await
    }

    async fn list_own_downloads(
        &self,
        user_id: &str,
        since: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        explore::list_own_downloads_impl(&self.pool, user_id, since, limit).await
    }

    async fn purge_events_before(&self, before: DateTime<Utc>) -> Result<u64, CoreError> {
        explore::purge_events_before_impl(&self.pool, before).await
    }

    async fn last_downloads(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Vec<(String, DateTime<Utc>)>, CoreError> {
        explore::last_downloads_impl(&self.pool, registry, package).await
    }

    async fn distinct_event_subjects(
        &self,
        contains: Option<&str>,
        limit: u64,
    ) -> Result<Vec<String>, CoreError> {
        explore::distinct_event_subjects_impl(&self.pool, contains, limit).await
    }

    async fn explore_packages(
        &self,
        filter: ExploreFilter,
    ) -> Result<Vec<ExploreEntry>, CoreError> {
        explore::explore_packages_impl(&self.pool, filter).await
    }

    async fn count_explore_packages(&self, filter: ExploreFilter) -> Result<u64, CoreError> {
        explore::count_explore_packages_impl(&self.pool, filter).await
    }

    async fn registry_explore_stats(
        &self,
        accessible_registries: &[String],
        viewer: &ExploreViewer,
    ) -> Result<Vec<RegistryStat>, CoreError> {
        explore::registry_explore_stats_impl(&self.pool, accessible_registries, viewer).await
    }

    async fn registry_package_counts(
        &self,
        registries: &[String],
    ) -> Result<HashMap<String, i64>, CoreError> {
        health::registry_package_counts_impl(&self.pool, registries).await
    }

    async fn registry_event_stats(
        &self,
        registries: &[String],
    ) -> Result<HashMap<String, (Option<DateTime<Utc>>, i64, i64)>, CoreError> {
        health::registry_event_stats_impl(&self.pool, registries).await
    }

    async fn recent_registry_errors(
        &self,
        registry: &str,
        limit: i64,
    ) -> Result<Vec<RecentErrorRecord>, CoreError> {
        health::recent_registry_errors_impl(&self.pool, registry, limit).await
    }
}
