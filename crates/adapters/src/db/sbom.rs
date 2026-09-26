use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};

use crate::db::DbResultExt;
use batlehub_core::{
    entities::{ArtifactSbom, SbomFormat, SbomSource},
    error::CoreError,
    ports::{SbomFacts, SbomRepository},
};

pub struct PgSbomRepository {
    pool: PgPool,
}

impl PgSbomRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_sbom(r: &sqlx::postgres::PgRow) -> ArtifactSbom {
    let format_str: String = r.get("format");
    let source_str: String = r.get("source");
    ArtifactSbom {
        id: r.get("id"),
        artifact_key: r.get("artifact_key"),
        registry: r.get("registry"),
        package_name: r.get("package_name"),
        version: r.get("version"),
        format: SbomFormat::parse(&format_str).unwrap_or(SbomFormat::Spdx),
        spec_version: r.get("spec_version"),
        document: r.get("document"),
        source: SbomSource::parse(&source_str).unwrap_or(SbomSource::Generated),
        created_at: r.get("created_at"),
        license: r.get("license"),
    }
}

#[async_trait]
impl SbomRepository for PgSbomRepository {
    async fn upsert_sbom(&self, sbom: ArtifactSbom) -> Result<(), CoreError> {
        sqlx::query(
            r#"
            INSERT INTO artifact_sboms
                (id, artifact_key, registry, package_name, version,
                 format, spec_version, document, source, created_at, license)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (artifact_key, format) DO UPDATE
                SET document     = EXCLUDED.document,
                    source       = EXCLUDED.source,
                    spec_version = EXCLUDED.spec_version,
                    created_at   = NOW(),
                    -- A re-record that could not read a licence must not erase
                    -- one an earlier pass did read: the extractor is
                    -- best-effort, and losing a known licence would silently
                    -- move the package into `allow_unknown` territory.
                    license      = COALESCE(EXCLUDED.license, artifact_sboms.license)
            "#,
        )
        .bind(sbom.id)
        .bind(&sbom.artifact_key)
        .bind(&sbom.registry)
        .bind(&sbom.package_name)
        .bind(&sbom.version)
        .bind(sbom.format.as_str())
        .bind(sbom.spec_version.as_str())
        .bind(&sbom.document)
        .bind(sbom.source.as_str())
        .bind(sbom.created_at)
        .bind(sbom.license.as_deref())
        .execute(&self.pool)
        .await
        .db_err()?;
        Ok(())
    }

    async fn get_sbom(
        &self,
        artifact_key: &str,
        format: &SbomFormat,
    ) -> Result<Option<ArtifactSbom>, CoreError> {
        let row = sqlx::query(
            "SELECT id, artifact_key, registry, package_name, version, \
             format, spec_version, document, source, created_at, license \
             FROM artifact_sboms WHERE artifact_key = $1 AND format = $2",
        )
        .bind(artifact_key)
        .bind(format.as_str())
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        Ok(row.as_ref().map(row_to_sbom))
    }

