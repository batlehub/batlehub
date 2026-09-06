use chrono::Utc;

use super::{AdminService, BulkActionResult, BulkBlockItem};
use crate::entities::{
    AccessAction, Finding, FindingKind, Identity, PackageId, PackageStatus, ReasonCode,
    ScanTrigger, Severity, VerdictState, BLOCK_LIST_SCANNER,
};
use crate::error::CoreError;

impl AdminService {
    pub async fn block_package(
        &self,
        pkg: &PackageId,
        reason: String,
        by_identity: &Identity,
    ) -> Result<(), CoreError> {
        let blocked_by = by_identity
            .user_id
            .clone()
            .unwrap_or_else(|| by_identity.role.to_string());

        self.repo
            .set_status(
                pkg,
                PackageStatus::Blocked {
                    reason: reason.clone(),
                    blocked_by: blocked_by.clone(),
                    blocked_at: Utc::now(),
                },
            )
            .await?;

        self.record_admin_action(Some(pkg.clone()), AccessAction::Block, by_identity)
            .await;

        self.propagate_to_verdict(pkg, Some((&reason, &blocked_by)))
            .await;

        tracing::info!(package = %pkg, blocked_by = %blocked_by, reason = %reason, "package blocked");
        Ok(())
    }

    pub async fn unblock_package(
        &self,
        pkg: &PackageId,
        by_identity: &Identity,
    ) -> Result<(), CoreError> {
        self.repo.set_status(pkg, PackageStatus::Available).await?;

        self.record_admin_action(Some(pkg.clone()), AccessAction::Unblock, by_identity)
            .await;

        self.propagate_to_verdict(pkg, None).await;

        tracing::info!(package = %pkg, "package unblocked");
        Ok(())
    }

