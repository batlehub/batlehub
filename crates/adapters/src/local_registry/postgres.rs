use std::time::Duration;

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use batlehub_core::{
    entities::{CompactionReport, PublishedPackage, Tombstone},
    error::CoreError,
    ports::LocalRegistryBackend,
};

/// `SELECT`-and-`FROM` for a [`Tombstone`], with the caller's `WHERE` clause
/// appended. Shared by the point lookup and the listing so the two column lists
/// cannot drift out of step with [`tombstone_from_row`].
///
/// A macro over `concat!` rather than a `&str` const because sqlx only accepts
/// SQL that is a literal at compile time — which is the point: the `WHERE`
/// fragment a caller passes is a literal too, so nothing dynamic can reach the
/// query text.
macro_rules! tombstone_query {
    ($where:literal) => {
        concat!(
            "SELECT registry, name, version, deleted_at, deleted_by, \
             detail_compacted_at, published_at, published_by, checksum \
             FROM local_packages WHERE ",
            $where
        )
    };
}

/// Read a tombstone from a row selected with [`TOMBSTONE_COLUMNS`].
///
/// `checksum` reads as `Option` because compaction nulls it, and a compacted
/// tombstone is exactly the row this has to keep returning.
fn tombstone_from_row(r: &sqlx::postgres::PgRow) -> Tombstone {
    Tombstone {
        registry: r.get("registry"),
        name: r.get("name"),
        version: r.get("version"),
        deleted_at: r.get("deleted_at"),
        deleted_by: r.get("deleted_by"),
        detail_compacted_at: r.get("detail_compacted_at"),
        published_at: r.get("published_at"),
        published_by: r.get("published_by"),
        checksum: r.get("checksum"),
    }
}

pub struct PostgresLocalRegistry {
    pool: PgPool,
}