    async fn get_sbom_by_coordinates(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
        format: &SbomFormat,
    ) -> Result<Option<ArtifactSbom>, CoreError> {
        let row = sqlx::query(
            "SELECT id, artifact_key, registry, package_name, version, \
             format, spec_version, document, source, created_at, license \
             FROM artifact_sboms \
             WHERE registry = $1 AND package_name = $2 AND version = $3 AND format = $4 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(registry)
        .bind(package_name)
        .bind(version)
        .bind(format.as_str())
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        Ok(row.as_ref().map(row_to_sbom))
    }

    /// Existence only — no `document` column in the projection.
    ///
    /// The default implementation in the port answers this by fetching both
    /// SBOMs whole, and this runs once per row of a version page: on a
    /// twenty-five row page that is fifty JSONB documents read and discarded to
    /// learn which two buttons to draw.
    async fn sbom_formats_for_coordinate(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
    ) -> Result<Vec<SbomFormat>, CoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT format FROM artifact_sboms \
             WHERE registry = $1 AND package_name = $2 AND version = $3",
        )
        .bind(registry)
        .bind(package_name)
        .bind(version)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        // Ordered by the enum rather than by whatever the DISTINCT returned, so
        // the console draws SPDX before CycloneDX on every row of every page.
        let held: Vec<String> = rows.iter().map(|r| r.get("format")).collect();
        Ok([SbomFormat::Spdx, SbomFormat::CycloneDx]
            .into_iter()
            .filter(|f| held.iter().any(|h| h == f.as_str()))
            .collect())
    }

    /// One statement for the page: the two per-version reads above, over
    /// `version = ANY($3)`, without the `document` column.
    async fn sbom_facts_for_versions(
        &self,
        registry: &str,
        package_name: &str,
        versions: &[String],
    ) -> Result<HashMap<String, SbomFacts>, CoreError> {
        if versions.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT version, format, license FROM artifact_sboms \
             WHERE registry = $1 AND package_name = $2 AND version = ANY($3) \
             ORDER BY created_at DESC",
        )
        .bind(registry)
        .bind(package_name)
        .bind(versions)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        let mut out: HashMap<String, SbomFacts> = HashMap::new();
        for r in &rows {
            let facts = out.entry(r.get("version")).or_default();
            // Newest first, so the first licence seen is the one
            // `get_license_for_coordinate` would answer.
            if facts.license.is_none() {
                facts.license = r.get("license");
            }
            if let Some(f) = SbomFormat::parse(r.get("format")) {
                if !facts.formats.contains(&f) {
                    facts.formats.push(f);
                }
            }
        }
        for facts in out.values_mut() {
            facts
                .formats
                .sort_by_key(|f| matches!(f, SbomFormat::CycloneDx));
        }
        Ok(out)
    }

    async fn get_license_for_coordinate(
        &self,
        registry: &str,
        package_name: &str,
        version: &str,
    ) -> Result<Option<String>, CoreError> {
        // Both formats carry the same licence, so the format is not part of the
        // question; `LIMIT 1` over the newest row is the answer either way.
        let row = sqlx::query(
            "SELECT license FROM artifact_sboms \
             WHERE registry = $1 AND package_name = $2 AND version = $3 \
               AND license IS NOT NULL \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(registry)
        .bind(package_name)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .db_err()?;

        Ok(row.map(|r| r.get("license")))
    }

    async fn list_sboms_for_export(
        &self,
        registry: Option<&str>,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<ArtifactSbom>, CoreError> {
        // Build a dynamic query with optional filters.
        // Bind parameters: registry ($1), from ($2), to ($3), limit ($4), offset ($5)
        // When a filter is None we use IS NULL / unconditional TRUE instead.
        let rows = sqlx::query(
            r#"
            SELECT id, artifact_key, registry, package_name, version,
                   format, spec_version, document, source, created_at, license
            FROM artifact_sboms
            WHERE ($1::TEXT IS NULL OR registry = $1)
              AND ($2::TIMESTAMPTZ IS NULL OR created_at >= $2)
              AND ($3::TIMESTAMPTZ IS NULL OR created_at <= $3)
            ORDER BY registry, created_at DESC
            LIMIT $4 OFFSET $5
            "#,
        )
        .bind(registry)
        .bind(from)
        .bind(to)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .db_err()?;

        Ok(rows.iter().map(row_to_sbom).collect())
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn row_parsing_smoke_fields() {
        // Ensure the format/source string round-trips.
        assert_eq!(SbomFormat::Spdx.as_str(), "spdx");
        assert_eq!(SbomFormat::CycloneDx.as_str(), "cyclonedx");
        assert_eq!(SbomSource::Generated.as_str(), "generated");
        assert_eq!(SbomSource::Upstream.as_str(), "upstream");
        assert_eq!(SbomSource::Extracted.as_str(), "extracted");

        assert_eq!(SbomFormat::parse("spdx"), Some(SbomFormat::Spdx));
        assert_eq!(SbomFormat::parse("cyclonedx"), Some(SbomFormat::CycloneDx));
        assert_eq!(SbomFormat::parse("unknown"), None);

        assert_eq!(SbomSource::parse("generated"), Some(SbomSource::Generated));
        assert_eq!(SbomSource::parse("unknown"), None);
    }

    #[test]
    fn upsert_sbom_round_trip_shape() {
        // Verify that a newly constructed ArtifactSbom has the expected shape.
        let sbom = ArtifactSbom {
            id: Uuid::new_v4(),
            artifact_key: "artifact:npm/lodash/4.17.21".into(),
            registry: "npm".into(),
            package_name: "lodash".into(),
            version: "4.17.21".into(),
            format: SbomFormat::Spdx,
            spec_version: "2.3".into(),
            document: serde_json::json!({"spdxVersion": "SPDX-2.3"}),
            source: SbomSource::Generated,
            created_at: Utc::now(),
            license: None,
        };
        assert_eq!(sbom.format.as_str(), "spdx");
        assert_eq!(sbom.spec_version, "2.3");
    }
}
