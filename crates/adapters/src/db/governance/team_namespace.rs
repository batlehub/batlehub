use crate::db::DbResultExt;
use std::str::FromStr;

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use batlehub_core::{
    entities::{NamespacePackage, TeamNamespace, Visibility},
    error::CoreError,
    ports::TeamNamespacePort,
};

pub struct PgTeamNamespaceStore {
    pool: PgPool,
}

/// The claim's separator column, as a `char`.
///
/// The column is `TEXT` with a `length = 1` check, so this cannot silently
/// truncate a real value — but a row written before migration 045, or by hand,
/// falls back to `/`, which is what every claim matched on before the column
/// existed.
fn separator_of(r: &sqlx::postgres::PgRow) -> char {
    r.try_get::<String, _>("separator")
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or('/')
}

impl PgTeamNamespaceStore {
    async fn get_visibility_uncached(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Visibility, CoreError> {
        let row = sqlx::query(
            "SELECT visibility FROM local_packages \
             WHERE registry = $1 AND name = $2 AND status = 'published' \
             LIMIT 1",
        )
        .bind(registry)
        .bind(package)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        match row {
            None => Ok(Visibility::Public),
            Some(r) => {
                let s: String = r.get("visibility");
                Visibility::from_str(&s)
                    .map_err(|e| CoreError::Database(format!("invalid visibility in db: {e}")))
            }
        }
    }

    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TeamNamespacePort for PgTeamNamespaceStore {
    async fn find_namespace(
        &self,
        registry: &str,
        package: &str,
    ) -> Result<Option<TeamNamespace>, CoreError> {
        // Longest-prefix match: a claim with prefix P covers package N when
        // N == P **or** N starts with P + the claim's own separator (§4.1).
        //
        // `tn.separator`, not `'/'`: the character is the ecosystem's, and every
        // matcher here used to assume npm's. `TeamNamespace::covers` is the same
        // rule in Rust and `LOCAL_VISIBILITY_PREDICATE` is the third copy;
        // `pg_team_namespace_separator.rs` runs all three over one table of cases
        // because §6.3 requires them to agree character for character.
        let row = sqlx::query(
            "SELECT prefix, group_id, claimed_by, separator FROM team_namespaces \
             WHERE registry = $1 \
               AND ($2 = prefix \
                    OR (LENGTH($2) > LENGTH(prefix) \
                        AND SUBSTRING($2, 1, LENGTH(prefix) + 1) = prefix || separator)) \
             ORDER BY LENGTH(prefix) DESC \
             LIMIT 1",
        )
        .bind(registry)
        .bind(package)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        Ok(row.map(|r| TeamNamespace {
            registry: registry.to_owned(),
            prefix: r.get("prefix"),
            group_id: r.get("group_id"),
            claimed_by: r.get("claimed_by"),
            separator: separator_of(&r),
        }))
    }

    async fn list_namespaces(&self, registry: &str) -> Result<Vec<TeamNamespace>, CoreError> {
        let rows = sqlx::query(
            "SELECT prefix, group_id, claimed_by FROM team_namespaces \
             WHERE registry = $1 \
             ORDER BY prefix ASC",
        )
        .bind(registry)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        Ok(rows
            .into_iter()
            .map(|r| TeamNamespace {
                registry: registry.to_owned(),
                prefix: r.get("prefix"),
                group_id: r.get("group_id"),
                claimed_by: r.get("claimed_by"),
                separator: separator_of(&r),
            })
            .collect())
    }

    async fn claim_namespace(&self, ns: TeamNamespace) -> Result<(), CoreError> {
        sqlx::query(
            // The separator is written with the claim (§4.1). Leaving it to the
            // column default made every new claim match on `/` whatever its
            // ecosystem — the bug this column exists to fix, surviving into the
            // write path — and `pg_namespace_separator.rs` caught it on the
            // round-trip assertion rather than on a match, which is the sharper
            // place for it: the matcher agreed with a stored value that was
            // simply the wrong one.
            "INSERT INTO team_namespaces (registry, prefix, group_id, claimed_by, separator) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&ns.registry)
        .bind(&ns.prefix)
        .bind(&ns.group_id)
        .bind(&ns.claimed_by)
        .bind(ns.separator.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db) = e {
                if db.constraint() == Some("uq_team_namespace") {
                    return CoreError::Conflict(format!(
                        "namespace '{}' in registry '{}' is already claimed",
                        ns.prefix, ns.registry
                    ));
                }
            }
            CoreError::Database(e.to_string())
        })?;
        Ok(())
    }

    async fn release_namespace(&self, registry: &str, prefix: &str) -> Result<(), CoreError> {
        sqlx::query("DELETE FROM team_namespaces WHERE registry = $1 AND prefix = $2")
            .bind(registry)
            .bind(prefix)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(())
    }

    async fn set_visibility(
        &self,
        registry: &str,
        package: &str,
        vis: Visibility,
    ) -> Result<(), CoreError> {
        let _clear = crate::db::read_cache::CACHES.visibility.clear_after();
        let result = sqlx::query(
            "UPDATE local_packages SET visibility = $3 \
             WHERE registry = $1 AND name = $2",
        )
        .bind(registry)
        .bind(package)
        .bind(vis.to_string())
        .execute(&self.pool)
        .await
        .db_err()?;

        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound(format!(
                "package '{}' not found in registry '{}'",
                package, registry
            )));
        }
        Ok(())
    }

    async fn get_visibility(&self, registry: &str, package: &str) -> Result<Visibility, CoreError> {
        crate::db::read_cache::CACHES
            .visibility
            .get_or_try(
                (registry.to_owned(), package.to_owned()),
                self.get_visibility_uncached(registry, package),
            )
            .await
    }

    async fn prefetch_visibility(&self, packages: &[(String, String)]) {
        let c = &crate::db::read_cache::CACHES.visibility;
        let missing = c.missing(packages);
        if missing.is_empty() {
            return;
        }
        let ticket = c.ticket();
        let (regs, names): (Vec<&str>, Vec<&str>) = missing
            .iter()
            .map(|(r, n)| (r.as_str(), n.as_str()))
            .unzip();
        let rows = sqlx::query(
            "SELECT DISTINCT ON (registry, name) registry, name, visibility FROM local_packages \
             WHERE (registry, name) IN (SELECT * FROM UNNEST($1::text[], $2::text[])) \
               AND status = 'published'",
        )
        .bind(&regs)
        .bind(&names)
        .fetch_all(&self.pool)
        .await;
        let Ok(rows) = rows else { return };
        let mut found: std::collections::HashMap<(String, String), Visibility> =
            std::collections::HashMap::new();
        for r in rows {
            let Ok(v) = Visibility::from_str(&r.get::<String, _>("visibility")) else {
                // Unparseable: leave it to the single read, which reports it.
                return;
            };
            found.insert((r.get("registry"), r.get("name")), v);
        }
        for k in missing {
            // No published row is `Public`, as `get_visibility` answers.
            let v = found.get(k).copied().unwrap_or(Visibility::Public);
            c.put(ticket, k.clone(), v);
        }
    }

    async fn list_namespaces_for_groups(
        &self,
        groups: &[String],
    ) -> Result<Vec<TeamNamespace>, CoreError> {
        if groups.is_empty() {
            return Ok(vec![]);
        }
        let rows = sqlx::query(
            "SELECT registry, prefix, group_id, claimed_by FROM team_namespaces \
             WHERE group_id = ANY($1) \
             ORDER BY registry, prefix ASC",
        )
        .bind(groups)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        Ok(rows
            .into_iter()
            .map(|r| TeamNamespace {
                registry: r.get("registry"),
                prefix: r.get("prefix"),
                group_id: r.get("group_id"),
                claimed_by: r.get("claimed_by"),
                separator: separator_of(&r),
            })
            .collect())
    }

    async fn list_packages_in_namespace(
        &self,
        registry: &str,
        prefix: &str,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<NamespacePackage>, CoreError> {
        let rows = sqlx::query(
            "SELECT name, version, visibility, published_by, published_at, yanked \
             FROM local_packages \
             WHERE registry = $1 \
               AND status = 'published' \
               AND (name = $2 \
                    OR (LENGTH(name) > LENGTH($2) \
                        AND SUBSTRING(name, 1, LENGTH($2) + 1) = $2 || '/')) \
             ORDER BY name, version \
             LIMIT $3 OFFSET $4",
        )
        .bind(registry)
        .bind(prefix)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        rows.into_iter()
            .map(|r| {
                let vis_str: String = r.get("visibility");
                let vis = Visibility::from_str(&vis_str)
                    .map_err(|e| CoreError::Database(format!("invalid visibility in db: {e}")))?;
                Ok(NamespacePackage {
                    name: r.get("name"),
                    version: r.get("version"),
                    visibility: vis,
                    published_by: r.get("published_by"),
                    published_at: r.get("published_at"),
                    yanked: r.get("yanked"),
                })
            })
            .collect()
    }

    async fn count_packages_in_namespace(
        &self,
        registry: &str,
        prefix: &str,
    ) -> Result<u64, CoreError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS total \
             FROM local_packages \
             WHERE registry = $1 \
               AND status = 'published' \
               AND (name = $2 \
                    OR (LENGTH(name) > LENGTH($2) \
                        AND SUBSTRING(name, 1, LENGTH($2) + 1) = $2 || '/'))",
        )
        .bind(registry)
        .bind(prefix)
        .fetch_one(&self.pool)
        .await
        .db_err()?;

        let count: i64 = row.try_get("total").unwrap_or(0);
        Ok(count as u64)
    }
}
