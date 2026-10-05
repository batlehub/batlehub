use chrono::{DateTime, Utc};

use super::{
    local_visibility_predicate, local_visibility_predicate_at, map_explore_entry,
    prepare_registries_param, sort_order_for, str_to_action, str_to_role,
    visible_package_predicate, AccessEvent, AccessResult, CoreError, DbResultExt, EventFilter,
    ExploreEntry, ExploreFilter, ExploreViewer, PackageId, PgPool, RegistryStat, Row,
};

/// The `action = ANY($n)` parameter, or `None` for "every action".
///
/// `NULL` rather than an empty array on purpose: `action = ANY('{}')` is false
/// for every row, so an empty [`EventFilter::actions`] would return nothing
/// instead of everything — the one way this filter could silently hide the
/// trail from an operator who did not ask it to.
fn action_param(filter: &EventFilter) -> Option<Vec<String>> {
    if filter.actions.is_empty() {
        return None;
    }
    Some(
        filter
            .actions
            .iter()
            .map(|a| a.as_str().to_owned())
            .collect(),
    )
}

pub(super) async fn list_events_impl(
    pool: &PgPool,
    filter: EventFilter,
) -> Result<Vec<AccessEvent>, CoreError> {
    let rows = sqlx::query(
        r#"
        SELECT
            id, user_id, user_role, registry, package_name, package_version,
            package_artifact, action, outcome, deny_reason, created_at,
            ip_address, user_agent, throttled_count, detail
        FROM access_events
        WHERE ($1::text IS NULL OR registry = $1)
          AND ($2::text IS NULL OR user_id = $2)
          AND ($3::timestamptz IS NULL OR created_at >= $3)
          AND ($4::timestamptz IS NULL OR created_at <= $4)
          AND ($5::boolean = false OR outcome = 'denied')
          AND ($8::text IS NULL OR package_name = $8)
          AND ($9::text[] IS NULL OR action = ANY($9))
        ORDER BY created_at DESC
        LIMIT $6 OFFSET $7
        "#,
    )
    .bind(&filter.registry)
    .bind(&filter.user_id)
    .bind(filter.from)
    .bind(filter.to)
    .bind(filter.denied_only)
    .bind(filter.limit as i64)
    .bind(filter.offset as i64)
    .bind(&filter.package_name)
    .bind(action_param(&filter))
    .fetch_all(pool)
    .await
    .db_err()?;

    rows.iter().map(map_access_event).collect()
}

/// Build an [`AccessEvent`] from one `access_events` row.
///
/// Every query feeding this must select the same thirteen columns; sqlx 0.9
/// rejects non-literal SQL, so the list is repeated per query rather than
/// shared through a `const`.
pub(super) fn map_access_event(r: &sqlx::postgres::PgRow) -> Result<AccessEvent, CoreError> {
    let outcome: String = r.get("outcome");
    // Account-wide/network-wide events store NULL in all three coordinate
    // columns (see migration 030); only build a `PackageId` when the row
    // actually has one.
    let registry: Option<String> = r.get("registry");
    let package_name: Option<String> = r.get("package_name");
    let package_version: Option<String> = r.get("package_version");
    let package_artifact: Option<String> = r.get("package_artifact");
    let package_id = match (registry, package_name, package_version) {
        (Some(registry), Some(name), Some(version)) => Some(PackageId {
            registry,
            name,
            version,
            artifact: package_artifact,
        }),
        _ => None,
    };
    Ok(AccessEvent {
        id: r.get("id"),
        user_id: r.get("user_id"),
        user_role: str_to_role(r.get::<&str, _>("user_role"))?,
        package_id,
        action: str_to_action(r.get::<&str, _>("action"))?,
        result: match outcome.as_str() {
            "denied" => AccessResult::Denied {
                reason: r
                    .get::<Option<String>, _>("deny_reason")
                    .unwrap_or_default(),
            },
            "error" => AccessResult::ProxyError {
                reason: r
                    .get::<Option<String>, _>("deny_reason")
                    .unwrap_or_default(),
            },
            "allowed" => AccessResult::Allowed,
            other => {
                return Err(CoreError::Database(format!(
                    "invalid access outcome in db: '{other}'"
                )))
            }
        },
        timestamp: r.get("created_at"),
        ip_address: r.get("ip_address"),
        user_agent: r.get("user_agent"),
        throttled_count: r
            .get::<Option<i32>, _>("throttled_count")
            .map(|n| n.max(0) as u32),
        detail: r.get("detail"),
    })
}

