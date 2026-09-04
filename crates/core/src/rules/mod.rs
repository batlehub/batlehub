pub mod block_list;
pub mod cve_gate;
pub mod deny_latest;
pub mod flags;
pub mod forge_ref;
pub mod license_gate;
pub mod raw_policy;
pub mod rbac;
pub mod release_age;
pub mod signed_release;
pub mod trusted_publisher;
pub mod verdict_gate;
pub mod version_gate;

pub use block_list::BlockListRule;
pub use cve_gate::CveGateRule;
pub use deny_latest::DenyLatestRule;
pub use flags::{FlagsRule, FLAGS_GATE};
pub use forge_ref::{ForgeRefRule, FORGE_REF_GATE};
pub use license_gate::LicenseGateRule;
pub use raw_policy::{RawPolicyRule, RAW_GATE};
pub use rbac::RbacRule;
pub use release_age::ReleaseAgeGateRule;
pub use signed_release::RequireSignedReleaseRule;
pub use trusted_publisher::TrustedPublisherRule;
pub use verdict_gate::VerdictGateRule;
pub use version_gate::VersionGateRule;

use async_trait::async_trait;

use crate::entities::{Action, Identity, PackageMetadata};
use crate::ports::CacheEntry;

pub struct RuleContext<'a> {
    /// The caller making the request.
    pub identity: &'a Identity,
    /// Resolved package metadata from the upstream registry.
    pub package: &'a PackageMetadata,
    /// The operation being requested.
    ///
    /// A closed enum since RFC 0015 phase 1: this was `&'a str`, and a typo in
    /// it asked for a permission nothing grants and refused every caller for a
    /// reason no log explained.
    pub action: Action,
    /// Cached metadata entry, if one exists. Absent on the first request for a package.
    pub cache_entry: Option<&'a CacheEntry>,
    /// The version string from the original request, before upstream resolution.
    /// For example `"latest"` even if the upstream resolved it to `"1.2.3"`.
    pub requested_version: Option<&'a str>,
}

/// A rule's verdict.
///
/// # Why there is no third variant
///
/// There used to be `RequireRole { minimum }`, which meant "allow this if the
/// caller is privileged enough" and left the comparison to whoever received it.
/// The operand was never missing — `RuleContext` carries the identity, and every
/// rule that produced a `RequireRole` was holding the very thing needed to
/// resolve it. What the extra variant bought was nothing; what it cost was a
/// verdict that reads as *not a denial* until someone remembers to call
/// `.resolve()`.
///
/// That cost was paid. The 2026-08-26 remediation review found two call sites in
/// `authz.rs` that matched on `Deny` alone, so every gate with a
/// non-empty `bypass_roles` — `version_gate`, `deny_latest`, `trusted_publisher`
/// — silently became a no-op the moment an operator named a bypass role: the
/// rule answered `RequireRole`, the caller saw "not a `Deny`", and the blocked
/// version was served to everyone.
///
/// RFC 0015 §5.1 deletes it. Rules now compare against `ctx.identity` and answer
/// `Allow` or `Deny`, so there is no unresolved state for a caller to
/// misread — the fix is structural rather than two more `.resolve()` calls that
/// the next caller can also forget.
#[derive(Debug, Clone)]
pub enum RuleDecision {
    /// The request is permitted.
    Allow,
    /// The request is rejected with a human-readable reason.
    Deny { reason: String },
}

impl RuleDecision {
    pub fn is_deny(&self) -> bool {
        matches!(self, RuleDecision::Deny { .. })
    }
}

/// A single rule in the evaluation pipeline.
#[async_trait]
pub trait Rule: Send + Sync {
    /// Short identifier used in log messages (e.g. `"block_list"`, `"rbac"`).
    fn name(&self) -> &str;

    /// Evaluate the rule against `ctx`. Rules are called in order; the first
    /// `Deny` short-circuits the chain.
    async fn evaluate(&self, ctx: &RuleContext<'_>) -> RuleDecision;
}

/// Evaluate a list of rules in order. Returns the first `Deny`, or `Allow`.
///
/// An empty rule list allows: tests and library consumers rely on this to
/// exercise `ProxyService` without RBAC noise. The real invariant this
/// depends on — that `build_policy` always puts `RbacRule` first for every
/// configured registry, so production policies are never actually empty — is
/// guarded by `build_policy_default_has_rbac_and_block_list_rules` in
/// `server/src/builders.rs`.
pub async fn evaluate_rules(rules: &[Box<dyn Rule>], ctx: &RuleContext<'_>) -> RuleDecision {
    for rule in rules {
        let decision = rule.evaluate(ctx).await;
        if decision.is_deny() {
            return decision;
        }
    }
    RuleDecision::Allow
}