impl PostgresLocalRegistry {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl LocalRegistryBackend for PostgresLocalRegistry {
    /// Insert the row with `status = 'pending'` so the version is reserved but
    /// invisible to readers until `commit_publish` promotes it.
    ///
    /// Any stale pending row from a previous crashed publish is removed first so
    /// the caller can retry without hitting the unique constraint. A *tombstoned*
    /// row is not stale and is never removed: it refuses the publish outright.
    async fn publish(&self, pkg: PublishedPackage) -> Result<(), CoreError> {
        // A spent coordinate is refused before anything is written. `uq_local_package`
        // would refuse it anyway once the tombstone row is in the way — this exists
        // so the caller is told *why*, rather than being told the version is
        // published when it is deleted.
        if let Some(ts) = self
            .find_tombstone(&pkg.registry, &pkg.name, &pkg.version)
            .await?
        {
            return Err(CoreError::Conflict(ts.burned_coordinate_message()));
        }

        // Remove a stale pending row if one exists (crash recovery for the caller).
        sqlx::query(
            "DELETE FROM local_packages \
             WHERE registry = $1 AND name = $2 AND version = $3 AND status = 'pending'",
        )
        .bind(&pkg.registry)
        .bind(&pkg.name)
        .bind(&pkg.version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;

        sqlx::query(
            "INSERT INTO local_packages \
                (registry, name, version, checksum, yanked, index_metadata, \
                 published_at, published_by, status, signature_bytes, signature_type, visibility, \
                 retention_keep) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending', $9, $10, $11, $12)",
        )
        .bind(&pkg.registry)
        .bind(&pkg.name)
        .bind(&pkg.version)
        .bind(&pkg.checksum)
        .bind(pkg.yanked)
        .bind(&pkg.index_metadata)
        .bind(pkg.published_at)
        .bind(&pkg.published_by)
        .bind(&pkg.signature_bytes)
        .bind(&pkg.signature_type)
        .bind(pkg.visibility.to_string())
        .bind(pkg.retention_keep)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db) = e {
                if db.constraint() == Some("uq_local_package") {
                    return CoreError::Conflict(format!(
                        "{}@{} already published in registry '{}'",
                        pkg.name, pkg.version, pkg.registry
                    ));
                }
            }
            CoreError::Database(e.to_string())
        })?;
        Ok(())
    }

    async fn commit_publish(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE local_packages SET status = 'published' \
             WHERE registry = $1 AND name = $2 AND version = $3",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn yank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE local_packages \
             SET yanked = TRUE, \
                 index_metadata = jsonb_set(index_metadata, '{yanked}', 'true') \
             WHERE registry = $1 AND name = $2 AND version = $3 AND status = 'published'",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn unyank(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE local_packages \
             SET yanked = FALSE, \
                 index_metadata = jsonb_set(index_metadata, '{yanked}', 'false') \
             WHERE registry = $1 AND name = $2 AND version = $3 AND status = 'published'",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn deprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        message: Option<&str>,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE local_packages \
             SET deprecated = TRUE, \
                 deprecation_message = $4, \
                 index_metadata = jsonb_set(index_metadata, '{deprecated}', \
                     to_jsonb(COALESCE($4::text, 'true'))) \
             WHERE registry = $1 AND name = $2 AND version = $3 AND status = 'published'",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .bind(message)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn undeprecate(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE local_packages \
             SET deprecated = FALSE, \
                 deprecation_message = NULL, \
                 index_metadata = index_metadata - 'deprecated' \
             WHERE registry = $1 AND name = $2 AND version = $3 AND status = 'published'",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn unlist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE local_packages SET unlisted = TRUE \
             WHERE registry = $1 AND name = $2 AND version = $3 AND status = 'published'",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    async fn relist(&self, registry: &str, name: &str, version: &str) -> Result<(), CoreError> {
        sqlx::query(
            "UPDATE local_packages SET unlisted = FALSE \
             WHERE registry = $1 AND name = $2 AND version = $3 AND status = 'published'",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    /// `WHERE … status = 'published'` makes this a no-op for a tombstone: a
    /// coordinate that is already spent cannot be protected from a reclamation
    /// that can never reach it.
    async fn set_channel(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        channel: &str,
    ) -> Result<bool, CoreError> {
        // `jsonb_set` on the one key, rather than reading the document and
        // writing it back: a read-modify-write would lose a concurrent yank's
        // `index_metadata.yanked`, and this is the field the *other* mutators
        // share. `IS DISTINCT FROM` makes a no-op move report `false` rather than
        // touching the row.
        let result = sqlx::query(
            "UPDATE local_packages \
             SET index_metadata = jsonb_set(\
                 COALESCE(index_metadata, '{}'::jsonb), '{channel}', to_jsonb($4::text), true) \
             WHERE registry = $1 AND name = $2 AND version = $3 \
               AND status = 'published' \
               AND COALESCE(index_metadata->>'channel', '') IS DISTINCT FROM $4::text",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .bind(channel)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_vsix_signature_provided(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        provided: bool,
    ) -> Result<bool, CoreError> {
        let result = if provided {
            sqlx::query(
                "UPDATE local_packages \
                 SET index_metadata = jsonb_set(COALESCE(index_metadata, '{}'::jsonb), \
                                                '{vsixSignature}', '\"provided\"'::jsonb) \
                 WHERE registry = $1 AND name = $2 AND version = $3 \
                   AND status = 'published' AND deleted_at IS NULL \
                   AND COALESCE(index_metadata->>'vsixSignature', '') <> 'provided'",
            )
        } else {
            sqlx::query(
                "UPDATE local_packages SET index_metadata = index_metadata - 'vsixSignature' \
                 WHERE registry = $1 AND name = $2 AND version = $3 \
                   AND status = 'published' AND deleted_at IS NULL \
                   AND index_metadata ? 'vsixSignature'",
            )
        }
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_retention_keep(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        keep: bool,
    ) -> Result<bool, CoreError> {
        let result = sqlx::query(
            "UPDATE local_packages SET retention_keep = $4 \
             WHERE registry = $1 AND name = $2 AND version = $3 \
               AND status = 'published' AND deleted_at IS NULL \
               AND retention_keep IS DISTINCT FROM $4",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .bind(keep)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn get_versions(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Vec<PublishedPackage>, CoreError> {
        let rows = sqlx::query(
            "SELECT registry, name, version, checksum, yanked, deprecated, \
                    deprecation_message, unlisted, index_metadata, \
                    published_at, published_by, signature_bytes, signature_type, visibility, \
                    retention_keep \
             FROM local_packages \
             WHERE registry = $1 AND name = $2 AND status = 'published' \
               AND deleted_at IS NULL \
             ORDER BY published_at ASC",
        )
        .bind(registry)
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;

        rows.into_iter()
            .map(|r| {
                let vis = r
                    .get::<String, _>("visibility")
                    .parse()
                    .map_err(|e| CoreError::Database(format!("invalid visibility in db: {e}")))?;
                Ok(PublishedPackage {
                    registry: r.get("registry"),
                    name: r.get("name"),
                    version: r.get("version"),
                    checksum: r.get("checksum"),
                    yanked: r.get("yanked"),
                    deprecated: r.get("deprecated"),
                    deprecation_message: r.get("deprecation_message"),
                    unlisted: r.get("unlisted"),
                    index_metadata: r.get("index_metadata"),
                    published_at: r.get("published_at"),
                    published_by: r.get("published_by"),
                    signature_bytes: r.get("signature_bytes"),
                    signature_type: r.get("signature_type"),
                    visibility: vis,
                    retention_keep: r.get("retention_keep"),
                })
            })
            .collect()
    }

    async fn exists(&self, registry: &str, name: &str) -> Result<bool, CoreError> {
        let row = sqlx::query(
            "SELECT 1 FROM local_packages \
             WHERE registry = $1 AND name = $2 AND status = 'published' \
               AND deleted_at IS NULL \
             LIMIT 1",
        )
        .bind(registry)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(row.is_some())
    }

    /// `deleted_at IS NULL` is on the rollback primitive too: this is the one
    /// path in the tree that still issues a `DELETE` against `local_packages`,
    /// and a coordinate RFC 0016 §4.4 calls permanently spent must not be freed
    /// by a caller reaching for the wrong cleanup.
    async fn remove_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<(), CoreError> {
        sqlx::query(
            "DELETE FROM local_packages \
             WHERE registry = $1 AND name = $2 AND version = $3 \
               AND deleted_at IS NULL",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(())
    }

    /// `status = 'deleted'` is set alongside `deleted_at`, so the six mutators
    /// and three readers above that already filter `status = 'published'` exclude
    /// the tombstone whether or not their own `deleted_at IS NULL` predicate is
    /// there. Two guards for one invariant, because a listing that serves a
    /// coordinate whose bytes are gone is a build that fails at download.
    ///
    /// `WHERE status = 'published'` also makes this idempotent for free: a second
    /// delete matches nothing and returns `false`, leaving the original
    /// `deleted_at` — the timestamp compaction ages against — untouched.
    async fn tombstone_version(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        deleted_by: Option<&str>,
    ) -> Result<bool, CoreError> {
        let result = sqlx::query(
            "UPDATE local_packages \
             SET status = 'deleted', deleted_at = NOW(), deleted_by = $4 \
             WHERE registry = $1 AND name = $2 AND version = $3 \
               AND status = 'published' AND deleted_at IS NULL",
        )
        .bind(registry)
        .bind(name)
        .bind(version)
        .bind(deleted_by)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(result.rows_affected() > 0)
    }

    async fn find_tombstone(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<Tombstone>, CoreError> {
        let row = sqlx::query(tombstone_query!(
            "registry = $1 AND name = $2 AND version = $3 AND deleted_at IS NOT NULL"
        ))
        .bind(registry)
        .bind(name)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(row.as_ref().map(tombstone_from_row))
    }

    async fn list_tombstones(
        &self,
        registry: &str,
        name: Option<&str>,
    ) -> Result<Vec<Tombstone>, CoreError> {
        // `$2 IS NULL OR name = $2` rather than two query strings: the filter is
        // optional at the call site and the plan is the same either way.
        let rows = sqlx::query(tombstone_query!(
            "registry = $1 AND deleted_at IS NOT NULL \
             AND ($2::text IS NULL OR name = $2) \
             ORDER BY deleted_at DESC, name ASC, version ASC"
        ))
        .bind(registry)
        .bind(name)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(rows.iter().map(tombstone_from_row).collect())
    }

    /// Strip the detail, keep the claim (RFC 0016 §4.5).
    ///
    /// `index_metadata` is set to `'{}'` rather than nulled — it is `NOT NULL`
    /// and three bytes is not what accumulates. Everything else the RFC calls
    /// detail is nulled. `deleted_at`, `deleted_by`, `published_at` and the
    /// coordinate stay: they *are* the claim and its provenance.
    ///
    /// `detail_compacted_at IS NULL` in the predicate is what makes a second run
    /// a no-op instead of a re-stamp, so `skipped` means the same thing on every
    /// run and a dry run's numbers survive to the live one.
    async fn compact_tombstone_detail(
        &self,
        registry: &str,
        older_than: Duration,
        dry_run: bool,
    ) -> Result<CompactionReport, CoreError> {
        let secs = older_than.as_secs() as i64;
        // The `WHERE` clause is repeated verbatim in the two arms rather than
        // shared: sqlx only takes literal SQL, so factoring it out would mean a
        // macro, and one predicate does not earn one. The two must agree, and
        // `pg_tombstones.rs` asserts that a dry run and the live run that follows
        // it report the same coordinates.
        //
        // The live path writes and reports in one statement rather than
        // select-then-update: `NOW()` moves between two statements, so a
        // tombstone that ages past the window in that gap would be stripped
        // without appearing in the report. `RETURNING` cannot disagree with what
        // it wrote.
        let rows = if dry_run {
            sqlx::query(
                "SELECT name, version FROM local_packages \
                 WHERE registry = $1 AND deleted_at IS NOT NULL \
                   AND detail_compacted_at IS NULL \
                   AND deleted_at < NOW() - ($2 || ' seconds')::INTERVAL \
                 ORDER BY name ASC, version ASC",
            )
        } else {
            sqlx::query(
                "UPDATE local_packages \
                 SET index_metadata = '{}'::jsonb, \
                     checksum = NULL, \
                     published_by = NULL, \
                     signature_bytes = NULL, \
                     signature_type = NULL, \
                     deprecation_message = NULL, \
                     detail_compacted_at = NOW() \
                 WHERE registry = $1 AND deleted_at IS NOT NULL \
                   AND detail_compacted_at IS NULL \
                   AND deleted_at < NOW() - ($2 || ' seconds')::INTERVAL \
                 RETURNING name, version",
            )
        }
        .bind(registry)
        .bind(secs)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;

        let total: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM local_packages \
             WHERE registry = $1 AND deleted_at IS NOT NULL",
        )
        .bind(registry)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?
        .get("n");

        let mut coordinates: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{}@{}",
                    r.get::<String, _>("name"),
                    r.get::<String, _>("version")
                )
            })
            .collect();
        coordinates.sort();
        let compacted = coordinates.len() as u64;

        Ok(CompactionReport {
            compacted,
            skipped: (total as u64).saturating_sub(compacted),
            dry_run,
            coordinates,
        })
    }

    async fn cleanup_pending(&self, older_than: Duration) -> Result<u64, CoreError> {
        let secs = older_than.as_secs() as i64;
        let result = sqlx::query(
            "DELETE FROM local_packages \
             WHERE status = 'pending' \
               AND published_at < NOW() - ($1 || ' seconds')::INTERVAL",
        )
        .bind(secs)
        .execute(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    async fn list_package_names(&self, registry: &str) -> Result<Vec<String>, CoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT name FROM local_packages \
             WHERE registry = $1 AND status = 'published' \
               AND deleted_at IS NULL \
             ORDER BY name ASC",
        )
        .bind(registry)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CoreError::Database(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("name"))
            .collect())
    }
}