    /// Carry a block or unblock into the verdict pipeline of a `[security]`
    /// registry, the way [`crate::services::FlagService`] carries a pushed
    /// flag (RFC 0018 §6.1).
    ///
    /// Two steps, and the order matters:
    ///
    /// 1. **The stored verdict is rewritten here and now.** A block is an
    ///    operator's containment decision during an incident; it cannot wait
    ///    for a worker to lease a job, and on a registry with no `[rescan]`
    ///    interval no worker would ever be asked to. `BlockListScanner`
    ///    re-derives the identical finding on the next scan, so the two agree.
    /// 2. **A `Webhook`-priority rescan is queued**, so every other scanner's
    ///    view of the coordinate is refreshed too and the durable path stays
    ///    the scanner's.
    ///
    /// A coordinate with no stored verdict has nothing to rewrite, so only the
    /// second step runs for it: the queued scan is what makes the block durable
    /// there, at the cost of the worker resolving — and, for the scanners that
    /// want bytes, downloading — a coordinate this instance had never fetched.
    ///
    /// Failures here are logged, never returned: the status row is already
    /// written and the audit event already recorded, so failing the admin call
    /// would report "not blocked" for a package that is. The queued rescan is
    /// the backstop.
    async fn propagate_to_verdict(&self, pkg: &PackageId, blocked: Option<(&str, &str)>) {
        let Some(hot) = &self.hot else { return };
        let (verdicts, queue) = {
            let hot = hot.read().await;
            // Not a `[security]` registry: `BlockListRule` is in its chain and
            // reads the status row on every request. Nothing to propagate.
            if !hot.security.contains_key(&pkg.registry) {
                return;
            }
            (hot.verdicts.clone(), hot.scan_queue.clone())
        };

        // A verdict is a *version*: the store is keyed by `coordinate_key`,
        // which drops the artifact, and the download gate has no per-file
        // granularity. A per-artifact block is a different promise —
        // `BlockListRule` documents that it leaves the version's other files
        // alone — and it cannot be expressed here. Reading the store under an
        // artifact-bearing key always misses, and queueing a job under one
        // would hand `BlockListScanner` the per-file status row and file its
        // finding against the whole version, denying every file of it.
        if pkg.artifact.is_some() {
            tracing::warn!(package = %pkg,
                "block: a per-artifact block has no effect on a [security] registry, whose gate \
                 judges the whole version; the version's verdict is left untouched");
            return;
        }

        if let Some(store) = verdicts {
            match store.get(pkg).await {
                Ok(Some(mut v)) => {
                    // Drop any previous block finding first, so a re-block with
                    // an edited reason does not leave both on the verdict.
                    v.findings.retain(|f| {
                        f.scanner != BLOCK_LIST_SCANNER && f.kind != FindingKind::BlockList
                    });
                    match blocked {
                        Some((reason, blocked_by)) => {
                            v.findings.push(Finding::new(
                                BLOCK_LIST_SCANNER,
                                FindingKind::BlockList,
                                ReasonCode::BlockList,
                                Severity::Critical,
                                format!("blocked by {blocked_by}: {reason}"),
                            ));
                            v.state = VerdictState::Denied;
                            if !v.reason_codes.contains(&ReasonCode::BlockList) {
                                v.reason_codes.push(ReasonCode::BlockList);
                            }
                            v.available_at = None;
                        }
                        None => {
                            // Unblocking only removes the block's own finding
                            // and reason code. Whether the version is servable
                            // again is the rescan's call, not this method's —
                            // another scanner may hold it for its own reason.
                            v.reason_codes.retain(|c| *c != ReasonCode::BlockList);
                        }
                    }
                    v.evaluated_at = Utc::now();
                    if let Err(e) = store.upsert(&v).await {
                        tracing::warn!(package = %pkg, error = %e,
                            "block: verdict not written, the rescan will catch up");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(package = %pkg, error = %e,
                    "block: verdict store unreadable, the rescan will catch up"),
            }
        }

        if let Some(queue) = queue {
            if let Err(e) = queue.enqueue(pkg, None, ScanTrigger::Webhook).await {
                tracing::warn!(package = %pkg, error = %e, "block: could not queue rescan");
            }
        }
    }

    /// Record an audit event for a package-scoped admin action performed
    /// through a port other than `PackageRepository` (e.g. ownership grants,
    /// visibility changes).
    pub async fn record_package_action(
        &self,
        pkg: &PackageId,
        action: AccessAction,
        by_identity: &Identity,
    ) {
        self.record_admin_action(Some(pkg.clone()), action, by_identity)
            .await;
    }

    /// Record an audit event for an account-wide or network-wide admin
    /// action that is not scoped to any specific package (user block/unblock,
    /// IP block/unblock).
    pub async fn record_account_action(&self, action: AccessAction, by_identity: &Identity) {
        self.record_admin_action(None, action, by_identity).await;
    }

    pub async fn bulk_block_packages(
        &self,
        items: Vec<BulkBlockItem>,
        by_identity: &Identity,
    ) -> BulkActionResult {
        self.run_bulk(items, |item| async move {
            let outcome = self
                .block_package(&item.package_id, item.reason, by_identity)
                .await;
            (item.package_id, outcome.map_err(|e| e.to_string()))
        })
        .await
    }

    pub async fn bulk_unblock_packages(
        &self,
        items: Vec<PackageId>,
        by_identity: &Identity,
    ) -> BulkActionResult {
        self.run_bulk(items, |pkg| async move {
            let outcome = self.unblock_package(&pkg, by_identity).await;
            (pkg, outcome.map_err(|e| e.to_string()))
        })
        .await
    }

    /// Remove a package's administrative record.
    ///
    /// Returns `true` if the row existed and was deleted. The caller is responsible
    /// for also purging the cached artifact from storage when desired.
    pub async fn delete_package(
        &self,
        pkg: &PackageId,
        by_identity: &Identity,
    ) -> Result<bool, CoreError> {
        let deleted = self.repo.delete_package(pkg).await?;
        if deleted {
            self.record_admin_action(Some(pkg.clone()), AccessAction::Delete, by_identity)
                .await;
            tracing::info!(package = %pkg, "package record deleted");
        }
        Ok(deleted)
    }

    pub async fn bulk_delete_packages(
        &self,
        items: Vec<PackageId>,
        by_identity: &Identity,
    ) -> BulkActionResult {
        self.run_bulk(items, |pkg| async move {
            let outcome = match self.delete_package(&pkg, by_identity).await {
                Ok(true) => Ok(()),
                Ok(false) => Err("package not found".to_string()),
                Err(e) => Err(e.to_string()),
            };
            (pkg, outcome)
        })
        .await
    }
}
