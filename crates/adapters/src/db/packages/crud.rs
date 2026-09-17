use super::{
    action_to_str, map_package_summary, prepare_registries_param, role_to_str, AccessAction,
    AccessEvent, AccessResult, CoreError, DateTime, DbResultExt, PackageFilter, PackageId,
    PackageStatus, PackageSummary, PgPool, Row, Utc, Uuid,
};

pub(super) async fn record_access_impl(pool: &PgPool, event: AccessEvent) -> Result<(), CoreError> {
    let (outcome, deny_reason): (&str, Option<String>) = match &event.result {
        AccessResult::Allowed => ("allowed", None),
        AccessResult::Denied { reason } => ("denied", Some(reason.clone())),
        AccessResult::ProxyError { reason } => ("error", Some(reason.clone())),
    };

    // Account-wide/network-wide actions (e.g. block_user, block_ip) carry no
    // package coordinate; the columns are nullable to allow this (migration
    // 030_access_events_nullable_target.sql).
    let registry = event.package_id.as_ref().map(|p| p.registry.as_str());
    let package_name = event.package_id.as_ref().map(|p| p.name.as_str());
    let package_version = event.package_id.as_ref().map(|p| p.version.as_str());
    let package_artifact = event
        .package_id
        .as_ref()
        .and_then(|p| p.artifact.as_deref());

    let query = sqlx::query(
        r#"
        INSERT INTO access_events
            (id, user_id, user_role, registry, package_name, package_version,
             package_artifact, action, outcome, deny_reason, created_at,
             ip_address, user_agent)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        "#,
    )
    .bind(event.id)
    .bind(&event.user_id)
    .bind(role_to_str(&event.user_role))
    .bind(registry)
    .bind(package_name)
    .bind(package_version)
    .bind(package_artifact)
    .bind(action_to_str(&event.action))
    .bind(outcome)
    .bind(deny_reason)
    .bind(event.timestamp)
    .bind(&event.ip_address)
    .bind(&event.user_agent)
    .execute(pool);
    crate::db::timed_query("record_access", query)
        .await
        .db_err()?;

    // Ensure the package appears in list_packages by creating an 'available' status
    // row on first access. DO NOTHING preserves any existing blocked status.
    // Restricted to the actions that always carry a real, version-specific
    // package coordinate — ownership/visibility/account-wide actions must not
    // spuriously create (or reuse an empty-version) package_statuses row.
    let creates_status_row = matches!(event.result, AccessResult::Allowed)
        && matches!(
            event.action,
            AccessAction::Download
                | AccessAction::ViewMetadata
                | AccessAction::Block
                | AccessAction::Unblock
        );
    if creates_status_row {
        if let Some(pkg) = &event.package_id {
            sqlx::query(
                r#"
                INSERT INTO package_statuses
                    (id, registry, package_name, package_version, package_artifact,
                     status, updated_at)
                VALUES ($1, $2, $3, $4, $5, 'available', NOW())
                ON CONFLICT (registry, package_name, package_version, COALESCE(package_artifact, ''))
                DO NOTHING
                "#,
            )
            .bind(Uuid::new_v4())
            .bind(&pkg.registry)
            .bind(&pkg.name)
            .bind(&pkg.version)
            .bind(&pkg.artifact)
            .execute(pool)
            .await
            .db_err()?;
        }
    }

    Ok(())
}

/// Every blocked version of one package.
///
/// Deliberately not routed through `list_packages_impl`: that query LATERAL-joins
/// `access_events` twice to compute audit counts nobody needs here, and this runs
/// on every version-listing request. One indexed predicate, one column.
///
/// Rows are matched on `(registry, package_name)` regardless of
/// `package_artifact`, so blocking a single artifact of a multi-file version
/// (a Maven classifier, say) hides that version from the listing entirely. That
/// is the safe direction: a version whose bytes are partly blocked should not be
/// advertised as installable.
pub(super) async fn blocked_versions_impl(
    pool: &PgPool,
    registry: &str,
    name: &str,
) -> Result<Vec<String>, CoreError> {
    let rows = sqlx::query(
        r#"
        SELECT DISTINCT package_version
        FROM package_statuses
        WHERE registry = $1 AND package_name = $2 AND status = 'blocked'
        "#,
    )
    .bind(registry)
    .bind(name)
    .fetch_all(pool)
    .await
    .db_err()?;

    Ok(rows.into_iter().map(|r| r.get("package_version")).collect())
}

