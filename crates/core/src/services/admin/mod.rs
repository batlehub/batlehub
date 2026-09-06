mod explore;
mod packages;
mod query;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::entities::{
    AccessAction, AccessEvent, AccessResult, ArtifactVulnerability, Identity, PackageId,
};
use crate::error::CoreError;
use crate::ports::{PackageRepository, VulnerabilityRepository};
use crate::services::explore_cache::ExploreCache;
use crate::services::hot_config::HotConfigLock;

/// Cap on simultaneous in-flight operations for a single bulk admin action, to
/// avoid a large selection (thousands of packages) opening more concurrent DB
/// connections than the pool can serve.
const BULK_ACTION_CONCURRENCY: usize = 16;

/// How many download rows to read per package wanted by
/// [`AdminService::recent_own_packages`]. Repeated pulls of one package are the
/// normal case, so the newest rows skew heavily towards a handful of names.
const RECENT_DOWNLOAD_SCAN_FACTOR: u64 = 40;

/// Hard ceiling on that scan, so a large `max_packages` cannot turn one widget
/// into an unbounded read of the audit trail.
const RECENT_DOWNLOAD_SCAN_CAP: u64 = 500;

pub struct BulkBlockItem {
    pub package_id: PackageId,
    pub reason: String,
}

pub struct BulkActionResult {
    pub succeeded: Vec<PackageId>,
    pub failed: Vec<(PackageId, String)>,
}

pub struct AdminService {
    pub repo: Arc<dyn PackageRepository>,
    pub explore_cache: Arc<ExploreCache>,
    /// Optional source of vulnerability findings (the periodic SBOM re-scan).
    /// When absent, `list_vulnerabilities` returns an empty list.
    pub vuln_repo: Option<Arc<dyn VulnerabilityRepository>>,
    /// Where the verdict pipeline lives, so a block reaches the download gate
    /// on a `[security]` registry (RFC 0018 §6.1).
    ///
    /// On such a registry `BlockListRule` is not in the rule chain — the gate
    /// is `VerdictGateRule`, which reads only the stored verdict — so a block
    /// that writes just the status row is invisible to downloads until a scan
    /// happens to run. `None` is the pre-`[security]` deployment, where
    /// `BlockListRule` reads the status row on every request and there is
    /// nothing to propagate.
    pub hot: Option<HotConfigLock>,
}

impl AdminService {
    pub fn new(repo: Arc<dyn PackageRepository>) -> Self {
        Self {
            repo,
            explore_cache: Arc::new(ExploreCache::new()),
            vuln_repo: None,
            hot: None,
        }
    }

    /// Attach the hot config so block/unblock propagate to verdicts.
    #[must_use]
    pub fn with_hot_config(mut self, hot: HotConfigLock) -> Self {
        self.hot = Some(hot);
        self
    }

    /// Attach a vulnerability repository so package detail views can surface
    /// findings recorded by the periodic SBOM re-scan.
    #[must_use]
    pub fn with_vulnerability_repo(mut self, repo: Arc<dyn VulnerabilityRepository>) -> Self {
        self.vuln_repo = Some(repo);
        self
    }

    /// List recorded vulnerability findings for a package coordinate.
    /// Returns an empty list when no vulnerability repository is attached.
    pub async fn list_vulnerabilities(
        &self,
        registry: &str,
        name: &str,
        version: &str,
    ) -> Result<Vec<ArtifactVulnerability>, CoreError> {
        match &self.vuln_repo {
            Some(repo) => repo.list_for_coordinate(registry, name, version).await,
            None => Ok(vec![]),
        }
    }

    /// List recorded findings for many coordinates in one go.
    /// Returns an empty list when no vulnerability repository is attached.
    pub async fn list_vulnerabilities_for(
        &self,
        coordinates: &[PackageId],
    ) -> Result<Vec<(PackageId, Vec<ArtifactVulnerability>)>, CoreError> {
        match &self.vuln_repo {
            Some(repo) => repo.list_for_coordinates(coordinates).await,
            None => Ok(vec![]),
        }
    }

