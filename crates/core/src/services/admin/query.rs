use chrono::{DateTime, Utc};

use super::AdminService;
use crate::entities::{
    AccessAction, AccessEvent, CallerNet, EventFilter, Identity, PackageFilter, PackageId,
    PackageStatus, PackageSummary,
};
use crate::error::CoreError;

impl AdminService {
    pub async fn list_packages(
        &self,
        filter: PackageFilter,
    ) -> Result<Vec<PackageSummary>, CoreError> {
        self.repo.list_packages(filter).await
    }

    pub async fn count_packages(&self, filter: PackageFilter) -> Result<u64, CoreError> {
        self.repo.count_packages(filter).await
    }

    pub async fn list_events(&self, filter: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
        self.repo.list_events(filter).await
    }

    pub async fn count_events(&self, filter: EventFilter) -> Result<u64, CoreError> {
        self.repo.count_events(filter).await
    }

    /// Purge access-audit rows older than `before`. Records one `AuditPurge`
    /// event capturing who ran the purge and the resulting row count, so the
    /// compliance trail survives even though the purge itself removes history.
    pub async fn purge_events_before(
        &self,
        before: DateTime<Utc>,
        by_identity: &Identity,
        net: &CallerNet,
    ) -> Result<u64, CoreError> {
        let deleted = self.repo.purge_events_before(before).await?;

        self.record_account_action(AccessAction::AuditPurge, by_identity, net)
            .await;

        tracing::info!(
            user_id = by_identity.user_id.as_deref().unwrap_or(""),
            cutoff = %before,
            deleted,
            "access-audit log purged"
        );
        Ok(deleted)
    }

    pub async fn get_package_status(&self, pkg: &PackageId) -> Result<PackageStatus, CoreError> {
        self.repo.get_status(pkg).await
    }
}
