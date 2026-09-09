//! What `[registries.raw]` does to a raw request (RFC 0019 §4.2 *Raw
//! content*, phase 3).
//!
//! Raw is **off unless written**: it was implicitly on for all three forges,
//! and an operator who relies on it says so. The refusal names the section,
//! because the alternative is a `403` that reads like a permissions problem
//! on a URL that worked yesterday.
//!
//! What this rule decides, before any byte is fetched:
//!
//! | Condition | Code |
//! | --- | --- |
//! | the section is absent or `enabled = false` | (no code — the section is named in the body) |
//! | the repository is outside `repos` | (same) |
//! | `require_pinned` and the ref is a branch | `PINNED_REF_REQUIRED` |
//! | the path names a script and `scripts` is not `ignore` | `RAW_SCRIPT` |
//!
//! The size ceiling is enforced by the read path, which is where the bytes
//! are, and the *shebang* half of the script check is read there too from the
//! first chunk — a payload with no telling extension is still refused under
//! `scripts = "deny"`.

use async_trait::async_trait;

use crate::entities::{ForgeCoordinate, ForgeKind, RawPolicy, RefKind, ScriptAction};
use crate::rules::{Rule, RuleContext, RuleDecision};

pub const RAW_GATE: &str = "raw";

pub struct RawPolicyRule {
    pub policy: RawPolicy,
    /// Whether the registry has a `[registries.security]` profile, and so a
    /// per-request verdict for a `warn` to ride on.
    pub carries_verdict: bool,
}

impl RawPolicyRule {
    pub fn new(policy: RawPolicy) -> Self {
        Self {
            policy,
            carries_verdict: false,
        }
    }

    pub fn carrying_verdict(mut self) -> Self {
        self.carries_verdict = true;
        self
    }
}

#[async_trait]
impl Rule for RawPolicyRule {
    fn name(&self) -> &str {
        RAW_GATE
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let Some(coord) = ForgeCoordinate::from_package_id(&ctx.package.id) else {
            return RuleDecision::Allow;
        };
        let ForgeKind::Raw { path, .. } = &coord.kind else {
            return RuleDecision::Allow;
        };
        if !self.policy.enabled {
            return RuleDecision::Deny {
                reason: format!(
                    "raw content is not served by this registry: add [registries.raw] with \
                     enabled = true to '{}' to turn it on (RFC 0019 §4.1)",
                    ctx.package.id.registry
                ),
            };
        }
        if !self.policy.allows_repo(&coord.owner_repo) {
            return RuleDecision::Deny {
                reason: format!(
                    "raw content from '{}' is not in this registry's raw.repos allowlist",
                    coord.owner_repo
                ),
            };
        }

        let ref_kind = ctx
            .package
            .extra
            .get(crate::entities::FORGE_EXTRA_KEY)
            .and_then(|f| f.get(crate::entities::FORGE_REF_KIND))
            .and_then(|v| v.as_str())
            .and_then(|k| k.parse::<RefKind>().ok());
        let mut findings = Vec::new();
        let mut denial: Option<String> = None;

        if self.policy.require_pinned && ref_kind == Some(RefKind::Branch) {
            let message = format!(
                "PINNED_REF_REQUIRED: raw content on '{}' must be addressed by a tag or a \
                 commit, not by a branch",
                ctx.package.id.registry
            );
            denial = Some(message.clone());
            findings.push(finding(
                crate::entities::ReasonCode::PinnedRefRequired,
                crate::entities::Severity::Critical,
                message,
            ));
        }

        if self.policy.scripts != ScriptAction::Ignore && RawPolicy::looks_like_script(path) {
            let deny = self.policy.scripts == ScriptAction::Deny;
            let message = format!(
                "RAW_SCRIPT: '{path}' is a script; this registry's raw.scripts is \"{}\"",
                self.policy.scripts.as_str()
            );
            if deny && denial.is_none() {
                denial = Some(message.clone());
            }
            findings.push(finding(
                crate::entities::ReasonCode::RawScript,
                if deny {
                    crate::entities::Severity::Critical
                } else {
                    crate::entities::Severity::Low
                },
                message,
            ));
        }

        crate::services::verdict::augment_request_verdict(
            &ctx.package.id,
            &format!("{}/raw", ctx.package.id.registry),
            findings,
            denial.is_some(),
            self.carries_verdict,
            chrono::Utc::now(),
        );
        match denial {
            Some(reason) => RuleDecision::Deny { reason },
            None => RuleDecision::Allow,
        }
    }
}

