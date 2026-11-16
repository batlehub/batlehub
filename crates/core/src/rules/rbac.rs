use std::collections::HashMap;

use async_trait::async_trait;

use crate::entities::{expand_patterns, Action, ActionParseError, Role, WildcardScope};
use crate::rules::{Rule, RuleContext, RuleDecision};

/// Checks whether the caller's role or group membership permits the requested operation.
///
/// The `permissions` map mirrors the `[registries.rbac]` TOML section:
/// ```toml
/// anonymous = ["releases:read"]
/// user      = ["releases:read", "source:read"]
/// admin     = ["*"]
/// [registries.rbac.groups]
/// "team-a" = ["releases:read", "source:read"]
/// ```
///
/// # Verbs are resolved here, not at evaluation
///
/// The maps hold [`Action`], not `String`. Until RFC 0015 phase 1 they held the
/// config's strings and compared them per request (`p == "*" || p == wanted`),
/// which put two decisions on the hot path that belong at load: whether a verb
/// exists at all, and what a wildcard covers. Both were unobservable — an
/// unknown verb simply never matched — so a typo in a config was a permission
/// silently granted to nobody.
///
/// Expansion now happens once, in [`RbacRule::from_patterns`], and an unknown
/// verb is a config-load error. `task config:explain` prints the result, because
/// an expansion nobody can print is only half of the property this paragraph
/// claims (RFC 0015 §4.2).
pub struct RbacRule {
    pub permissions: HashMap<Role, Vec<Action>>,
    pub group_permissions: HashMap<String, Vec<Action>>,
}

impl RbacRule {
    /// Build from already-resolved verbs.
    pub fn new(permissions: HashMap<Role, Vec<Action>>) -> Self {
        Self {
            permissions,
            group_permissions: HashMap::new(),
        }
    }

    /// Build from the config's patterns, expanding each one.
    ///
    /// [`WildcardScope::Legacy`], which is RFC 0015 §10 rule 3: a `"*"` in
    /// `[registries.rbac]` has always meant "both of the two verbs that exist",
    /// and reading it as the new wildcard would hand publish, overwrite, yank,
    /// delete, `packages:block`, `gates:exempt` and `audit:read` to every config
    /// that ever wrote one — which `config.example.toml` does eight times.
    pub fn from_patterns(
        permissions: HashMap<Role, Vec<String>>,
    ) -> Result<Self, ActionParseError> {
        let mut resolved = HashMap::new();
        for (role, patterns) in permissions {
            resolved.insert(role, expand_patterns(&patterns, WildcardScope::Legacy)?);
        }
        Ok(Self::new(resolved))
    }

    pub fn with_groups(mut self, group_permissions: HashMap<String, Vec<Action>>) -> Self {
        self.group_permissions = group_permissions;
        self
    }

    /// [`Self::with_groups`] over the config's patterns.
    pub fn with_group_patterns(
        mut self,
        group_permissions: HashMap<String, Vec<String>>,
    ) -> Result<Self, ActionParseError> {
        let mut resolved = HashMap::new();
        for (group, patterns) in group_permissions {
            resolved.insert(group, expand_patterns(&patterns, WildcardScope::Legacy)?);
        }
        self.group_permissions = resolved;
        Ok(self)
    }

    fn is_permitted(&self, role: &Role, action: Action) -> bool {
        // Walk from the requested role down to Anonymous, granting if any level permits.
        // This implements role inheritance: Admin inherits User's permissions, etc.
        let roles_to_check: Vec<&Role> = match role {
            Role::Admin => vec![&Role::Admin, &Role::User, &Role::Anonymous],
            Role::User => vec![&Role::User, &Role::Anonymous],
            Role::Anonymous => vec![&Role::Anonymous],
        };

        for check_role in roles_to_check {
            if let Some(perms) = self.permissions.get(check_role) {
                if perms.contains(&action) {
                    return true;
                }
            }
        }
        false
    }

    fn perms_allow(&self, key: &str, action: Action) -> bool {
        self.group_permissions
            .get(key)
            .map(|perms| perms.contains(&action))
            .unwrap_or(false)
    }