/// One principal's own allowed downloads, newest first.
///
/// The `user_id`, `action` and `outcome` predicates are written here rather
/// than assembled by a caller: this is what `GET /api/v1/me/downloads` reads,
/// and a `where` clause a handler can forget is a handler that returns someone
/// else's history (RFC 0004 §7).
pub(super) async fn list_own_downloads_impl(
    pool: &PgPool,
    user_id: &str,
    since: DateTime<Utc>,
    limit: u64,
) -> Result<Vec<AccessEvent>, CoreError> {
    let rows = sqlx::query(
        r#"
        SELECT
            id, user_id, user_role, registry, package_name, package_version,
            package_artifact, action, outcome, deny_reason, created_at,
            ip_address, user_agent, throttled_count, detail
        FROM access_events
        WHERE user_id = $1
          AND action = 'download'
          AND outcome = 'allowed'
          AND created_at >= $2
          AND package_name IS NOT NULL
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(user_id)
    .bind(since)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .db_err()?;

    rows.iter().map(map_access_event).collect()
}

pub(super) async fn count_events_impl(
    pool: &PgPool,
    filter: EventFilter,
) -> Result<u64, CoreError> {
    let row = sqlx::query(
        r#"
        SELECT COUNT(*) AS total
        FROM access_events
        WHERE ($1::text IS NULL OR registry = $1)
          AND ($2::text IS NULL OR user_id = $2)
          AND ($3::timestamptz IS NULL OR created_at >= $3)
          AND ($4::timestamptz IS NULL OR created_at <= $4)
          AND ($5::boolean = false OR outcome = 'denied')
          AND ($6::text IS NULL OR package_name = $6)
          AND ($7::text[] IS NULL OR action = ANY($7))
        "#,
    )
    .bind(&filter.registry)
    .bind(&filter.user_id)
    .bind(filter.from)
    .bind(filter.to)
    .bind(filter.denied_only)
    .bind(&filter.package_name)
    .bind(action_param(&filter))
    .fetch_one(pool)
    .await
    .db_err()?;

    let count: i64 = row.try_get("total").unwrap_or(0);
    Ok(count as u64)
}

/// Assemble the catalog query, splicing in the sort order and the viewer's
/// visibility predicate.
///
/// Separate from [`explore_packages_impl`] so the tests at the bottom of this
/// file can inspect the SQL without a database. Both spliced fragments are
/// trusted, statically-known strings — `visibility` is
/// [`local_visibility_predicate`](super::local_visibility_predicate) and `order` comes from [`sort_order_for`]'s
/// closed match — which is what makes the `AssertSqlSafe` at the call site true.
///
/// **Every literal brace in this string must be doubled**, comments included: a
/// `{visibility}` written inside a `--` comment still expands, and because the
/// predicate is multi-line only its first line stays commented out — the rest
/// lands in the query as live SQL. That produced a `syntax error at or near
/// "AND"` that no amount of type-checking catches, which is what
/// `no_placeholder_survives_into_live_sql` now guards.
fn explore_sql(order: &str, visibility: &str) -> String {
    let proxied_visibility = super::proxied_visibility_predicate(visibility);
    format!(
        r#"
        WITH proxied AS (
            SELECT
                ps.registry,
                ps.package_name,
                COUNT(DISTINCT ps.package_version)::bigint AS version_count,
                BOOL_OR(ps.status = 'blocked') AS has_blocked,
                -- Only a local publish can be yanked; a proxied artifact is
                -- withdrawn upstream, which this instance learns about as an
                -- absent version rather than as a flag.
                false AS has_yanked,
                true AS has_proxied,
                false AS has_local
            FROM package_statuses ps
            WHERE ($1::text IS NULL OR ps.registry = $1)
              AND ($2::text IS NULL OR ps.package_name ILIKE '%' || $2 || '%')
              AND ($3::text[] IS NULL OR ps.registry = ANY($3::text[]))
              {proxied_visibility}
            GROUP BY ps.registry, ps.package_name
        ),
        local_pkgs AS (
            SELECT
                lp.registry,
                lp.name AS package_name,
                COUNT(DISTINCT lp.version)::bigint AS version_count,
                -- A yank used to be reported in this column, which made an
                -- owner withdrawing a version indistinguishable from an
                -- operator blocking one. Two different facts, two different
                -- remedies; they now travel separately.
                false AS has_blocked,
                BOOL_OR(lp.yanked) AS has_yanked,
                false AS has_proxied,
                true AS has_local
            FROM local_packages lp
            WHERE lp.status = 'published'
              AND ($1::text IS NULL OR lp.registry = $1)
              AND ($2::text IS NULL OR lp.name ILIKE '%' || $2 || '%')
              AND ($3::text[] IS NULL OR lp.registry = ANY($3::text[]))
              {visibility}
            GROUP BY lp.registry, lp.name
        ),
        combined AS (
            SELECT * FROM proxied
            UNION ALL
            SELECT * FROM local_pkgs
        ),
        agg AS (
            SELECT
                registry,
                package_name,
                SUM(version_count)::bigint AS version_count,
                BOOL_OR(has_blocked) AS has_blocked,
                BOOL_OR(has_yanked) AS has_yanked,
                BOOL_OR(has_proxied) AS has_proxied,
                BOOL_OR(has_local) AS has_local
            FROM combined
            GROUP BY registry, package_name
            -- The `name_in` restriction, applied once here rather than in each
            -- CTE: it is only ever set by the README search, whose candidate
            -- list is capped, so an aggregate-level filter is correct and the
            -- CTEs stay the shape every other query reads them in.
            --
            -- One array of `registry/name` rather than two parallel arrays: a
            -- registry name cannot contain a `/`, so the concatenation is
            -- unambiguous even for a scoped package like `npm/@scope/pkg`.
            HAVING ($9::text[] IS NULL
                    OR (registry || '/' || package_name) = ANY($9::text[]))
        )
        SELECT
            agg.registry,
            agg.package_name,
            agg.version_count,
            agg.has_blocked,
            agg.has_yanked,
            agg.has_proxied,
            agg.has_local,
            COALESCE(ae.total_downloads, 0)::bigint AS total_downloads,
            ae.last_accessed,
            COALESCE(cm.cached_versions, 0)::bigint AS cached_versions,
            cm.cached_bytes,
            cm.last_fetched_at,
            nv.newest_version,
            nv.newest_published_at
        FROM agg
        LEFT JOIN LATERAL (
            SELECT COUNT(*)::bigint AS total_downloads, MAX(created_at) AS last_accessed
            FROM access_events
            WHERE registry = agg.registry AND package_name = agg.package_name
        ) ae ON true
        -- What this instance actually holds. `agg` is built from
        -- `package_statuses` and `local_packages`, both of which record that a
        -- version is *known*; only `artifact_cache_meta` records that its bytes
        -- are here. The difference between those two is exactly the Pending
        -- state, so it needs its own join rather than an inference from
        -- `version_count`.
        --
        -- SUM over a column that is NULL for every row is NULL, not 0, which is
        -- what carries "size unknown" through to the DTO: artifacts cached
        -- before 004 added `size_bytes` have no number, and reporting 0 for
        -- them would claim we hold empty files.
        -- COUNT(DISTINCT version), not COUNT(*): `artifact_cache_meta` is keyed
        -- on `artifact_key`, which for multi-file registries is one row *per
        -- file*. A Maven version with pom + jar + sources + javadoc is four
        -- rows and one version, and COUNT(*) reported it as four cached
        -- versions of a package whose `version_count` is 1.
        LEFT JOIN LATERAL (
            SELECT COUNT(DISTINCT acm.version)::bigint AS cached_versions,
                   SUM(acm.size_bytes)::bigint AS cached_bytes,
                   MAX(acm.cached_at) AS last_fetched_at
            FROM artifact_cache_meta acm
            WHERE acm.registry = agg.registry AND acm.package_name = agg.package_name
        ) cm ON true
        -- The version we most recently obtained, from either source, ordered by
        -- the timestamp that means "we got it": `cached_at` for a proxied
        -- artifact, `published_at` for a local one. NOT `MAX(version)` — version
        -- strings sort lexically, where '1.10.0' precedes '1.9.0', and no
        -- semantic ordering is available in SQL here.
        --
        -- `published_at` is carried out alongside because the release-age gate
        -- needs it for this specific version; it is NULL for proxied artifacts,
        -- which is a case the gate already defines behaviour for.
        LEFT JOIN LATERAL (
            SELECT v.version AS newest_version, v.published_at AS newest_published_at
            FROM (
                SELECT acm.version, NULL::timestamptz AS published_at, acm.cached_at AS obtained_at
                FROM artifact_cache_meta acm
                WHERE acm.registry = agg.registry AND acm.package_name = agg.package_name
                UNION ALL
                -- The same `{{visibility}}` predicate the `local_pkgs` CTE
                -- applies. A row can reach `agg` through `package_statuses`
                -- alone (`record_access` writes one on any allowed download or
                -- metadata read, including on the local path), so without it
                -- this join hands an anonymous caller the newest version string
                -- and publish date of a `team`-visibility package that its own
                -- owner happened to pull once.
                SELECT lp.version, lp.published_at, lp.published_at AS obtained_at
                FROM local_packages lp
                WHERE lp.registry = agg.registry AND lp.name = agg.package_name
                  AND lp.status = 'published'
                  {visibility}
            ) v
            ORDER BY v.obtained_at DESC NULLS LAST
            LIMIT 1
        ) nv ON true
        ORDER BY {order}
        LIMIT $7 OFFSET $8
        "#
    )
}

/// `name_in` as the array the query binds, or `None` for "no restriction".
///
/// `None` rather than an empty array: `= ANY('{}')` is false for every row, so
/// an empty array would silently return nothing where the caller meant
/// everything — which is the difference between a filter that is off and a
/// filter that rejects.
fn prepare_name_in_param(name_in: &[(String, String)]) -> Option<Vec<String>> {
    if name_in.is_empty() {
        return None;
    }
    Some(
        name_in
            .iter()
            .map(|(registry, name)| format!("{registry}/{name}"))
            .collect(),
    )
}

pub(super) async fn explore_packages_impl(
    pool: &PgPool,
    filter: ExploreFilter,
) -> Result<Vec<ExploreEntry>, CoreError> {
    let registries = prepare_registries_param(&filter.registries);
    let groups = filter.viewer.normalised_groups();
    let sql = explore_sql(
        sort_order_for(&filter.sort_by),
        &local_visibility_predicate(),
    );

    // $4/$5/$6 are the viewer parameters consumed by LOCAL_VISIBILITY_PREDICATE.
    // They sit before limit/offset so the same fragment can be spliced into this
    // query and into `count_explore_packages_impl` (which has no limit/offset)
    // without renumbering — the two must apply an identical predicate or the
    // reported total would not match the rows returned.
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&filter.registry)
        .bind(&filter.name_contains)
        .bind(registries)
        .bind(filter.viewer.is_admin)
        .bind(filter.viewer.is_authenticated)
        .bind(&groups)
        .bind(filter.limit as i64)
        .bind(filter.offset as i64)
        .bind(prepare_name_in_param(&filter.name_in))
        .fetch_all(pool)
        .await
        .db_err()?;

    Ok(rows.into_iter().map(map_explore_entry).collect())
}