fn finding(
    code: crate::entities::ReasonCode,
    severity: crate::entities::Severity,
    summary: String,
) -> crate::entities::Finding {
    crate::entities::Finding::new(
        RAW_GATE,
        crate::entities::FindingKind::Ref,
        code,
        severity,
        summary,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{Action, Identity, PackageId, PackageMetadata, FORGE_EXTRA_KEY};

    fn meta(path: &str, ref_kind: &str) -> PackageMetadata {
        let mut m = PackageMetadata::minimal(
            PackageId::new("gh", "cli/cli", "main").with_artifact(format!("raw/{path}")),
            serde_json::Value::Null,
        );
        m.extra = serde_json::json!({ FORGE_EXTRA_KEY: { "ref_kind": ref_kind } });
        m
    }

    async fn decide(rule: &RawPolicyRule, m: &PackageMetadata) -> RuleDecision {
        let identity = Identity::anonymous();
        rule.evaluate(&RuleContext {
            identity: &identity,
            package: m,
            action: Action::SourceRead,
            cache_entry: None,
            requested_version: None,
        })
        .await
    }

    fn on() -> RawPolicy {
        RawPolicy {
            enabled: true,
            ..RawPolicy::default()
        }
    }

    #[tokio::test]
    async fn raw_is_refused_until_the_section_turns_it_on() {
        let off = RawPolicyRule::new(RawPolicy::default());
        let RuleDecision::Deny { reason } = decide(&off, &meta("README.md", "tag")).await else {
            panic!("raw is off unless written");
        };
        assert!(reason.contains("[registries.raw]"), "{reason}");

        let rule = RawPolicyRule::new(on());
        assert!(matches!(
            decide(&rule, &meta("README.md", "tag")).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn the_allowlist_narrows_and_never_widens() {
        let rule = RawPolicyRule::new(RawPolicy {
            repos: vec!["cli/*".into()],
            ..on()
        });
        assert!(matches!(
            decide(&rule, &meta("README.md", "tag")).await,
            RuleDecision::Allow
        ));
        let mut other = meta("README.md", "tag");
        other.id = PackageId::new("gh", "evil/repo", "main").with_artifact("raw/README.md");
        let RuleDecision::Deny { reason } = decide(&rule, &other).await else {
            panic!("a repository outside the allowlist is refused");
        };
        assert!(reason.contains("raw.repos"), "{reason}");
    }

    #[tokio::test]
    async fn a_branch_is_refused_when_raw_must_be_pinned() {
        let rule = RawPolicyRule::new(RawPolicy {
            require_pinned: true,
            ..on()
        });
        assert!(matches!(
            decide(&rule, &meta("README.md", "tag")).await,
            RuleDecision::Allow
        ));
        let RuleDecision::Deny { reason } = decide(&rule, &meta("README.md", "branch")).await
        else {
            panic!("a branch is refused under require_pinned");
        };
        assert!(reason.contains("PINNED_REF_REQUIRED"), "{reason}");
    }

    #[tokio::test]
    async fn a_script_is_warned_by_default_and_refused_under_deny() {
        let warn = RawPolicyRule::new(on());
        assert!(
            matches!(
                decide(&warn, &meta("install.sh", "tag")).await,
                RuleDecision::Allow
            ),
            "warn serves it"
        );
        let deny = RawPolicyRule::new(RawPolicy {
            scripts: ScriptAction::Deny,
            ..on()
        });
        let RuleDecision::Deny { reason } = decide(&deny, &meta("install.sh", "tag")).await else {
            panic!("deny refuses a script");
        };
        assert!(reason.contains("RAW_SCRIPT"), "{reason}");
        // Every extension §4.2 names, and nothing else.
        for name in ["get.bash", "setup.ps1", "boot.py", "run.bat", "x.cmd"] {
            assert!(RawPolicy::looks_like_script(name), "{name}");
        }
        for name in ["README.md", "config.yaml", "shell.nix", "a.sha256"] {
            assert!(!RawPolicy::looks_like_script(name), "{name}");
        }
        // Ignore looks at nothing.
        let ignore = RawPolicyRule::new(RawPolicy {
            scripts: ScriptAction::Ignore,
            ..on()
        });
        assert!(matches!(
            decide(&ignore, &meta("install.sh", "tag")).await,
            RuleDecision::Allow
        ));
    }

    #[test]
    fn a_shebang_is_a_script_whatever_the_name() {
        assert!(RawPolicy::starts_like_script(
            b"#!/usr/bin/env bash\necho hi"
        ));
        assert!(RawPolicy::starts_like_script(b"@echo off\r\n"));
        assert!(!RawPolicy::starts_like_script(b"# A markdown heading\n"));
        assert!(!RawPolicy::starts_like_script(&[]));
    }
}
