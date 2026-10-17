use async_trait::async_trait;

use crate::entities::Role;
use crate::rules::{Rule, RuleContext, RuleDecision};

/// Blocks access to a release that the upstream registry reports as
/// unsigned, unless the caller clears `bypass_roles` (`Role::clears_bypass`).
///
/// This checks `PackageMetadata::is_signed`, which registry adapters populate
/// with a best-effort signal (e.g. presence of a `.asc`/`.sig` release asset
/// on GitHub/Forgejo, a signature blob in an OpenVSX/VS Code extension). It
/// is not full cryptographic signature *verification* — adapters that have
/// no such signal for their ecosystem (npm, PyPI, crates.io, Maven, …) leave
/// `is_signed` as `None`, and this rule treats `None` the same as a missing
/// timestamp in [`super::ReleaseAgeGateRule`]: skipped by default, or denied
/// when `deny_missing_signature` is set.
pub struct RequireSignedReleaseRule {
    pub bypass_roles: Vec<Role>,
    /// When `true`, deny the request if the upstream provides no signature
    /// signal at all (`is_signed == None`). When `false` (the default), a
    /// missing signal causes the rule to be skipped and the download to
    /// proceed.
    pub deny_missing_signature: bool,
}

impl RequireSignedReleaseRule {
    pub fn new(bypass_roles: Vec<Role>) -> Self {
        Self {
            bypass_roles,
            deny_missing_signature: false,
        }
    }

    pub fn with_deny_missing_signature(mut self, deny: bool) -> Self {
        self.deny_missing_signature = deny;
        self
    }
}

#[async_trait]
impl Rule for RequireSignedReleaseRule {
    fn name(&self) -> &str {
        "require_signed_release"
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let bypassed = ctx.identity.role.clears_bypass(&self.bypass_roles);

        let signed = match ctx.package.is_signed {
            Some(signed) => signed,
            None => {
                return if self.deny_missing_signature {
                    deny(bypassed)
                } else {
                    RuleDecision::Allow
                }
            }
        };

        if signed || bypassed {
            RuleDecision::Allow
        } else {
            deny(bypassed)
        }
    }
}

fn deny(bypassed: bool) -> RuleDecision {
    if bypassed {
        return RuleDecision::Allow;
    }
    RuleDecision::Deny {
        reason: "this release is not signed; require_signed_release is enabled for this registry"
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Action;
    use crate::entities::{Identity, PackageId, PackageMetadata, Role};
    use crate::rules::RuleContext;

    fn meta(is_signed: Option<bool>) -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("github", "octo/repo", "1.0.0"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    fn identity(role: Role) -> Identity {
        Identity {
            user_id: None,
            role,
            auth_provider: None,
            groups: vec![],
        }
    }

    fn ctx<'a>(meta: &'a PackageMetadata, identity: &'a Identity) -> RuleContext<'a> {
        RuleContext {
            identity,
            package: meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: Some("1.0.0"),
        }
    }

    #[tokio::test]
    async fn allows_signed_release() {
        let rule = RequireSignedReleaseRule::new(vec![]);
        let m = meta(Some(true));
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn denies_unsigned_release_with_no_bypass() {
        let rule = RequireSignedReleaseRule::new(vec![]);
        let m = meta(Some(false));
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn bypass_role_allows_unsigned_release() {
        let rule = RequireSignedReleaseRule::new(vec![Role::Admin]);
        let m = meta(Some(false));
        let id = identity(Role::Admin);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn missing_signal_allows_by_default() {
        let rule = RequireSignedReleaseRule::new(vec![]);
        let m = meta(None);
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn missing_signal_denied_when_configured() {
        let rule = RequireSignedReleaseRule::new(vec![]).with_deny_missing_signature(true);
        let m = meta(None);
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn missing_signal_denied_but_bypassed() {
        let rule =
            RequireSignedReleaseRule::new(vec![Role::Admin]).with_deny_missing_signature(true);
        let m = meta(None);
        let id = identity(Role::Admin);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Allow
        ));
    }
}