/// The row count [`explore_sql`]'s page is drawn from.
///
/// Must apply exactly the same visibility predicate as [`explore_sql`], or the
/// pagination total would count rows the page itself filters out — the UI would
/// show "37 packages" over an empty page. The brace-doubling rule from
/// [`explore_sql`] applies here too.
fn count_explore_sql(visibility: &str) -> String {
    let proxied_visibility = super::proxied_visibility_predicate(visibility);
    format!(
        r#"
        WITH proxied AS (
            SELECT ps.registry, ps.package_name
            FROM package_statuses ps
            WHERE ($1::text IS NULL OR ps.registry = $1)
              AND ($2::text IS NULL OR ps.package_name ILIKE '%' || $2 || '%')
              AND ($3::text[] IS NULL OR ps.registry = ANY($3::text[]))
              {proxied_visibility}
        ),
        local_pkgs AS (
            SELECT lp.registry, lp.name AS package_name
            FROM local_packages lp
            WHERE lp.status = 'published'
              AND ($1::text IS NULL OR lp.registry = $1)
              AND ($2::text IS NULL OR lp.name ILIKE '%' || $2 || '%')
              AND ($3::text[] IS NULL OR lp.registry = ANY($3::text[]))
              {visibility}
        )
        SELECT COUNT(*) AS total FROM (
            SELECT registry, package_name FROM proxied
            UNION
            SELECT registry, package_name FROM local_pkgs
        ) combined
        -- The same `name_in` restriction `explore_sql` applies, so the reported
        -- total cannot disagree with the rows returned. Its parameter number
        -- differs because this query has no limit/offset — the two are kept in
        -- step by `the_two_queries_apply_the_same_name_restriction`.
        WHERE ($7::text[] IS NULL
               OR (combined.registry || '/' || combined.package_name) = ANY($7::text[]))
        "#
    )
}

