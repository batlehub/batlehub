//! Postgres [`PolicyRepository`] — RFC 0015 §6.3's `policy` table.
//!
//! Checked against `InMemoryPolicyRepository`, which is the behavioural
//! reference. The two must agree on the edges as well as the middle, and
//! agreement alone is not evidence: survey finding 2 shipped because an empty
//! list meant "everything" in four repository implementations "that all agreed
//! with each other". `crates/adapters/tests/pg_policy.rs` asserts the same
//! properties against both.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use batlehub_core::entities::{QuotaRules, RuleOverride, VersioningRules, Visibility};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{version_node_key, NodeKind, PolicyRepository, StoredPolicy};

use crate::db::DbResultExt;

pub struct PgPolicyRepository {
    pool: PgPool,
}

impl PgPolicyRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The JSON shape `versioning` is stored in.
///
/// A named struct rather than `serde_json::Value` handling, so a field added to
/// [`VersioningRules`] and forgotten here is a compile error rather than a
/// policy that silently stops round-tripping. `#[serde(default)]` throughout so
/// a row written by an older build reads back as its defaults rather than as an
/// error — a policy that becomes unreadable on upgrade would take a registry's
/// constraints off without saying so.
#[derive(serde::Serialize, serde::Deserialize)]
struct VersioningJson {
    #[serde(default)]
    enforce_semver: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    allow_prerelease: bool,
    #[serde(default)]
    version_pattern: Option<String>,
    #[serde(default)]
    immutable: batlehub_core::entities::Immutable,
    #[serde(default)]
    monotonic: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct QuotaJson {
    #[serde(default)]
    max_bytes_per_user: Option<u64>,
    #[serde(default)]
    max_packages_per_user: Option<u32>,
    #[serde(default)]
    warn_threshold_pct: Option<u8>,
    #[serde(default)]
    block: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RuleOverrideJson {
    gate: String,
    settings: serde_json::Value,
}

/// One row → a [`StoredPolicy`].
///
/// A row this build cannot parse is an **error**, not a skip, for the reason
/// `row_to_grant` gives: skipping would silently drop a policy an operator wrote
/// and can see in the table. The failure mode is worse here than for a grant,
/// because a dropped *constraint* fails open — a `visibility` that would not
/// parse becomes a package with no audience restriction at all.
fn row_to_policy(row: &sqlx::postgres::PgRow) -> Result<StoredPolicy, CoreError> {
    let node_kind: String = row.try_get("node_kind").db_err()?;
    let visibility: Option<String> = row.try_get("visibility").db_err()?;
    let prerelease: Option<String> = row.try_get("prerelease_visibility").db_err()?;
    let versioning: Option<serde_json::Value> = row.try_get("versioning").db_err()?;
    let quota: Option<serde_json::Value> = row.try_get("quota").db_err()?;
    let rules: serde_json::Value = row.try_get("rules").db_err()?;

    let parse_visibility =
        |raw: Option<String>, field: &str| -> Result<Option<Visibility>, CoreError> {
            raw.map(|v| {
                v.parse::<Visibility>()
                    .map_err(|e| CoreError::InvalidInput(format!("stored policy {field}: {e}")))
            })
            .transpose()
        };

    Ok(StoredPolicy {
        registry: row.try_get("registry").db_err()?,
        node_kind: node_kind.parse()?,
        node_key: row.try_get("node_key").db_err()?,
        visibility: parse_visibility(visibility, "visibility")?,
        prerelease_visibility: parse_visibility(prerelease, "prerelease_visibility")?,
        versioning: versioning
            .map(|v| {
                serde_json::from_value::<VersioningJson>(v)
                    .map(|j| VersioningRules {
                        enforce_semver: j.enforce_semver,
                        allow_prerelease: j.allow_prerelease,
                        version_pattern: j.version_pattern,
                        immutable: j.immutable,
                        monotonic: j.monotonic,
                        dry_run: j.dry_run,
                    })
                    .map_err(|e| CoreError::InvalidInput(format!("stored policy versioning: {e}")))
            })
            .transpose()?,
        quota: quota
            .map(|q| {
                serde_json::from_value::<QuotaJson>(q)
                    .map(|j| QuotaRules {
                        max_bytes_per_user: j.max_bytes_per_user,
                        max_packages_per_user: j.max_packages_per_user,
                        warn_threshold_pct: j.warn_threshold_pct,
                        block: j.block,
                    })
                    .map_err(|e| CoreError::InvalidInput(format!("stored policy quota: {e}")))
            })
            .transpose()?,
        rules: serde_json::from_value::<Vec<RuleOverrideJson>>(rules)
            .map_err(|e| CoreError::InvalidInput(format!("stored policy rules: {e}")))?
            .into_iter()
            .map(|r| RuleOverride {
                gate: r.gate,
                settings: r.settings,
            })
            .collect(),
        set_by: row.try_get("set_by").db_err()?,
    })
}

#[async_trait]
impl PolicyRepository for PgPolicyRepository {
    async fn policy_for(
        &self,
        registry: &str,
        package: &str,
        version: Option<&str>,
    ) -> Result<Vec<StoredPolicy>, CoreError> {
        // An empty package names no node. The `node_key = ANY` below is simply
        // false for it and the answer is the same — the guard is here because
        // the *next* edit is the one that makes it vacuous, and finding 2 is
        // what a vacuous predicate costs.
        if package.is_empty() {
            return Ok(Vec::new());
        }
        // Both tiers in one round trip, as `grants_for` does and for the same
        // reason: §11.7 budgets 2 ms p99 for a single-coordinate resolution, and
        // two queries spend it twice.
        //
        // `= ANY($2)` rather than a LIKE: the version key is exact, and a prefix
        // match would take `@acme/billing-internal@1.0.0` for `@acme/billing`'s
        // — RFC 0011-bis §4.2's segment-boundary bug, on the read path.
        let mut keys = vec![package.to_owned()];
        if let Some(v) = version {
            keys.push(version_node_key(package, v));
        }
        // `ORDER BY node_kind` puts 'package' before 'version' alphabetically,
        // which is also **deepest last** — what the port promises and what
        // composition depends on, since `PolicyPath::resolve` takes the last
        // declaration. The coincidence is load-bearing enough to be stated: if a
        // third tier is ever stored here, this ordering stops being free.
        // The column list is written out at each call site rather than shared
        // in a constant: `sqlx::query` takes a `&'static str` on purpose, and
        // the lint that enforces it is the one thing standing between a column
        // list and an injection. Two literals beat one `format!`.
        let rows = sqlx::query(
            "SELECT registry, node_kind, node_key, visibility, prerelease_visibility, \
                    versioning, quota, rules, set_by \
             FROM policy \
             WHERE registry = $1 AND node_key = ANY($2) \
             ORDER BY node_kind",
        )
        .bind(registry)
        .bind(&keys)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        rows.iter().map(row_to_policy).collect()
    }

    async fn put_policy(&self, policy: StoredPolicy) -> Result<(), CoreError> {
        if let Some(reason) = policy.validate() {
            return Err(CoreError::InvalidInput(reason));
        }
        if policy.is_empty() {
            // A row declaring nothing is not a policy — see the in-memory
            // adapter, which is the reference for this. Writing one would make
            // "has a policy node" and "has a policy" different questions.
            return self
                .delete_policy(&policy.registry, policy.node_kind, &policy.node_key)
                .await;
        }

        let versioning = policy.versioning.as_ref().map(|v| {
            serde_json::json!(VersioningJson {
                enforce_semver: v.enforce_semver,
                dry_run: v.dry_run,
                allow_prerelease: v.allow_prerelease,
                version_pattern: v.version_pattern.clone(),
                immutable: v.immutable,
                monotonic: v.monotonic,
            })
        });
        let quota = policy.quota.as_ref().map(|q| {
            serde_json::json!(QuotaJson {
                max_bytes_per_user: q.max_bytes_per_user,
                max_packages_per_user: q.max_packages_per_user,
                warn_threshold_pct: q.warn_threshold_pct,
                block: q.block,
            })
        });
        let rules = serde_json::json!(policy
            .rules
            .iter()
            .map(|r| RuleOverrideJson {
                gate: r.gate.clone(),
                settings: r.settings.clone(),
            })
            .collect::<Vec<_>>());

        sqlx::query(
            "INSERT INTO policy (registry, node_kind, node_key, visibility, \
                                 prerelease_visibility, versioning, quota, rules, set_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (registry, node_kind, node_key) \
             DO UPDATE SET visibility = EXCLUDED.visibility, \
                           prerelease_visibility = EXCLUDED.prerelease_visibility, \
                           versioning = EXCLUDED.versioning, \
                           quota = EXCLUDED.quota, \
                           rules = EXCLUDED.rules, \
                           set_by = EXCLUDED.set_by, \
                           set_at = NOW()",
        )
        .bind(&policy.registry)
        .bind(policy.node_kind.as_str())
        .bind(&policy.node_key)
        .bind(policy.visibility.map(|v| v.as_str()))
        .bind(policy.prerelease_visibility.map(|v| v.as_str()))
        .bind(&versioning)
        .bind(&quota)
        .bind(&rules)
        .bind(&policy.set_by)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn delete_policy(
        &self,
        registry: &str,
        node_kind: NodeKind,
        node_key: &str,
    ) -> Result<(), CoreError> {
        sqlx::query("DELETE FROM policy WHERE registry = $1 AND node_kind = $2 AND node_key = $3")
            .bind(registry)
            .bind(node_kind.as_str())
            .bind(node_key)
            .execute(&self.pool)
            .await
            .db_err()?;
        Ok(())
    }

    async fn policy_on_node(
        &self,
        registry: &str,
        node_kind: NodeKind,
        node_key: &str,
    ) -> Result<Option<StoredPolicy>, CoreError> {
        let row = sqlx::query(
            "SELECT registry, node_kind, node_key, visibility, prerelease_visibility, \
                    versioning, quota, rules, set_by \
             FROM policy \
             WHERE registry = $1 AND node_kind = $2 AND node_key = $3",
        )
        .bind(registry)
        .bind(node_kind.as_str())
        .bind(node_key)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        row.as_ref().map(row_to_policy).transpose()
    }

    async fn exemptions_in_registry(&self, registry: &str) -> Result<Vec<StoredPolicy>, CoreError> {
        // `@>` against a one-element array: "the `rules` array contains an object
        // whose `settings` carry `exempt: true`" — a rule is stored as
        // `{"gate", "settings": {"exempt", …}}`, so a top-level `exempt` matched
        // nothing and every exemption was invisible. The GIN-friendly
        // containment operator rather than an unrolled `jsonb_array_elements`,
        // and the shape matters — a rule override that merely *mentions* the
        // gate is not an exemption, so the predicate is about the flag.
        let rows = sqlx::query(
            "SELECT registry, node_kind, node_key, visibility, prerelease_visibility, \
                    versioning, quota, rules, set_by \
             FROM policy \
             WHERE registry = $1 \
               AND node_kind = 'version' \
               AND rules @> '[{\"settings\": {\"exempt\": true}}]'::jsonb \
             ORDER BY node_key",
        )
        .bind(registry)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        rows.iter().map(row_to_policy).collect()
    }

    async fn delete_package_policy(&self, registry: &str, package: &str) -> Result<(), CoreError> {
        if package.is_empty() {
            return Ok(());
        }
        // `LIKE $3 || '@%'` with the package escaped would still be a pattern
        // match, so the version tier is matched by an explicit prefix comparison
        // on `package@` instead: a bare prefix would take
        // `@acme/billing-internal`'s rows out with `@acme/billing`'s, which is
        // RFC 0011-bis §4.2's segment-boundary bug on the delete path, where it
        // destroys rather than discloses. `starts_with` in SQL is
        // `LEFT(node_key, LENGTH($3)) = $3`, which is literal by construction —
        // the same reasoning `LOCAL_VISIBILITY_PREDICATE` gives for using
        // `SUBSTRING` rather than `LIKE`.
        let version_prefix = format!("{package}@");
        sqlx::query(
            "DELETE FROM policy \
             WHERE registry = $1 \
               AND ( (node_kind = 'package' AND node_key = $2) \
                  OR (node_kind = 'version' \
                      AND LEFT(node_key, LENGTH($3)) = $3) )",
        )
        .bind(registry)
        .bind(package)
        .bind(&version_prefix)
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }
}
