use std::sync::Arc;

use async_trait::async_trait;

use crate::ports::PackageRepository;
use crate::rules::{Rule, RuleContext, RuleDecision};

/// Denies access if an administrator has manually blocked the package.
pub struct BlockListRule {
    pub repo: Arc<dyn PackageRepository>,
}

impl BlockListRule {
    pub fn new(repo: Arc<dyn PackageRepository>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl Rule for BlockListRule {
    fn name(&self) -> &str {
        "block_list"
    }

    /// Downloads address a *file within* a version — `…/1.1.0/tarball` for
    /// npm, `dl` for cargo, a classifier for Maven — so the requested
    /// coordinate carries an `artifact` the operator's block does not: blocking
    /// a version records `artifact = None`, and `get_status` matches all four
    /// fields. Without the widening in [`PackageRepository::covering_block`] a
    /// blocked npm version stayed fully downloadable by anyone who knew its
    /// number, while the admin UI showed it as blocked.
    ///
    /// A block on the bare version therefore covers every artifact of it. The
    /// converse does not hold: blocking one artifact leaves the version's other
    /// files alone, which is what makes per-artifact blocks useful.
    ///
    /// A repository error allows. SECURITY: fail-open by design — prefer
    /// availability over blocking. If the DB is unreachable we allow the
    /// request through rather than turning the proxy into a brick wall. Accept
    /// this trade-off only for self-hosted deployments where uptime matters
    /// more than hard blocks.
    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        match self.repo.covering_block(&ctx.package.id).await {
            Ok(Some(reason)) => RuleDecision::Deny { reason },
            Ok(None) => RuleDecision::Allow,
            Err(e) => {
                tracing::warn!(
                    package = %ctx.package.id,
                    error = %e,
                    "BlockListRule: failed to query package status, failing open"
                );
                RuleDecision::Allow
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::entities::Action;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Utc;

    use super::*;
    use crate::entities::{
        AccessEvent, EventFilter, Identity, PackageFilter, PackageId, PackageMetadata,
        PackageStatus, PackageSummary,
    };
    use crate::error::CoreError;
    use crate::ports::PackageRepository;
    use crate::rules::{Rule, RuleContext};

    struct MemRepo {
        statuses: Mutex<HashMap<String, PackageStatus>>,
    }

    impl MemRepo {
        fn available() -> Arc<Self> {
            Arc::new(Self {
                statuses: Mutex::new(HashMap::new()),
            })
        }

        fn blocked(reason: &str) -> Arc<Self> {
            let r = Arc::new(Self {
                statuses: Mutex::new(HashMap::new()),
            });
            r.statuses.lock().unwrap().insert(
                "npm/evil/1.0.0".to_owned(),
                PackageStatus::Blocked {
                    reason: reason.to_owned(),
                    blocked_by: "admin".to_owned(),
                    blocked_at: Utc::now(),
                },
            );
            r
        }
    }

    #[async_trait]
    impl PackageRepository for MemRepo {
        async fn record_access(&self, _e: AccessEvent) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get_status(&self, pkg: &PackageId) -> Result<PackageStatus, CoreError> {
            Ok(self
                .statuses
                .lock()
                .unwrap()
                .get(&pkg.cache_key())
                .cloned()
                .unwrap_or(PackageStatus::Available))
        }
        async fn set_status(
            &self,
            pkg: &PackageId,
            status: PackageStatus,
        ) -> Result<(), CoreError> {
            self.statuses
                .lock()
                .unwrap()
                .insert(pkg.cache_key(), status);
            Ok(())
        }
        async fn list_packages(&self, _f: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
            Ok(vec![])
        }
        async fn count_packages(&self, _f: PackageFilter) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn list_events(&self, _f: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
            Ok(vec![])
        }
        async fn count_events(&self, _f: EventFilter) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn delete_package(&self, pkg: &PackageId) -> Result<bool, CoreError> {
            Ok(self
                .statuses
                .lock()
                .unwrap()
                .remove(&pkg.cache_key())
                .is_some())
        }
    }

    struct AlwaysErrorRepo;

    #[async_trait]
    impl PackageRepository for AlwaysErrorRepo {
        async fn record_access(&self, _e: AccessEvent) -> Result<(), CoreError> {
            Ok(())
        }
        async fn get_status(&self, _pkg: &PackageId) -> Result<PackageStatus, CoreError> {
            Err(CoreError::Database("connection refused".into()))
        }
        async fn set_status(
            &self,
            _pkg: &PackageId,
            _status: PackageStatus,
        ) -> Result<(), CoreError> {
            Ok(())
        }
        async fn list_packages(&self, _f: PackageFilter) -> Result<Vec<PackageSummary>, CoreError> {
            Ok(vec![])
        }
        async fn count_packages(&self, _f: PackageFilter) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn list_events(&self, _f: EventFilter) -> Result<Vec<AccessEvent>, CoreError> {
            Ok(vec![])
        }
        async fn count_events(&self, _f: EventFilter) -> Result<u64, CoreError> {
            Ok(0)
        }
        async fn delete_package(&self, _: &PackageId) -> Result<bool, CoreError> {
            Ok(false)
        }
    }

    fn make_ctx<'a>(pkg: &'a PackageMetadata, identity: &'a Identity) -> RuleContext<'a> {
        RuleContext {
            identity,
            package: pkg,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        }
    }

    fn meta(pkg_id: PackageId) -> PackageMetadata {
        PackageMetadata {
            id: pkg_id,
            published_at: Some(Utc::now() - chrono::Duration::days(10)),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::json!({}),
            cache_control: None,
        }
    }

    #[tokio::test]
    async fn available_package_returns_allow() {
        let rule = BlockListRule::new(MemRepo::available());
        let pkg_id = PackageId::new("npm", "lodash", "4.0.0");
        let m = meta(pkg_id);
        let identity = Identity::anonymous();
        let decision = rule.evaluate(&make_ctx(&m, &identity)).await;
        assert!(matches!(decision, crate::rules::RuleDecision::Allow));
    }

    #[tokio::test]
    async fn blocked_package_returns_deny_with_reason() {
        let repo = MemRepo::blocked("security vulnerability");
        let rule = BlockListRule::new(repo);
        let pkg_id = PackageId::new("npm", "evil", "1.0.0");
        let m = meta(pkg_id);
        let identity = Identity::anonymous();
        let decision = rule.evaluate(&make_ctx(&m, &identity)).await;
        assert!(
            matches!(&decision, crate::rules::RuleDecision::Deny { reason } if reason == "security vulnerability"),
            "expected Deny with correct reason, got {:?}",
            decision,
        );
    }

    /// The download coordinate carries an artifact suffix (`…/1.0.0/tarball`)
    /// that the operator's version-level block does not. Before this was
    /// handled, a blocked npm version stayed downloadable to anyone who knew its
    /// number while the admin UI reported it blocked.
    #[tokio::test]
    async fn version_level_block_covers_an_artifact_of_that_version() {
        let rule = BlockListRule::new(MemRepo::blocked("security vulnerability"));
        let m = meta(PackageId::new("npm", "evil", "1.0.0").with_artifact("tarball"));
        let identity = Identity::anonymous();

        let decision = rule.evaluate(&make_ctx(&m, &identity)).await;
        assert!(
            matches!(&decision, crate::rules::RuleDecision::Deny { reason } if reason == "security vulnerability"),
            "artifact request must inherit the version's block, got {decision:?}",
        );
    }

    /// The relationship is one-way: a block on the *version* covers its files,
    /// but a different version of the same package is unaffected.
    #[tokio::test]
    async fn a_block_does_not_leak_to_other_versions() {
        let rule = BlockListRule::new(MemRepo::blocked("security vulnerability"));
        let m = meta(PackageId::new("npm", "evil", "2.0.0").with_artifact("tarball"));
        let identity = Identity::anonymous();

        let decision = rule.evaluate(&make_ctx(&m, &identity)).await;
        assert!(matches!(decision, crate::rules::RuleDecision::Allow));
    }

    #[tokio::test]
    async fn db_error_fails_open() {
        // SECURITY: intentional fail-open — DB unavailable must not block all traffic.
        let rule = BlockListRule::new(Arc::new(AlwaysErrorRepo));
        let pkg_id = PackageId::new("npm", "pkg", "1.0.0");
        let m = meta(pkg_id);
        let identity = Identity::anonymous();
        let decision = rule.evaluate(&make_ctx(&m, &identity)).await;
        assert!(
            matches!(decision, crate::rules::RuleDecision::Allow),
            "DB error must fail open"
        );
    }
}