pub(super) async fn count_explore_packages_impl(
    pool: &PgPool,
    filter: ExploreFilter,
) -> Result<u64, CoreError> {
    let registries = prepare_registries_param(&filter.registries);
    let groups = filter.viewer.normalised_groups();
    let sql = count_explore_sql(&local_visibility_predicate());

    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&filter.registry)
        .bind(&filter.name_contains)
        .bind(registries)
        .bind(filter.viewer.is_admin)
        .bind(filter.viewer.is_authenticated)
        .bind(&groups)
        .bind(prepare_name_in_param(&filter.name_in))
        .fetch_one(pool)
        .await
        .db_err()?;

    let count: i64 = row.try_get("total").unwrap_or(0);
    Ok(count as u64)
}

/// Per-registry package counts, download totals and cached bytes — RFC 0015 §4.4.
///
/// > An aggregate is a listing that has been counted. Every one of those tiles
/// > is a query over packages, and it is filtered by the caller's grants exactly
/// > like a version index — a number computed over rows the caller may not see
/// > is a disclosure whether or not the rows themselves are returned.
///
/// This query used to do neither half of that, and §4.4 predicted both:
///
/// - **It had no visibility predicate at all**, so `package_count` and
///   `total_downloads` were computed over `internal`, `team` and `private`
///   packages alike. *"You have 47 packages"* over a set the caller can see three
///   of discloses the other 44 exist — survey finding 12, one abstraction level
///   up, on the surface §4.4 says it *"will arrive a fourth time"*.
/// - **An empty `accessible_registries` bound `NULL`**, and `$1 IS NULL OR …`
///   reads that as *every* registry. That is survey finding 2 verbatim: a caller
///   with no browsable registry at all was handed the whole estate's numbers by
///   the one query whose job is to scope them. `explore_packages` refuses this
///   case in its handler; the aggregate beside it did not.
///
/// Both are closed here rather than in the caller: an empty list now binds an
/// empty array, so the predicate is `= ANY('{}')` and matches nothing by
/// construction — *"a predicate that is vacuous rather than absent"* is the shape
/// finding 2 came in on, and the fix is to make the vacuous reading
/// unrepresentable.
///
/// # Grants are deliberately not part of this filter
///
/// §4.4 asks for grants as well as visibility, and the aggregate must agree with
/// the listing it summarises — so it filters on exactly what
/// `explore_packages`/`count_explore_packages` filter on, which today is registry
/// access plus visibility. Grants do not yet filter the explore catalogue at all,
/// and an aggregate stricter than its own listing would be the same disagreement
/// in the opposite direction: the tile says 0 and the page beside it shows three.
/// The two have to move together, and that is one change rather than this one.
pub(super) async fn registry_explore_stats_impl(
    pool: &PgPool,
    accessible_registries: &[String],
    viewer: &ExploreViewer,
) -> Result<Vec<RegistryStat>, CoreError> {
    // No `Option`, and no `IS NULL` arm to read as "everything": an empty scope
    // is an empty result. See the doc comment.
    let registries = accessible_registries.to_vec();
    if registries.is_empty() {
        return Ok(Vec::new());
    }
    let groups = viewer.normalised_groups();

    // $1 registries, $2 is_admin, $3 is_authenticated, $4 groups.
    let local_visibility = local_visibility_predicate_at("$2", "$3", "$4");
    // The same rule, correlated on each table that feeds a tile. One body, three
    // call sites: a disclosure boundary written out three times is one that will
    // disagree with itself.
    let statuses_visible =
        visible_package_predicate("ps.registry", "ps.package_name", &local_visibility);
    let events_visible =
        visible_package_predicate("ae.registry", "ae.package_name", &local_visibility);
    let cached_visible =
        visible_package_predicate("acm.registry", "acm.package_name", &local_visibility);

    let sql = format!(
        r#"
        WITH pkg_counts AS (
            SELECT registry, COUNT(DISTINCT package_name)::bigint AS package_count
            FROM (
                SELECT ps.registry, ps.package_name FROM package_statuses ps
                WHERE ps.registry = ANY($1::text[])
                  {statuses_visible}
                UNION
                SELECT lp.registry, lp.name AS package_name FROM local_packages lp
                WHERE lp.status = 'published'
                  AND lp.registry = ANY($1::text[])
                  {local_visibility}
            ) combined
            GROUP BY registry
        ),
        -- The `SUM`'s sibling, and the shape §4.4 calls the dangerous one:
        -- "a count taken first and trimmed afterwards" is impossible here
        -- because the filter is inside the aggregation, which is the whole
        -- requirement.
        download_counts AS (
            SELECT ae.registry, COUNT(*)::bigint AS total_downloads
            FROM access_events ae
            WHERE ae.registry = ANY($1::text[])
              {events_visible}
            GROUP BY ae.registry
        ),
        -- SUM over all-NULL sizes is NULL, which is how "we hold artifacts but
        -- recorded no sizes for them" stays distinguishable from "we hold
        -- nothing". A registry with no cached artifacts at all has no row here
        -- and the LEFT JOIN leaves it NULL too — both are honestly unknown.
        --
        -- §4.4: "A `SUM` cannot be trimmed at all, which is what makes this the
        -- version of rule 1 that fails silently." So it is filtered in place.
        cached_sizes AS (
            SELECT acm.registry, SUM(acm.size_bytes)::bigint AS cached_bytes
            FROM artifact_cache_meta acm
            WHERE acm.registry = ANY($1::text[])
              {cached_visible}
            GROUP BY acm.registry
        )
        SELECT
            pc.registry,
            pc.package_count,
            COALESCE(dc.total_downloads, 0) AS total_downloads,
            cs.cached_bytes
        FROM pkg_counts pc
        LEFT JOIN download_counts dc ON dc.registry = pc.registry
        LEFT JOIN cached_sizes cs ON cs.registry = pc.registry
        ORDER BY pc.package_count DESC
        "#
    );

    // `AssertSqlSafe`, as the sibling queries above: every interpolated fragment
    // is a compile-time constant assembled by `local_visibility_predicate_at` and
    // `visible_package_predicate`, and every value is a bind parameter.
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&registries)
        .bind(viewer.is_admin)
        .bind(viewer.is_authenticated)
        .bind(&groups)
        .fetch_all(pool)
        .await
        .db_err()?;

    let stats = rows
        .into_iter()
        .map(|r| {
            let pkg_count: i64 = r.get("package_count");
            let downloads: i64 = r.get("total_downloads");
            let cached_bytes: Option<i64> = r.get("cached_bytes");
            RegistryStat {
                registry: r.get("registry"),
                package_count: pkg_count as u64,
                total_downloads: downloads as u64,
                cached_bytes: cached_bytes.map(|b| b.max(0) as u64),
            }
        })
        .collect();

    Ok(stats)
}

