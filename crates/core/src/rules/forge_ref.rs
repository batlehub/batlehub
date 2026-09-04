//! What a forge ref does to a request (RFC 0019 §4.2, phase 2).
//!
//! Three facts about a coordinate, each with an operator-chosen action:
//!
//! | Fact | Code | Default |
//! | --- | --- | --- |
//! | the request followed a branch | `MUTABLE_REF` | `warn` |
//! | a tag resolves to a different commit than it did | `TAG_MOVED` | `deny` |
//! | a release asset's digest changed since the bytes were cached | `ASSET_REPLACED` | `deny` |
//!
//! # Two paths, one detection
//!
//! On a registry with `[registries.security]` the same facts are produced as
//! findings by [`crate::services::ForgeRefScanner`] and judged with the rest
//! of the verdict — that is where a `warn` becomes a `warned` response the
//! client can read. On a forge registry without one this rule runs in the
//! chain: the `deny` actions refuse, and a `warn` is carried by the
//! `X-BatleHub-Ref-Kind` / `X-BatleHub-Ref-Previous-Commit` headers alone,
//! because there is no verdict to attach it to. That is the honest
//! degradation the RFC names, and the registry page says so.
//!
//! # Why the detection reads metadata rather than the resolver
//!
//! `ProxyService` resolves the ref before the rules run and merges the answer
//! into `extra.forge` (§4.2 *Metadata contract*). Reading it back here keeps
//! one resolution per request — a rule that resolved again would double every
//! forge API call and could disagree with the commit that was actually
//! served.

use std::sync::Arc;

use async_trait::async_trait;

use crate::entities::ForgeRefsPolicy;
use crate::ports::ArtifactCacheMeta;
use crate::rules::{Rule, RuleContext, RuleDecision};
use crate::services::forge_refs::{ref_findings, RefFinding};

pub const FORGE_REF_GATE: &str = "forge_ref";

pub struct ForgeRefRule {
    pub policy: ForgeRefsPolicy,
    /// Where the digest of the bytes already cached under this coordinate
    /// lives. `None` disables `ASSET_REPLACED` only — nothing else needs it,
    /// and a deployment without the store still refuses a moved tag.
    pub artifact_meta: Option<Arc<dyn ArtifactCacheMeta>>,
    /// Whether this registry has a `[registries.security]` profile, and so a
    /// per-request verdict for these findings to ride on. Without one a
    /// refusal is a plain `403` and a warn is the ref headers alone — the
    /// degradation RFC 0019 §6.1 names, rather than a verdict invented for a
    /// registry that opted into none.
    pub carries_verdict: bool,
}

impl ForgeRefRule {
    pub fn new(policy: ForgeRefsPolicy) -> Self {
        Self {
            policy,
            artifact_meta: None,
            carries_verdict: false,
        }
    }

    /// On a registry with `[registries.security]`: the findings ride the
    /// request's verdict, so a warned ref reaches the client in
    /// `X-BatleHub-Verdict` / `X-BatleHub-Reason` beside everything else the
    /// verdict says.
    pub fn carrying_verdict(mut self) -> Self {
        self.carries_verdict = true;
        self
    }

    pub fn with_artifact_meta(mut self, meta: Arc<dyn ArtifactCacheMeta>) -> Self {
        self.artifact_meta = Some(meta);
        self
    }

    /// The recorded digest of what is already cached under the coordinate,
    /// bare hex as the streaming store writes it.
    pub(crate) async fn cached_digest(&self, ctx: &RuleContext<'_>) -> Option<String> {
        let meta = self.artifact_meta.as_ref()?;
        let key = format!("artifact:{}", ctx.package.id.cache_key());
        match meta.get_artifact_checksum(&key).await {
            Ok(d) => d,
            Err(e) => {
                // No digest is "not seen before", which is trusted — the same
                // reading a first fetch gets. A store blip must not invent a
                // replacement that did not happen.
                tracing::warn!(key = %key, error = %e, "forge_ref: could not read the cached digest");
                None
            }
        }
    }
}

