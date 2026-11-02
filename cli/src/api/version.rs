use anyhow::Result;

use super::admin::BulkPackageResult;
use super::BatleHubClient;

/// Report the first per-package failure from a bulk-endpoint call as an error,
/// matching the single-package yank/unyank/delete contract these bulk calls
/// stand in for.
fn require_success(result: BulkPackageResult) -> Result<()> {
    if let Some(f) = result.failed.first() {
        anyhow::bail!("{}/{}: {}", f.name, f.version, f.error);
    }
    Ok(())
}

impl BatleHubClient {
    /// Yanks/unyanks/deletes call their bulk-endpoint counterpart with a
    /// single-package list rather than maintaining a parallel single-package
    /// DTO and response shape, so there is exactly one client-side
    /// implementation of each bulk-* endpoint.
    pub async fn yank_version(&self, registry: &str, name: &str, version: &str) -> Result<()> {
        let result = self
            .bulk_yank(registry, vec![(name.to_string(), version.to_string())])
            .await?;
        require_success(result)
    }

    pub async fn unyank_version(&self, registry: &str, name: &str, version: &str) -> Result<()> {
        let result = self
            .bulk_unyank(registry, vec![(name.to_string(), version.to_string())])
            .await?;
        require_success(result)
    }

    /// Pin or unpin a version against retention (RFC 0016 §4.1).
    ///
    /// Not a bulk endpoint, unlike its neighbours above: a pin is a considered
    /// decision about one release, and there is no bulk-pin to stand in for.
    pub async fn set_retention_pin(
        &self,
        registry: &str,
        name: &str,
        version: &str,
        keep: bool,
    ) -> Result<()> {
        #[derive(serde::Serialize)]
        struct PinRequest<'a> {
            name: &'a str,
            version: &'a str,
            keep: bool,
        }
        self.post_void(
            &format!("/api/v1/admin/registries/{registry}/retention-pin"),
            &PinRequest {
                name,
                version,
                keep,
            },
        )
        .await
    }

    /// Run retention over a registry (RFC 0016 §4.2).
    ///
    /// `dry_run` is sent explicitly rather than left to the server's config,
    /// and only ever tightens: the server refuses to let a query string disarm a
    /// configured `dry_run = true`, so passing `false` here reclaims only on a
    /// registry an operator has already configured to.
    pub async fn run_retention(&self, registry: &str, dry_run: bool) -> Result<RetentionReport> {
        self.post(
            &format!("/api/v1/admin/registries/{registry}/retention?dry_run={dry_run}"),
            &(),
        )
        .await
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RetentionDecision {
    pub name: String,
    pub version: String,
    /// The condition that kept this version, or absent when it is reclaimed.
    #[serde(default)]
    pub kept_because: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct RetentionReport {
    pub registry: String,
    pub examined: u64,
    pub reclaimed: u64,
    pub kept: u64,
    pub dry_run: bool,
    #[serde(default)]
    pub reclaimed_coordinates: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<RetentionDecision>,
    #[serde(default)]
    pub decisions_truncated: u64,
    /// Set when the run stopped early — a partial run that says so.
    #[serde(default)]
    pub incomplete_because: Option<String>,
}