    fn is_permitted_by_group(&self, groups: &[String], action: Action) -> bool {
        groups.iter().any(|g| {
            // Exact match: "oidc1:team-a"
            if self.perms_allow(g, action) {
                return true;
            }
            // Wildcard match: "*:team-a" covers any provider prefix
            if let Some(colon) = g.find(':') {
                let wildcard = format!("*:{}", &g[colon + 1..]);
                if self.perms_allow(&wildcard, action) {
                    return true;
                }
            }
            false
        })
    }
}

#[async_trait]
impl Rule for RbacRule {
    fn name(&self) -> &str {
        "rbac"
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        if self.is_permitted(&ctx.identity.role, ctx.action)
            || self.is_permitted_by_group(&ctx.identity.groups, ctx.action)
        {
            RuleDecision::Allow
        } else {
            RuleDecision::Deny {
                reason: format!(
                    "role '{}' is not permitted to perform '{}' on this registry",
                    ctx.identity.role, ctx.action
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Identity;

    fn make_identity(role: Role) -> Identity {
        Identity {
            user_id: None,
            role,
            auth_provider: None,
            groups: vec![],
        }
    }

    fn make_identity_with_groups(role: Role, groups: Vec<&str>) -> Identity {
        Identity {
            user_id: None,
            role,
            auth_provider: None,
            groups: groups.into_iter().map(str::to_owned).collect(),
        }
    }

    fn make_rule() -> RbacRule {
        RbacRule::from_patterns(HashMap::from([
            (Role::Anonymous, vec!["releases:read".to_owned()]),
            (
                Role::User,
                vec!["releases:read".to_owned(), "source:read".to_owned()],
            ),
            (Role::Admin, vec!["*".to_owned()]),
        ]))
        .expect("fixture patterns are valid")
    }

    /// The name is the denial reason the access log records.
    #[test]
    fn rule_name_is_rbac() {
        assert_eq!(make_rule().name(), "rbac");
    }

    #[tokio::test]
    async fn anonymous_can_read_releases() {
        let rule = make_rule();
        let identity = make_identity(Role::Anonymous);
        let meta = crate::entities::PackageMetadata {
            id: crate::entities::PackageId::new("github", "rust-lang/rust", "v1.80.0"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        };
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
    async fn anonymous_cannot_read_source() {
        let rule = make_rule();
        let identity = make_identity(Role::Anonymous);
        let meta = crate::entities::PackageMetadata {
            id: crate::entities::PackageId::new("github", "rust-lang/rust", "v1.80.0"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        };
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(
            rule.evaluate(&ctx).await,
            RuleDecision::Deny { .. }
        ));
    }

    /// A legacy `"*"` covers the **read** verbs, and `catalogue:browse` is not
    /// one of them.
    ///
    /// It asserted `resource_type: "actions:read"` — a verb nothing defines and
    /// nothing ever asked for — and passed because `"*"` matched any string at
    /// evaluation time. RFC 0015 §10 rule 3 removes that reading: a legacy `"*"`
    /// expands at load to today's reachable read set, so the unspellable case is
    /// now unconstructible rather than allowed.
    ///
    /// It then asserted `catalogue:browse`, which rule 3 as written listed among
    /// the four — and which §10 rule 2 says may only come from the conjunction of
    /// the `explore` flag with proxy access. The two rules disagreed and rule 3
    /// was wrong; see `LEGACY_WILDCARD_EXPANSION`. The verb this asserts is now
    /// one the wildcard genuinely covers.
    #[tokio::test]
    async fn admin_wildcard_covers_the_read_verbs() {
        let rule = make_rule();
        let identity = make_identity(Role::Admin);
        let meta = crate::entities::PackageMetadata {
            id: crate::entities::PackageId::new("github", "rust-lang/rust", "v1.80.0"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        };
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    fn make_group_rule() -> RbacRule {
        RbacRule::from_patterns(HashMap::from([
            (Role::Anonymous, vec![]),
            (Role::User, vec!["releases:read".to_owned()]),
            (Role::Admin, vec!["*".to_owned()]),
        ]))
        .expect("fixture patterns are valid")
        .with_group_patterns(HashMap::from([
            (
                "team-a".to_owned(),
                vec!["releases:read".to_owned(), "source:read".to_owned()],
            ),
            ("team-b".to_owned(), vec!["releases:read".to_owned()]),
        ]))
        .expect("fixture patterns are valid")
    }

    fn make_meta() -> crate::entities::PackageMetadata {
        crate::entities::PackageMetadata {
            id: crate::entities::PackageId::new("github", "rust-lang/rust", "v1.80.0"),
            published_at: None,
            download_url: None,
            checksum: None,
            is_signed: None,
            extra: serde_json::Value::Null,
            cache_control: None,
        }
    }

    #[tokio::test]
    async fn group_member_can_access_group_registry() {
        let rule = make_group_rule();
        let identity = make_identity_with_groups(Role::Anonymous, vec!["team-a"]);
        let meta = make_meta();
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn non_group_member_cannot_access_group_only_resource() {
        let rule = make_group_rule();
        let identity = make_identity_with_groups(Role::Anonymous, vec!["team-b"]);
        let meta = make_meta();
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(
            rule.evaluate(&ctx).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn multi_group_member_sees_union_of_permissions() {
        let rule = make_group_rule();
        let identity = make_identity_with_groups(Role::Anonymous, vec!["team-b", "team-a"]);
        let meta = make_meta();
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    fn make_wildcard_rule() -> RbacRule {
        RbacRule::from_patterns(HashMap::from([
            (Role::Anonymous, vec![]),
            (Role::User, vec![]),
            (Role::Admin, vec!["*".to_owned()]),
        ]))
        .expect("fixture patterns are valid")
        .with_group_patterns(HashMap::from([
            // Wildcard entry: any provider's "team-a" group gets releases:read
            ("*:team-a".to_owned(), vec!["releases:read".to_owned()]),
            // Exact entry: only oidc2's "team-b" gets source:read
            ("oidc2:team-b".to_owned(), vec!["source:read".to_owned()]),
        ]))
        .expect("fixture patterns are valid")
    }

    #[tokio::test]
    async fn wildcard_prefix_matches_any_provider() {
        let rule = make_wildcard_rule();
        let meta = make_meta();
        for provider_group in &["oidc1:team-a", "oidc2:team-a", "kubernetes:team-a"] {
            let identity = make_identity_with_groups(Role::Anonymous, vec![provider_group]);
            let ctx = RuleContext {
                identity: &identity,
                package: &meta,
                action: Action::ReleasesRead,
                cache_entry: None,
                requested_version: None,
            };
            assert!(
                matches!(rule.evaluate(&ctx).await, RuleDecision::Allow),
                "{provider_group} should match wildcard *:team-a"
            );
        }
    }

    #[tokio::test]
    async fn exact_entry_does_not_match_wrong_provider() {
        let rule = make_wildcard_rule();
        // "oidc2:team-b" has source:read; "oidc1:team-b" should NOT match (no wildcard for team-b)
        let identity = make_identity_with_groups(Role::Anonymous, vec!["oidc1:team-b"]);
        let meta = make_meta();
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(
            rule.evaluate(&ctx).await,
            RuleDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn exact_entry_matches_correct_provider() {
        let rule = make_wildcard_rule();
        let identity = make_identity_with_groups(Role::Anonymous, vec!["oidc2:team-b"]);
        let meta = make_meta();
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        };
        assert!(matches!(rule.evaluate(&ctx).await, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn group_without_colon_does_not_panic_on_wildcard_lookup() {
        let rule = make_wildcard_rule();
        let identity = make_identity_with_groups(Role::Anonymous, vec!["no-prefix-group"]);
        let meta = make_meta();
        let ctx = RuleContext {
            identity: &identity,
            package: &meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        };
        // Should not match any entry and not panic
        assert!(matches!(
            rule.evaluate(&ctx).await,
            RuleDecision::Deny { .. }
        ));
    }
}