/// The newest `blocked_at` among one package's block rows (RFC 0031 §4.4).
///
/// A single indexed aggregate rather than the default's listing query, for the
/// same reason [`blocked_versions_impl`] overrides its own default.
pub(super) async fn blocked_changed_at_impl(
    pool: &PgPool,
    registry: &str,
    name: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, CoreError> {
    let row = sqlx::query(
        r#"
        SELECT MAX(blocked_at) AS newest
        FROM package_statuses
        WHERE registry = $1 AND package_name = $2 AND status = 'blocked'
        "#,
    )
    .bind(registry)
    .bind(name)
    .fetch_one(pool)
    .await
    .db_err()?;

    Ok(row.try_get("newest").ok().flatten())
}

pub(super) async fn get_status_impl(
    pool: &PgPool,
    pkg: &PackageId,
) -> Result<PackageStatus, CoreError> {
    let row = sqlx::query(
        r#"
        SELECT status, block_reason, blocked_by, blocked_at
        FROM package_statuses
        WHERE registry = $1 AND package_name = $2 AND package_version = $3
          AND (package_artifact IS NOT DISTINCT FROM $4)
        "#,
    )
    .bind(&pkg.registry)
    .bind(&pkg.name)
    .bind(&pkg.version)
    .bind(&pkg.artifact)
    .fetch_optional(pool)
    .await
    .db_err()?;

    match row {
        None => Ok(PackageStatus::Available),
        Some(r) => {
            let status: String = r.get("status");
            if status == "blocked" {
                Ok(PackageStatus::Blocked {
                    reason: r
                        .get::<Option<String>, _>("block_reason")
                        .unwrap_or_default(),
                    blocked_by: r.get::<Option<String>, _>("blocked_by").unwrap_or_default(),
                    blocked_at: r
                        .get::<Option<DateTime<Utc>>, _>("blocked_at")
                        .unwrap_or_else(Utc::now),
                })
            } else {
                Ok(PackageStatus::Available)
            }
        }
    }
}

pub(super) async fn set_status_impl(
    pool: &PgPool,
    pkg: &PackageId,
    status: PackageStatus,
) -> Result<(), CoreError> {
    let (status_str, reason, blocked_by, blocked_at): (
        &str,
        Option<String>,
        Option<String>,
        Option<DateTime<Utc>>,
    ) = match &status {
        PackageStatus::Available => ("available", None, None, None),
        PackageStatus::Blocked {
            reason,
            blocked_by,
            blocked_at,
        } => (
            "blocked",
            Some(reason.clone()),
            Some(blocked_by.clone()),
            Some(*blocked_at),
        ),
    };

    sqlx::query(
        r#"
        INSERT INTO package_statuses
            (id, registry, package_name, package_version, package_artifact,
             status, block_reason, blocked_by, blocked_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
        ON CONFLICT (registry, package_name, package_version, COALESCE(package_artifact, ''))
        DO UPDATE SET
            status = EXCLUDED.status,
            block_reason = EXCLUDED.block_reason,
            blocked_by = EXCLUDED.blocked_by,
            blocked_at = EXCLUDED.blocked_at,
            updated_at = NOW()
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(&pkg.registry)
    .bind(&pkg.name)
    .bind(&pkg.version)
    .bind(&pkg.artifact)
    .bind(status_str)
    .bind(reason)
    .bind(blocked_by)
    .bind(blocked_at)
    .execute(pool)
    .await
    .db_err()?;

    Ok(())
}

pub(super) async fn delete_package_impl(pool: &PgPool, pkg: &PackageId) -> Result<bool, CoreError> {
    let result = sqlx::query(
        "DELETE FROM package_statuses \
         WHERE registry = $1 AND package_name = $2 AND package_version = $3 \
         AND (package_artifact IS NOT DISTINCT FROM $4)",
    )
    .bind(&pkg.registry)
    .bind(&pkg.name)
    .bind(&pkg.version)
    .bind(&pkg.artifact)
    .execute(pool)
    .await
    .db_err()?;
    Ok(result.rows_affected() > 0)
}

pub(super) async fn list_packages_impl(
    pool: &PgPool,
    filter: PackageFilter,
) -> Result<Vec<PackageSummary>, CoreError> {
    // `ps` is filtered, ordered, and paginated in the `page` CTE *before* the
    // LATERAL joins run, so the correlated access_events subqueries only ever
    // execute against the `limit` rows actually returned — not every row
    // matching the WHERE clause. Safe because ORDER BY only references `page`
    // (i.e. `ps`) columns, never the joined access_events aggregates.
    let rows = sqlx::query(
        r#"
        WITH page AS (
            SELECT
                ps.id,
                ps.registry,
                ps.package_name,
                ps.package_version,
                ps.package_artifact,
                ps.status,
                ps.block_reason,
                ps.blocked_by,
                ps.blocked_at
            FROM package_statuses ps
            WHERE ($1::text IS NULL OR ps.registry = $1)
              AND ($2::text IS NULL OR ps.package_name ILIKE '%' || $2 || '%')
              AND ($3::boolean = false OR ps.status = 'blocked')
              AND ($6::text IS NULL OR ps.package_name = $6)
              AND ($7::text[] IS NULL OR ps.registry = ANY($7::text[]))
            ORDER BY ps.registry, ps.package_name, ps.package_version
            LIMIT $4 OFFSET $5
        )
        SELECT
            page.id,
            page.registry,
            page.package_name,
            page.package_version,
            page.package_artifact,
            page.status,
            page.block_reason,
            page.blocked_by,
            page.blocked_at,
            COALESCE(ae_counts.access_count, 0) AS access_count,
            ae_counts.last_accessed,
            ae_user.last_accessed_by
        FROM page
        LEFT JOIN LATERAL (
            SELECT COUNT(*) AS access_count, MAX(ae.created_at) AS last_accessed
            FROM access_events ae
            WHERE ae.registry       = page.registry
              AND ae.package_name   = page.package_name
              AND ae.package_version = page.package_version
        ) ae_counts ON true
        LEFT JOIN LATERAL (
            SELECT ae2.user_id AS last_accessed_by
            FROM access_events ae2
            WHERE ae2.registry       = page.registry
              AND ae2.package_name   = page.package_name
              AND ae2.package_version = page.package_version
              AND ae2.outcome = 'allowed'
            ORDER BY ae2.created_at DESC
            LIMIT 1
        ) ae_user ON true
        ORDER BY page.registry, page.package_name, page.package_version
        "#,
    )
    .bind(&filter.registry)
    .bind(&filter.name_contains)
    .bind(filter.blocked_only)
    .bind(filter.limit as i64)
    .bind(filter.offset as i64)
    .bind(&filter.name_exact)
    .bind(prepare_registries_param(&filter.registries))
    .fetch_all(pool)
    .await
    .db_err()?;

    Ok(rows.into_iter().map(map_package_summary).collect())
}

pub(super) async fn count_packages_impl(
    pool: &PgPool,
    filter: PackageFilter,
) -> Result<u64, CoreError> {
    let row = sqlx::query(
        r#"
        SELECT COUNT(*) AS total
        FROM package_statuses ps
        WHERE ($1::text IS NULL OR ps.registry = $1)
          AND ($2::text IS NULL OR ps.package_name ILIKE '%' || $2 || '%')
          AND ($3::boolean = false OR ps.status = 'blocked')
          AND ($4::text IS NULL OR ps.package_name = $4)
          AND ($5::text[] IS NULL OR ps.registry = ANY($5::text[]))
        "#,
    )
    .bind(&filter.registry)
    .bind(&filter.name_contains)
    .bind(filter.blocked_only)
    .bind(&filter.name_exact)
    .bind(if filter.registries.is_empty() {
        None
    } else {
        Some(filter.registries)
    })
    .fetch_one(pool)
    .await
    .db_err()?;

    let count: i64 = row.try_get("total").unwrap_or(0);
    Ok(count as u64)
}