    /// The caller's own successful downloads, newest first.
    ///
    /// A thin delegate: the scoping that makes this safe lives in
    /// [`PackageRepository::list_own_downloads`], not here (RFC 0004 §6.2).
    pub async fn list_own_downloads(
        &self,
        user_id: &str,
        since: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<AccessEvent>, CoreError> {
        self.repo.list_own_downloads(user_id, since, limit).await
    }

    /// The `max_coordinates` most recently pulled *coordinates*, each with the
    /// time it was last pulled.
    ///
    /// RFC 0004 R6 bounds "recently pulled" on both axes — a window and a count
    /// — because either alone degenerates: a count is meaningless for a busy
    /// user, and a window is unbounded for one.
    ///
    /// Collapsing is by the full `(registry, name, version)` coordinate
    /// (RFC 0004 R15): repeated pulls of one version become one row, and two
    /// versions of a package stay two rows. An advisory is a fact about a
    /// version, so the version is what a reader has to see — a row that named
    /// only the package would leave them guessing which of the versions they
    /// pulled is the affected one.
    ///
    /// `artifact` is dropped from the key and from the result. It names a file
    /// within a coordinate (a tarball, a `.vsix`, a GitHub asset id), and
    /// findings are recorded per coordinate, so keeping it would split one
    /// version into several rows that all carry the same advisories.
    pub async fn recent_own_coordinates(
        &self,
        user_id: &str,
        since: DateTime<Utc>,
        max_coordinates: usize,
    ) -> Result<Vec<(PackageId, DateTime<Utc>)>, CoreError> {
        if max_coordinates == 0 {
            return Ok(vec![]);
        }
        // Scan more rows than we keep: repeatedly pulling the same version is
        // the common case (a CI job with a warm lockfile), so the newest N rows
        // are often one coordinate N times.
        let scan_limit = (max_coordinates as u64)
            .saturating_mul(RECENT_DOWNLOAD_SCAN_FACTOR)
            .min(RECENT_DOWNLOAD_SCAN_CAP);

        let events = self
            .repo
            .list_own_downloads(user_id, since, scan_limit)
            .await?;

        let mut seen: Vec<(String, String, String)> = Vec::with_capacity(max_coordinates);
        let mut out = Vec::with_capacity(max_coordinates);
        // `list_own_downloads` returns newest first, so the first row for a
        // coordinate is the most recent pull of it.
        for event in events {
            let Some(pkg) = event.package_id else {
                continue;
            };
            let key = (pkg.registry.clone(), pkg.name.clone(), pkg.version.clone());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            out.push((
                PackageId::new(pkg.registry, pkg.name, pkg.version),
                event.timestamp,
            ));
            if out.len() == max_coordinates {
                break;
            }
        }
        Ok(out)
    }

    /// Record a cached artifact dropped by hand.
    ///
    /// Public because the three surfaces that do it are handlers with no other
    /// business in `AdminService` — `DELETE /registries/{r}/cache`,
    /// `POST /packages/invalidate`, and `POST /registries/{r}/clear-cache` —
    /// and until this existed all three deleted cached bytes and left nothing
    /// behind at all.
    ///
    /// `pkg` is `None` for the whole-registry clear, which is a
    /// `delete_by_prefix` that never knew the coordinates. Pass
    /// [`AccessAction::CacheClear`] with it; [`AccessAction::CacheEvict`]
    /// always carries one.
    pub async fn record_cache_eviction(
        &self,
        pkg: Option<PackageId>,
        action: AccessAction,
        by_identity: &Identity,
    ) {
        self.record_admin_action(pkg, action, by_identity).await;
    }

    /// Shared audit-write path for admin actions that don't otherwise touch
    /// `PackageRepository` (ownership/visibility edits go through their own
    /// ports, account/network-wide actions have no package at all). Mirrors
    /// the fail-open behaviour of `block_package`/`unblock_package`/
    /// `delete_package`: an audit-write failure is logged but never fails the
    /// calling admin action.
    pub(super) async fn record_admin_action(
        &self,
        package_id: Option<PackageId>,
        action: AccessAction,
        by_identity: &Identity,
    ) {
        self.repo
            .record_access(AccessEvent {
                id: uuid::Uuid::new_v4(),
                user_id: by_identity.user_id.clone(),
                user_role: by_identity.role.clone(),
                package_id,
                action,
                result: AccessResult::Allowed,
                timestamp: chrono::Utc::now(),
                ip_address: None,
                user_agent: None,
            })
            .await
            .unwrap_or_else(|e| tracing::warn!(error = %e, "failed to record admin action"));
    }

    /// Shared fan-out path for bulk admin actions: runs `op` over `items` with
    /// bounded concurrency and aggregates the per-item outcomes into a
    /// [`BulkActionResult`]. `op` reports its own failure message (rather than
    /// a `CoreError`) so callers can report domain-specific failures — e.g.
    /// `bulk_delete_packages`'s "package not found" for a `false` return —
    /// without forcing every bulk action through the same error type.
    pub(super) async fn run_bulk<T, F, Fut>(&self, items: Vec<T>, op: F) -> BulkActionResult
    where
        F: Fn(T) -> Fut,
        Fut: Future<Output = (PackageId, Result<(), String>)>,
    {
        use futures::StreamExt;

        let results: Vec<_> = futures::stream::iter(items)
            .map(op)
            .buffer_unordered(BULK_ACTION_CONCURRENCY)
            .collect()
            .await;

        let mut result = BulkActionResult {
            succeeded: vec![],
            failed: vec![],
        };
        for (pkg, outcome) in results {
            match outcome {
                Ok(()) => result.succeeded.push(pkg),
                Err(msg) => result.failed.push((pkg, msg)),
            }
        }
        result
    }
}
