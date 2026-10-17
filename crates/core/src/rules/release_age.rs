use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;

use crate::entities::Role;
use crate::rules::{Rule, RuleContext, RuleDecision};

/// Blocks access to a release that was published less than `min_age` ago,
/// unless the caller holds the least-privileged of the `bypass_roles` or a
/// higher one.
///
/// Useful for preventing immediate mirroring of brand-new releases that might
/// be yanked or recalled within a short window (supply-chain hygiene).
pub struct ReleaseAgeGateRule {
    pub min_age: Duration,
    pub bypass_roles: Vec<Role>,
    /// When `true`, deny the request if the upstream did not provide a
    /// publish timestamp.  When `false` (the default), a missing timestamp
    /// causes the rule to be skipped and the download to proceed.
    pub deny_missing_timestamp: bool,
}

impl ReleaseAgeGateRule {
    pub fn new(min_age: Duration, bypass_roles: Vec<Role>) -> Self {
        Self {
            min_age,
            bypass_roles,
            deny_missing_timestamp: false,
        }
    }

    pub fn with_deny_missing_timestamp(mut self, deny: bool) -> Self {
        self.deny_missing_timestamp = deny;
        self
    }
}

#[async_trait]
impl Rule for ReleaseAgeGateRule {
    fn name(&self) -> &str {
        "release_age_gate"
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let bypassed = ctx.identity.role.clears_bypass(&self.bypass_roles);

        let Some(published_at) = ctx.package.published_at else {
            if !self.deny_missing_timestamp {
                return RuleDecision::Allow;
            }
            return if bypassed {
                RuleDecision::Allow
            } else {
                RuleDecision::Deny {
                    reason: "release timestamp is missing and deny_missing_timestamp is enabled; \
                             the upstream did not provide a publish date for this package"
                        .to_owned(),
                }
            };
        };

        let age = (Utc::now() - published_at).to_std().unwrap_or_default();

        if age >= self.min_age || bypassed {
            return RuleDecision::Allow;
        }

        let remaining_secs = (self.min_age - age).as_secs();
        RuleDecision::Deny {
            reason: format!(
                "release is too recent (age: {}s, minimum: {}s, {} seconds remaining in quarantine)",
                age.as_secs(),
                self.min_age.as_secs(),
                remaining_secs,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Action;
    use chrono::{Duration as CDuration, Utc};

    use crate::entities::{Identity, PackageId, PackageMetadata, Role};
    use crate::rules::RuleContext;

    fn make_meta(published_at: chrono::DateTime<Utc>) -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("github", "owner/repo", "v1.0.0"),
            published_at: Some(published_at),
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    fn make_identity(role: Role) -> Identity {
        Identity {
            user_id: None,
            role,
            auth_provider: None,
            groups: vec![],
        }
    }

    #[tokio::test]
    async fn allows_old_release() {
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![Role::Admin]);
        let meta = make_meta(Utc::now() - CDuration::hours(2));
        let identity = make_identity(Role::Anonymous);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn denies_new_release_for_anonymous() {
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![Role::Admin]);
        let meta = make_meta(Utc::now() - CDuration::minutes(5));
        let identity = make_identity(Role::Anonymous);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(
            rule.evaluate(&ctx).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn admin_bypasses_age_gate() {
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![Role::Admin]);
        let meta = make_meta(Utc::now() - CDuration::minutes(5));
        let identity = make_identity(Role::Admin);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    fn make_meta_no_timestamp() -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("conda", "numpy", "1.26.0"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    #[tokio::test]
    async fn missing_timestamp_allows_by_default() {
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![]);
        let meta = make_meta_no_timestamp();
        let identity = make_identity(Role::Anonymous);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn missing_timestamp_denies_when_configured() {
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![])
            .with_deny_missing_timestamp(true);
        let meta = make_meta_no_timestamp();
        let identity = make_identity(Role::Anonymous);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(
            rule.evaluate(&ctx).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn bypass_role_overrides_missing_timestamp_deny() {
        // A bypass role allows the download even when deny_missing_timestamp is set,
        // consistent with how bypass_roles work for the age check itself.
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![Role::Admin])
            .with_deny_missing_timestamp(true);
        let meta = make_meta_no_timestamp();
        let identity = make_identity(Role::Admin);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn a_role_above_the_listed_bypass_role_bypasses_too() {
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![Role::User])
            .with_deny_missing_timestamp(true);
        let meta = make_meta_no_timestamp();
        let identity = make_identity(Role::Admin);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn non_bypass_role_denied_on_missing_timestamp() {
        let rule = ReleaseAgeGateRule::new(Duration::from_secs(3600), vec![Role::Admin])
            .with_deny_missing_timestamp(true);
        let meta = make_meta_no_timestamp();
        let identity = make_identity(Role::User);
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(
            rule.evaluate(&ctx).await,
            RuleDecision::Deny { .. }
        ));
    }
}
