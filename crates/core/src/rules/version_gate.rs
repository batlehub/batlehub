use crate::entities::{Identity, Role};
use crate::rules::version_match::VersionMatcher;
use crate::rules::{Rule, RuleContext, RuleDecision};
use async_trait::async_trait;

/// Gates downloads by version: an optional approved-version allowlist plus a
/// blocklist of specific versions with known issues.
///
/// The resolved package version (`ctx.package.id.version`) is matched against
/// both lists. A version that matches any `block` entry is rejected. When
/// `allow` is non-empty, a version that matches **none** of its entries is also
/// rejected. Roles listed in `bypass_roles` may still download a gated version.
///
/// ```toml
/// [[registries.rules]]
/// kind = "version_gate"
/// allow = [">=1.2.0, <2.0.0"]   # optional approved-version allowlist
/// block = ["1.4.7", "1.5.0"]    # specific versions with known issues
/// bypass_roles = ["admin"]
/// ```
pub struct VersionGateRule {
    allow: Vec<VersionMatcher>,
    block: Vec<VersionMatcher>,
    bypass_roles: Vec<Role>,
}

impl VersionGateRule {
    pub fn new(allow: &[String], block: &[String], bypass_roles: Vec<Role>) -> Self {
        Self {
            allow: allow.iter().map(|s| VersionMatcher::parse(s)).collect(),
            block: block.iter().map(|s| VersionMatcher::parse(s)).collect(),
            bypass_roles,
        }
    }

    /// Turn a gate violation into a verdict, admitting a caller at or above the
    /// least-privileged configured bypass role ([`crate::rules::gate`]).
    fn gate(&self, identity: &Identity, reason: String) -> RuleDecision {
        crate::rules::gate(&self.bypass_roles, identity, reason)
    }
}

#[async_trait]
impl Rule for VersionGateRule {
    fn name(&self) -> &str {
        "version_gate"
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let version = ctx.package.id.version.as_str();

        if self.block.iter().any(|m| m.matches(version)) {
            return self.gate(
                ctx.identity,
                format!("version '{version}' is blocked by policy (known issue)"),
            );
        }

        if !self.allow.is_empty() && !self.allow.iter().any(|m| m.matches(version)) {
            return self.gate(
                ctx.identity,
                format!("version '{version}' is not in the approved allowlist"),
            );
        }

        RuleDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Action;
    use crate::entities::{Identity, PackageId, PackageMetadata, Role};
    use crate::rules::RuleContext;

    fn meta(version: &str) -> PackageMetadata {
        PackageMetadata {
            id: PackageId::new("npm", "lodash", version),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
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
            requested_version: Some(meta.id.version.as_str()),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The name is what a refusal and the access log carry (as `cve_gate`'s).
    #[test]
    fn rule_name_is_version_gate() {
        assert_eq!(
            VersionGateRule::new(&[], &[], vec![]).name(),
            "version_gate"
        );
    }

    #[tokio::test]
    async fn no_lists_allows_everything() {
        let rule = VersionGateRule::new(&[], &[], vec![]);
        let m = meta("1.2.3");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn exact_block_denies() {
        let rule = VersionGateRule::new(&[], &strings(&["1.4.7"]), vec![]);
        let m = meta("1.4.7");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn exact_block_allows_other_versions() {
        let rule = VersionGateRule::new(&[], &strings(&["1.4.7"]), vec![]);
        let m = meta("1.4.8");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn allowlist_miss_denies() {
        let rule = VersionGateRule::new(&strings(&[">=1.2.0, <2.0.0"]), &[], vec![]);
        let m = meta("2.0.1");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn allowlist_semver_range_hit_allows() {
        let rule = VersionGateRule::new(&strings(&[">=1.2.0, <2.0.0"]), &[], vec![]);
        let m = meta("1.9.9");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn block_takes_precedence_over_allow() {
        let rule = VersionGateRule::new(&strings(&[">=1.0.0"]), &strings(&["1.4.7"]), vec![]);
        let m = meta("1.4.7");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn exact_match_is_not_caret() {
        // A bare "1.2.3" entry must be exact, not the caret range VersionReq would
        // otherwise infer (which would also match 1.2.5).
        let rule = VersionGateRule::new(&strings(&["1.2.3"]), &[], vec![]);
        let m = meta("1.2.5");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn bypass_role_resolves_to_allow_for_admin() {
        let rule = VersionGateRule::new(&[], &strings(&["1.4.7"]), vec![Role::Admin]);
        let m = meta("1.4.7");
        let admin = identity(Role::Admin);
        let decision = rule.evaluate(&ctx(&m, &admin)).await;
        assert!(matches!(decision, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn bypass_role_resolves_to_deny_for_anonymous() {
        let rule = VersionGateRule::new(&[], &strings(&["1.4.7"]), vec![Role::Admin]);
        let m = meta("1.4.7");
        let anon = identity(Role::Anonymous);
        let decision = rule.evaluate(&ctx(&m, &anon)).await;
        assert!(matches!(decision, RuleDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn non_semver_version_matched_exactly() {
        let rule = VersionGateRule::new(&[], &strings(&["deadbeef"]), vec![]);
        let m = meta("deadbeef");
        let id = identity(Role::Anonymous);
        assert!(matches!(
            rule.evaluate(&ctx(&m, &id)).await,
            RuleDecision::Deny { .. }
        ));
    }
}