#[async_trait]
impl Rule for ForgeRefRule {
    fn name(&self) -> &str {
        FORGE_REF_GATE
    }

    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision {
        let cached = self.cached_digest(ctx).await;
        let findings: Vec<RefFinding> = ref_findings(self.policy, ctx.package, cached.as_deref());
        if findings.is_empty() {
            return RuleDecision::Allow;
        }
        let denial = findings.iter().find(|f| f.denies()).cloned();
        crate::services::verdict::augment_request_verdict(
            &ctx.package.id,
            &format!("{}/refs", ctx.package.id.registry),
            findings
                .into_iter()
                .map(|f| {
                    crate::entities::Finding::new(
                        FORGE_REF_GATE,
                        crate::entities::FindingKind::Ref,
                        f.code,
                        f.severity(),
                        f.message,
                    )
                })
                .collect(),
            denial.is_some(),
            self.carries_verdict,
            chrono::Utc::now(),
        );
        match denial {
            Some(f) => RuleDecision::Deny { reason: f.message },
            None => RuleDecision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{
        Action, Identity, PackageId, PackageMetadata, RefAction, Role, FORGE_EXTRA_KEY,
    };

    fn policy(mutable: RefAction, moved: RefAction) -> ForgeRefsPolicy {
        ForgeRefsPolicy {
            mutable_refs: mutable,
            tag_moved: moved,
            ..ForgeRefsPolicy::default()
        }
    }

    fn metadata(kind: &str, previous: Option<&str>) -> PackageMetadata {
        let mut m = PackageMetadata::minimal(
            PackageId::new("gh", "cli/cli", "main").with_artifact("tarball/main"),
            serde_json::Value::Null,
        );
        m.extra = serde_json::json!({
            FORGE_EXTRA_KEY: {
                "ref_kind": kind,
                "resolved_commit": "a".repeat(40),
                "previous_commit": previous,
            }
        });
        m
    }

    async fn decide(rule: &ForgeRefRule, meta: &PackageMetadata) -> RuleDecision {
        let identity = Identity::anonymous();
        rule.evaluate(&RuleContext {
            identity: &identity,
            package: meta,
            action: Action::ReleasesRead,
            cache_entry: None,
            requested_version: None,
        })
        .await
    }

    #[tokio::test]
    async fn a_branch_is_warned_by_default_and_denied_when_configured() {
        let meta = metadata("branch", None);
        let warn = ForgeRefRule::new(policy(RefAction::Warn, RefAction::Deny));
        assert!(matches!(decide(&warn, &meta).await, RuleDecision::Allow));

        let deny = ForgeRefRule::new(policy(RefAction::Deny, RefAction::Deny));
        let RuleDecision::Deny { reason } = decide(&deny, &meta).await else {
            panic!("a branch must be refused under mutable_refs = deny");
        };
        assert!(reason.contains("MUTABLE_REF"), "{reason}");
    }

    #[tokio::test]
    async fn a_moved_tag_is_denied_by_default_and_a_first_sight_tag_is_not() {
        let rule = ForgeRefRule::new(policy(RefAction::Warn, RefAction::Deny));
        // Never resolved before: nothing to compare, so nothing is refused.
        assert!(matches!(
            decide(&rule, &metadata("tag", None)).await,
            RuleDecision::Allow
        ));

        let moved = metadata("tag", Some(&"b".repeat(40)));
        let RuleDecision::Deny { reason } = decide(&rule, &moved).await else {
            panic!("a moved tag is denied by default");
        };
        assert!(reason.contains("TAG_MOVED"), "{reason}");
        assert!(
            reason.contains(&"b".repeat(40)),
            "names the previous commit: {reason}"
        );

        // The operator who prefers to be told rather than blocked.
        let warn = ForgeRefRule::new(policy(RefAction::Warn, RefAction::Warn));
        assert!(matches!(decide(&warn, &moved).await, RuleDecision::Allow));
    }

    #[tokio::test]
    async fn an_advanced_branch_is_not_a_moved_tag() {
        // A branch whose commit changed is the normal case, not a finding of
        // its own: `previous_commit` is set on every branch that advanced.
        let rule = ForgeRefRule::new(policy(RefAction::Warn, RefAction::Deny));
        let advanced = metadata("branch", Some(&"c".repeat(40)));
        assert!(matches!(
            decide(&rule, &advanced).await,
            RuleDecision::Allow
        ));
    }

    #[tokio::test]
    async fn a_non_forge_coordinate_is_left_alone() {
        let rule = ForgeRefRule::new(policy(RefAction::Deny, RefAction::Deny));
        let meta = PackageMetadata::minimal(
            PackageId::new("npm", "left-pad", "1.3.1"),
            serde_json::Value::Null,
        );
        assert!(matches!(decide(&rule, &meta).await, RuleDecision::Allow));
        let _ = Role::Anonymous;
    }
}