pub(super) async fn purge_events_before_impl(
    pool: &PgPool,
    before: DateTime<Utc>,
) -> Result<u64, CoreError> {
    // Access class only (RFC 0036 §4.2): a purge removes traffic, never the
    // evidence — so a second purge cannot erase the first one's row.
    let result = sqlx::query(
        "DELETE FROM access_events WHERE created_at < $1 \
         AND action IN ('download', 'view_metadata')",
    )
    .bind(before)
    .execute(pool)
    .await
    .db_err()?;
    Ok(result.rows_affected())
}

/// The newest allowed download of each version of one package (RFC 0016 §4.3).
///
/// `DISTINCT ON` rather than `GROUP BY`, so the planner walks
/// `idx_access_events_pkg_allowed_recent` — `(registry, package_name,
/// package_version, outcome, created_at DESC)` — and stops at the first row per
/// version instead of aggregating the whole history of a package that may have
/// been downloaded a million times.
///
/// `action = 'download'` is the whole of the sidecar rule as far as SQL is
/// concerned: the `Download`/`ViewMetadata` split is drawn at record time by
/// `PackageId::is_verification_sidecar`, so a `.sha1` never wrote a `download`
/// row to exclude here and a `.pom` did write one to include.
pub(super) async fn last_downloads_impl(
    pool: &PgPool,
    registry: &str,
    package: &str,
) -> Result<Vec<(String, DateTime<Utc>)>, CoreError> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (package_version) package_version, created_at \
         FROM access_events \
         WHERE registry = $1 AND package_name = $2 \
           AND action = 'download' AND outcome = 'allowed' \
         ORDER BY package_version, created_at DESC",
    )
    .bind(registry)
    .bind(package)
    .fetch_all(pool)
    .await
    .db_err()?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get("package_version"), r.get("created_at")))
        .collect())
}

/// Distinct audit subjects, most-recently-active first (RFC 0004-bis A8).
///
/// Ordered by last activity rather than alphabetically: a suggesting field is
/// answering "who has been doing things here", and the operator is almost
/// always looking for someone recent. `ILIKE` because a subject typed as
/// `alice` should find `oidc:alice` — matching only a prefix would reproduce
/// the very failure this endpoint exists to fix.
pub(super) async fn distinct_event_subjects_impl(
    pool: &PgPool,
    contains: Option<&str>,
    limit: u64,
) -> Result<Vec<String>, CoreError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT user_id FROM access_events \
         WHERE user_id IS NOT NULL AND ($1::text IS NULL OR user_id ILIKE '%' || $1 || '%') \
         GROUP BY user_id ORDER BY MAX(created_at) DESC LIMIT $2",
    )
    .bind(contains)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .db_err()?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Structural checks on the two `format!`-assembled catalog queries.
///
/// The rest of this module is only exercised against a real database
/// (`crates/adapters/tests/pg_explore_*.rs`, opt-in via `DATABASE_URL`), which
/// means a malformed query reaches CI's Postgres job and nothing before it.
/// These tests need no database: they assert the *shape* of the generated SQL,
/// and they exist because of a specific failure.
///
/// A comment reading ``-- The same `{visibility}` predicate …`` was added to
/// `explore_sql`'s format string. `format!` expanded it like any other
/// placeholder, and since `local_visibility_predicate()` is multi-line, only its
/// first line stayed behind the `--`; the remainder landed directly after a
/// `UNION ALL` as live SQL. The result compiled, type-checked, and failed every
/// pg_explore test with `syntax error at or near "AND"`.
#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::ExploreSortBy;

    /// Every ordering the sort selector can splice in, so a new
    /// [`ExploreSortBy`] variant is covered by whichever of these fails first.
    const ALL_SORTS: &[ExploreSortBy] = &[
        ExploreSortBy::Name,
        ExploreSortBy::Downloads,
        ExploreSortBy::Recent,
        ExploreSortBy::Fetched,
    ];

    /// A stand-in for the visibility predicate, shaped like the real one: it
    /// leads with a newline and spans several lines, which is exactly what made
    /// the original leak partially invisible. Distinctive enough to count.
    const SENTINEL: &str =
        "\n              AND (\n                  sentinel_visibility\n              )";

    /// The query with `--` comments removed — what Postgres would actually
    /// parse. Line-wise stripping is only exact while no string literal in these
    /// queries contains `--`, so the parity check enforces that precondition
    /// rather than trusting it.
    fn live_sql(sql: &str) -> String {
        sql.lines()
            .map(|line| {
                let code = match line.find("--") {
                    Some(i) => &line[..i],
                    None => line,
                };
                assert!(
                    code.matches('\'').count() % 2 == 0,
                    "`--` inside a string literal — live_sql() can no longer strip \
                     comments line-wise, and these guards would silently weaken: {line}"
                );
                code
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The word following each `UNION` / `UNION ALL` in the live SQL. Every one
    /// must be `SELECT`; the leak put an `AND` here.
    fn union_followers(sql: &str) -> Vec<String> {
        let live = live_sql(sql);
        let tokens: Vec<&str> = live.split_whitespace().collect();
        tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.eq_ignore_ascii_case("UNION"))
            .map(|(i, _)| {
                let mut next = i + 1;
                if tokens
                    .get(next)
                    .is_some_and(|t| t.eq_ignore_ascii_case("ALL"))
                {
                    next += 1;
                }
                tokens
                    .get(next)
                    .copied()
                    .unwrap_or("<end of query>")
                    .to_uppercase()
            })
            .collect()
    }

    /// The text of one CTE, from its `name AS (` to the start of the next.
    ///
    /// Counting occurrences over the whole query cannot tell "both CTEs are
    /// gated" from "one CTE is gated twice", and that distinction is the whole
    /// of survey finding 12: the `newest_version` join carried the predicate
    /// while the `proxied` CTE feeding it carried none.
    fn cte<'a>(sql: &'a str, name: &str) -> &'a str {
        let start = sql
            .find(&format!("{name} AS ("))
            .unwrap_or_else(|| panic!("no `{name}` CTE in the query"));
        let rest = &sql[start..];
        // CTEs are separated by `),\n` at this indentation; the last one ends
        // the WITH block. Either way the next `\n        )` closes this one.
        match rest.find("\n        )") {
            Some(end) => &rest[..end],
            None => rest,
        }
    }

    #[test]
    fn visibility_predicate_reaches_live_sql_exactly_at_its_three_splice_points() {
        // `proxied`, `local_pkgs`, and the lateral join that picks the newest
        // version. The first of those was survey finding 12.
        for sort in ALL_SORTS {
            let sql = explore_sql(sort_order_for(sort), SENTINEL);
            assert_eq!(
                live_sql(&sql).matches("sentinel_visibility").count(),
                3,
                "the visibility predicate is spliced somewhere it should not be \
                 (a `{{visibility}}` in a comment?) or has stopped being applied"
            );
        }
    }

    /// **Both** CTEs, named individually.
    ///
    /// `record_access` writes a `package_statuses` row on every allowed read,
    /// including local ones, so a `team`-visibility package acquires one the
    /// first time its own owner pulls it. Ungated, the `proxied` CTE then listed
    /// that package to anyone who could browse the registry, and the
    /// `local_pkgs` predicate never saw it.
    #[test]
    fn both_catalogue_ctes_are_gated_by_visibility() {
        for sort in ALL_SORTS {
            let sql = live_sql(&explore_sql(sort_order_for(sort), SENTINEL));
            for name in ["proxied", "local_pkgs"] {
                assert!(
                    cte(&sql, name).contains("sentinel_visibility"),
                    "the `{name}` CTE lists packages without a visibility gate"
                );
            }
        }
        let count = live_sql(&count_explore_sql(SENTINEL));
        for name in ["proxied", "local_pkgs"] {
            assert!(
                cte(&count, name).contains("sentinel_visibility"),
                "the count query's `{name}` CTE counts packages without a visibility gate"
            );
        }
    }

    #[test]
    fn count_query_applies_the_visibility_predicate_at_both_splice_points() {
        // A count that skips the predicate reports a total the page it
        // paginates cannot show — in either direction.
        assert_eq!(
            live_sql(&count_explore_sql(SENTINEL))
                .matches("sentinel_visibility")
                .count(),
            2
        );
    }

    #[test]
    fn every_union_is_followed_by_a_select() {
        for sort in ALL_SORTS {
            let sql = explore_sql(sort_order_for(sort), &local_visibility_predicate());
            for follower in union_followers(&sql) {
                assert_eq!(follower, "SELECT", "malformed UNION in the explore query");
            }
        }
        for follower in union_followers(&count_explore_sql(&local_visibility_predicate())) {
            assert_eq!(follower, "SELECT", "malformed UNION in the count query");
        }
    }

    #[test]
    fn no_placeholder_survives_into_live_sql() {
        // The other direction of the same mistake: doubled braces render as
        // literal `{…}`, which is fine in a comment and a syntax error anywhere
        // Postgres can see it.
        let mut queries: Vec<String> = ALL_SORTS
            .iter()
            .map(|sort| explore_sql(sort_order_for(sort), &local_visibility_predicate()))
            .collect();
        queries.push(count_explore_sql(&local_visibility_predicate()));
        for sql in queries {
            let live = live_sql(&sql);
            assert!(
                !live.contains('{') && !live.contains('}'),
                "a format placeholder escaped into executable SQL"
            );
        }
    }
}
