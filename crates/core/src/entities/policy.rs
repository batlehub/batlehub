//! Policy on the resource hierarchy — RFC 0015 §4.1.
//!
//! Phase 3 put *grants* on the four tiers. This puts everything else there:
//! `visibility`, `versioning`, `quota` and `rules`, plus the hook RFC 0016's
//! `retention` attaches to.
//!
//! # The composition rule is not the same for every policy
//!
//! Pretending otherwise would be the trap, and §4.1 spends a table on it. Each
//! rule is chosen so that the **mistake fails in the recoverable direction**,
//! and the recoverable direction differs by what the policy does:
//!
//! | Policy | Composition | Why |
//! | --- | --- | --- |
//! | `grants` | additive — union over the path | Fewer permissions is the safe direction, so a union of what matched fails closed when nothing does. |
//! | `visibility` | deepest wins | A single value; there is nothing to merge. |
//! | `versioning` | deepest wins, **wholesale** | See below. |
//! | `quota` | deepest wins, wholesale | Same reasoning. |
//! | `retention` | deepest wins, wholesale | Same reasoning. RFC 0016's block. |
//! | `rules` | deepest wins, **per rule** | Each gate is independently configured. |
//!
//! `grants` is resolved by [`resolve`](super::resolve) and is not here — this
//! module is the other five.
//!
//! ## Why `versioning` and `quota` are wholesale and `rules` is not
//!
//! The motivating case for `versioning` and `quota` is a **narrower** policy on
//! a deeper tier: the one package in an ordinary namespace that publishes a 2 GB
//! artifact per CI run, or the one that follows a different release convention.
//! A per-field merge cannot express that — an inherited constraint could never
//! be *dropped*, only tightened, so a deeper tier could only ever keep more.
//! Wholesale is also greppable: what you see on the node is what runs.
//!
//! `rules` is the opposite, because each gate is configured independently. A
//! wholesale override would force an operator to redeclare `cve_gate` and
//! `license_gate` in order to change `release_age`, and a forgotten one is a
//! gate silently switched off — the fail-open direction, which is the one this
//! model refuses to make easy.
//!
//! ## The sharp edge, and what compensates
//!
//! Wholesale composition means a deeper block that omits a constraint its parent
//! declared **drops** that constraint. That is the point, and it is also the
//! edit most likely to be a mistake: narrowing is precisely what reclaims
//! something someone was relying on, or accepts a version its namespace would
//! have refused. So it is a warning on every reload rather than a silent
//! success — see [`PolicyPath::narrowing_warnings`].

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Tier, Visibility};

/// Whether published bytes may be replaced — RFC 0015 §4.5.
///
/// # Why this is not a verb
///
/// **Immutability is a property of the resource, the verb is a property of the
/// subject, and a replace needs both.** That split is what lets a namespace be
/// append-only for *everyone, including admins*, which no role-based model can
/// say. Adding a `releases:overwrite`-style exception here would make it a rule
/// an admin can step over, and a rule an admin can step over is not an
/// invariant.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Immutable {
    /// Any version may be replaced by a caller holding `releases:overwrite`.
    ///
    /// The default, and **not** because it is the best policy — §4.5 says
    /// `released` is what most estates want. It is the default because nothing
    /// enforces immutability today, so any other value would change the meaning
    /// of an existing config, which §10 rule 8 forbids.
    #[default]
    Never,
    /// A release is immutable; a pre-release may be replaced.
    ///
    /// The Maven shape — SNAPSHOT churns, releases do not. "Pre-release" here is
    /// [`version_order::is_prerelease`](crate::services::version_order::is_prerelease),
    /// the one definition; before phase 4 converged it, the rule in use called
    /// `1.0-SNAPSHOT` a release and would have frozen exactly the versions this
    /// value exists to let churn.
    Released,
    /// No version may ever be replaced; `releases:overwrite` grants nothing here.
    Always,
}

impl Immutable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Released => "released",
            Self::Always => "always",
        }
    }
}

impl std::fmt::Display for Immutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Immutable {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "never" => Ok(Self::Never),
            "released" => Ok(Self::Released),
            "always" => Ok(Self::Always),
            other => Err(format!("unknown immutable value: '{other}'")),
        }
    }
}

/// One tier's declared policy. Every field is `Option`, and the distinction is
/// load-bearing: `None` **inherits**, `Some` **overrides**.
///
/// The same rule §4.3 states for grants — "absence is not everything" — applies
/// here in its twin form: absence is not *nothing* either. A node that declared
/// no `versioning` must run its parent's, not an empty one.
/// No `Default`, because [`Tier`] has none: there is no neutral tier, and a
/// node that defaulted to one would silently claim a position in the hierarchy.
/// Build with [`PolicyNode::new`].
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyNode {
    pub tier: Tier,
    /// How this node is addressed, for diagnostics and for
    /// [`PolicyPath::narrowing_warnings`]. `registry:npm1`, `namespace:@acme`,
    /// `package:@acme/cards`, `version:1.4.2`.
    pub key: String,
    pub visibility: Option<Visibility>,
    pub prerelease_visibility: Option<Visibility>,
    pub versioning: Option<VersioningRules>,
    pub quota: Option<QuotaRules>,
    /// Gate overrides, keyed by the gate's own name (`release_age`,
    /// `cve_gate`, …). Composes per rule, so this is a map rather than a list.
    pub rules: Vec<RuleOverride>,
}

impl PolicyNode {
    pub fn new(tier: Tier, key: impl Into<String>) -> Self {
        Self {
            tier,
            key: key.into(),
            visibility: None,
            prerelease_visibility: None,
            versioning: None,
            quota: None,
            rules: Vec::new(),
        }
    }
}

/// What a version may be called here, and whether it may change (§4.5).
///
/// The compiled `version_pattern` lives on the runtime side
/// (`services::hot_config::VersioningPolicy`); this is the resolved *policy*,
/// carried as the source string so a node is comparable and printable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersioningRules {
    pub enforce_semver: bool,
    pub allow_prerelease: bool,
    pub version_pattern: Option<String>,
    pub immutable: Immutable,
    pub monotonic: bool,
    /// RFC 0015 §4.7 — evaluate fully, record what would have been refused,
    /// refuse nothing.
    ///
    /// §4.7's table calls this direction **mixed**: a badly-named or duplicate
    /// version is accepted, so bad data lands, but nothing leaks. That is why it
    /// carries no expiry requirement while `grants_shadow` does — forgetting
    /// this costs a messy registry, where forgetting the other is an
    /// authorization bypass.
    pub dry_run: bool,
}

impl VersioningRules {
    /// The naming half — what a version may be *called*.
    ///
    /// §4.1: these fields are meaningless at version tier, where the name
    /// already exists and `enforce_semver` on `1.4.0` has nothing left to
    /// decide. Config load rejects them there rather than silently ignoring
    /// them, and this is the list it rejects.
    pub fn declares_naming_fields(&self) -> bool {
        self.enforce_semver
            || !self.allow_prerelease
            || self.version_pattern.is_some()
            || self.monotonic
    }
}

/// How much may be published here (§4.5).
///
/// Stops at the package tier: a per-version quota would limit a thing published
/// exactly once, which has nothing to constrain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaRules {
    pub max_bytes_per_user: Option<u64>,
    pub max_packages_per_user: Option<u32>,
    pub warn_threshold_pct: Option<u8>,
    pub block: bool,
}

/// One gate's override at one tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOverride {
    /// The gate's name, as `RuleConfig`'s `kind` tag spells it.
    pub gate: String,
    /// The override's settings, opaque here: this module composes overrides, it
    /// does not interpret them. The rule that consumes one knows its own shape.
    pub settings: serde_json::Value,
}

/// The tiers a coordinate resolves through, registry-first.
///
/// Built by the caller — the same shape `resolve` takes for grants, and
/// deliberately the same order, so a reader who has understood one has
/// understood the other. Several namespace nodes may match one package (§4.1),
/// and for the deepest-wins policies here **the last matching one wins**, which
/// is config order: an operator listing `@acme` then `@acme/billing` gets the
/// more specific answer from the more specific block, because they wrote it
/// second.
#[derive(Debug, Clone, Default)]
pub struct PolicyPath {
    pub nodes: Vec<PolicyNode>,
}

/// Everything that applies to one coordinate, after composition.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedPolicy {
    pub visibility: Visibility,
    /// The audience for a pre-release, which is `visibility` when no tier
    /// declared one — a pre-release is not narrower by default.
    pub prerelease_visibility: Visibility,
    pub versioning: VersioningRules,
    pub quota: Option<QuotaRules>,
    /// Gate overrides, merged per rule across the path.
    pub rules: Vec<RuleOverride>,
    /// Which node supplied each answer, for `explain` (§4.8) and for the
    /// narrowing warnings. Empty entries mean "the default", not "the registry".
    pub sources: PolicySources,
    /// §4.1's compensating warnings: deeper tiers that **dropped** a constraint
    /// their parent declared.
    ///
    /// Carried on the result rather than left to the caller to recompute,
    /// because the [`PolicyPath`] is consumed by [`PolicyPath::resolve`] and
    /// nothing outside this module ever holds one — which is precisely how
    /// [`PolicyPath::narrowing_warnings`] came to have no caller at all between
    /// the day it was written and 2026-09-01.
    ///
    /// `(node_key, what_was_dropped)` pairs, in path order.
    pub narrowing: Vec<(String, String)>,
}

/// Which tier each answer came from.
///
/// `explain` has to be able to say *why*, and "visibility is team" is not an
/// answer to that — "visibility is team, from the namespace `@acme/billing`" is.
/// Carried on the result rather than recomputed, because recomputing it would
/// mean a second implementation of the composition rules and the two would
/// drift, which is the defect this whole document is about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicySources {
    pub visibility: Option<String>,
    pub prerelease_visibility: Option<String>,
    pub versioning: Option<String>,
    pub quota: Option<String>,
    /// Gate name → the node that last set it.
    pub rules: Vec<(String, String)>,
}

impl PolicyPath {
    pub fn new(nodes: Vec<PolicyNode>) -> Self {
        Self { nodes }
    }

    /// Compose the path into one answer, by §4.1's per-policy rules.
    pub fn resolve(&self) -> ResolvedPolicy {
        let mut out = ResolvedPolicy {
            narrowing: self.narrowing_warnings(),
            ..Default::default()
        };
        let mut prerelease_declared = false;

        for node in &self.nodes {
            // ── deepest wins, single value ───────────────────────────────────
            if let Some(v) = node.visibility {
                out.visibility = v;
                out.sources.visibility = Some(node.key.clone());
            }
            if let Some(v) = node.prerelease_visibility {
                out.prerelease_visibility = v;
                out.sources.prerelease_visibility = Some(node.key.clone());
                prerelease_declared = true;
            }

            // ── deepest wins, wholesale ──────────────────────────────────────
            //
            // The whole block is replaced, not merged field by field. A deeper
            // node that omits `enforce_semver` drops it.
            if let Some(v) = &node.versioning {
                out.versioning = v.clone();
                out.sources.versioning = Some(node.key.clone());
            }
            if let Some(q) = &node.quota {
                out.quota = Some(q.clone());
                out.sources.quota = Some(node.key.clone());
            }

            // ── deepest wins, per rule ───────────────────────────────────────
            Self::apply_rule_overrides(node, &mut out);
        }

        // A pre-release is not a narrower audience by default; it is the same
        // audience unless someone said otherwise. Applied after the walk so a
        // `visibility` set *deeper* than the last `prerelease_visibility` still
        // carries — which is the reading that surprises nobody: setting a
        // package to `team` should not leave its pre-releases public.
        if !prerelease_declared {
            out.prerelease_visibility = out.visibility;
        }

        out
    }

    /// Merge one node's rule overrides into `out`, gate by gate.
    ///
    /// A node overriding `release_age` leaves `cve_gate` alone. This is the one
    /// policy where a deeper block does *not* replace what it does not mention,
    /// and §4.1 gives the reason: a wholesale rule override would make a
    /// forgotten gate a silently disabled one.
    fn apply_rule_overrides(node: &PolicyNode, out: &mut ResolvedPolicy) {
        for over in &node.rules {
            match out.rules.iter_mut().find(|r| r.gate == over.gate) {
                Some(existing) => *existing = over.clone(),
                None => out.rules.push(over.clone()),
            }
            match out.sources.rules.iter_mut().find(|(g, _)| *g == over.gate) {
                Some(slot) => slot.1 = node.key.clone(),
                None => out
                    .sources
                    .rules
                    .push((over.gate.clone(), node.key.clone())),
            }
        }
    }

    /// §4.1's compensating warning: a deeper tier that **drops** a constraint
    /// its parent declared.
    ///
    /// Wholesale composition is what makes a narrower policy on a deeper tier
    /// expressible, and it is also what makes dropping one silent. This is the
    /// edit most likely to be a mistake in the direction that matters:
    /// narrowing reclaims something someone was relying on, or accepts a version
    /// its namespace would have refused.
    ///
    /// Returns `(node_key, what_was_dropped)` pairs, in path order.
    pub fn narrowing_warnings(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut inherited: Option<&VersioningRules> = None;
        let mut inherited_from = "";

        for node in &self.nodes {
            let Some(here) = &node.versioning else {
                continue;
            };
            if let Some(parent) = inherited {
                Self::dropped_constraints(parent, here, inherited_from, &node.key, &mut out);
            }
            inherited = Some(here);
            inherited_from = &node.key;
        }
        out
    }

    /// Everything `here` stops enforcing that `parent` declared, as
    /// `(node_key, what_was_dropped)` pairs appended to `out`.
    fn dropped_constraints(
        parent: &VersioningRules,
        here: &VersioningRules,
        inherited_from: &str,
        key: &str,
        out: &mut Vec<(String, String)>,
    ) {
        for (dropped, held) in [
            (
                "enforce_semver",
                parent.enforce_semver && !here.enforce_semver,
            ),
            ("monotonic", parent.monotonic && !here.monotonic),
            (
                "version_pattern",
                parent.version_pattern.is_some() && here.version_pattern.is_none(),
            ),
            (
                "allow_prerelease = false",
                !parent.allow_prerelease && here.allow_prerelease,
            ),
        ] {
            if held {
                out.push((
                    key.to_owned(),
                    format!(
                        "drops `{dropped}`, which `{inherited_from}` declares. \
                         `versioning` composes wholesale, so this node's block replaces \
                         its parent's entirely rather than adding to it — if the \
                         constraint was meant to keep applying, restate it here."
                    ),
                ));
            }
        }

        // Immutability is an ordering rather than a set of flags, so it is
        // checked as one: `always` is stricter than `released`, which is
        // stricter than `never`.
        if here.immutable < parent.immutable {
            out.push((
                key.to_owned(),
                format!(
                    "relaxes `immutable` from `{}` to `{}`, which `{inherited_from}` \
                     declares. Versions frozen by the parent become replaceable here.",
                    parent.immutable.as_str(),
                    here.immutable.as_str()
                ),
            ));
        }
    }
}

/// The gates an exemption may be written on — RFC 0015 §4.5.
///
/// **Two, and the line is not arbitrary.** An exemptible gate reports an
/// *assessable finding*; a non-exemptible one establishes an *invariant*:
///
/// - `cve_gate` reports a finding a human can assess. "The affected path is
///   unreachable from our usage" is a real judgement about a real fact, and the
///   fact stays true — what is accepted is the risk.
/// - `license_gate` is the same shape: counsel approves a licence for one
///   dependency, the declaration is unchanged, the decision is recorded.
///
/// Everything else is on the other side of that sentence. `release_age`
/// establishes an invariant, and a quarantine a version can skip is not a
/// quarantine — the value is entirely in its uniformity. An unsigned artifact is
/// not a finding to assess, it is an **absence of evidence**, so
/// `require_signed_release` and `trusted_publisher` have nothing to reason
/// about. `block_list` is an admin's decision, and a namespace owner exempting
/// it would be undoing someone else's authority from below. `deny_latest` and
/// `version_gate` judge the *request* or the *name* rather than the artifact, so
/// there is no finding for a per-version exemption to attach to.
///
/// A future gate is exemptible only if it falls on the first side of that
/// sentence, and **adding it here is the decision** — not a config value someone
/// can set.
///
/// `security_verdict` (RFC 0018 §4.1) is the one addition: an exemption on it
/// overrides a verdict, and the service folds it in as `ADMIN_OVERRIDE` — the
/// result is `warned`, never `allowed`, so the override stays visible.
///
/// `flags` (RFC 0002 §13 decision 4): an exemption on it silences a pushed
/// flag on one version — on a `[security]` registry through the verdict, on
/// any other through `FlagsRule`. It replaces the suppress endpoint and the
/// role bypass RFC 0002 first designed: one mechanism, one audit trail.
pub const EXEMPTIBLE_GATES: &[&str] = &["cve_gate", "license_gate", "security_verdict", "flags"];

/// A gate exemption, as it is written on a version-tier node.
///
/// `exempt_until` and `reason` are both **required**, and the discipline is the
/// same one `grants.dry_run` carries in §4.7 and for the same reason: the
/// realistic failure is not a wrong assessment, it is a right assessment nobody
/// revisited. An exemption is audited on creation, surfaced beside the finding it
/// silences, and expires on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GateExemption {
    /// Always `true`, and **not redundant with the entry's existence**.
    ///
    /// §4.5's wire shape is `{"exempt": true, "exempt_until": …, "reason": …}`,
    /// and a `rules` entry under a gate's name is the general shape for *any*
    /// override of that gate — a namespace re-tuning `cve_gate`'s severity
    /// threshold writes one too. The flag is what distinguishes "this gate is
    /// configured differently here" from "this gate does not apply to this
    /// version", and reading an override as an exemption because it happened to
    /// be on an exemptible gate would silence a gate nobody meant to silence.
    #[serde(default)]
    pub exempt: bool,
    /// The gate being silenced. One of [`EXEMPTIBLE_GATES`].
    pub gate: String,
    /// When it stops applying. Required, and a date already past is refused.
    pub exempt_until: chrono::DateTime<chrono::Utc>,
    /// Why. Required, and non-empty.
    pub reason: String,
    /// Who wrote it, for the audit record and the self-approval marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by: Option<String>,
    /// Set when the principal that granted the exemption also published the
    /// version.
    ///
    /// **A marker, not a refusal.** Blocking was the alternative and it is the
    /// wrong trade: four-eyes enforced by the tool is friction a small team
    /// routes around, most often by granting `gates:exempt` more widely, which
    /// is strictly worse than the thing it was trying to prevent. A visible
    /// marker gives an auditor a filter — *show me every exemption nobody else
    /// looked at* — without giving anyone a reason to widen a grant.
    #[serde(default)]
    pub self_approved: bool,
}

impl GateExemption {
    /// Why this exemption may not be written, or `None` when it is fine (§4.9).
    pub fn validate(&self, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
        if !self.exempt {
            return Some(
                "an exemption must set `exempt: true`; an override without it configures the \
                 gate rather than switching it off for this version"
                    .to_owned(),
            );
        }
        if !EXEMPTIBLE_GATES.contains(&self.gate.as_str()) {
            return Some(format!(
                "'{}' is not an exemptible gate. An exemptible gate reports an assessable \
                 finding ({}); every other gate establishes an invariant, and an invariant with \
                 exceptions is not one.",
                self.gate,
                EXEMPTIBLE_GATES.join(", ")
            ));
        }
        if self.reason.trim().is_empty() {
            return Some(
                "an exemption needs a `reason`: the realistic failure is not a wrong assessment, \
                 it is a right assessment nobody revisited"
                    .to_owned(),
            );
        }
        if self.exempt_until <= now {
            return Some(format!(
                "`exempt_until` is {} , which is already past — an exemption that expires on \
                 creation silences nothing and reads as one that does",
                self.exempt_until.to_rfc3339()
            ));
        }
        None
    }

    /// Whether this exemption is in force at `now`.
    pub fn is_active(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.exempt_until > now
    }
}

impl ResolvedPolicy {
    /// The gates currently exempt for this coordinate.
    ///
    /// Expired exemptions are **not** returned: they expire on their own, which
    /// is the property that makes the required `exempt_until` worth requiring.
    /// An unparseable or non-exemptible entry is skipped rather than trusted —
    /// this is the read path, and the fail-closed direction here is to keep the
    /// gate running.
    pub fn exempt_gates(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<GateExemption> {
        self.rules
            .iter()
            .filter_map(|r| {
                let value = r.settings.get("exempt")?;
                if !value.as_bool().unwrap_or(false) {
                    return None;
                }
                let mut ex: GateExemption = serde_json::from_value(r.settings.clone()).ok()?;
                ex.gate = r.gate.clone();
                (ex.validate(now).is_none() && ex.is_active(now)).then_some(ex)
            })
            .collect()
    }
}

/// One registry's config-declared policy tiers.
///
/// The registry- and namespace-tier halves, built at config load. Package and
/// version tiers come from the `policy` table (§6.3) and are not here, for the
/// reason §4.1 gives: a registry with 200 000 packages will not enumerate them
/// in TOML, let alone their two million versions.
///
/// Deliberately the same shape as [`RegistryGrants`](super::RegistryGrants) —
/// same `kind`, same `(prefix, node)` list, same matching — because a reader who
/// has understood how a grant reaches a package has understood how a policy
/// does, and two different hierarchies over the same four tiers would be exactly
/// the duplication this RFC exists to remove.
#[derive(Debug, Clone)]
pub struct RegistryPolicyTiers {
    /// The ecosystem, which decides the namespace separator. Carried here rather
    /// than looked up per request, for the reason `RegistryGrants` gives:
    /// matching `com.acme` with `/` instead of `:` silently changes which
    /// packages a policy reaches.
    pub kind: super::RegistryKind,
    pub registry: PolicyNode,
    /// `(match_prefix, node)`, in config order. Several may match one package,
    /// and for these deepest-wins policies the **last** one wins — so an
    /// operator who writes the more specific block second gets the more specific
    /// answer.
    pub namespaces: Vec<(String, PolicyNode)>,
}

impl RegistryPolicyTiers {
    /// A registry with nothing declared: every default, nothing overridden.
    ///
    /// Unlike `RegistryGrants::closed()` this is genuinely neutral, and the
    /// asymmetry is the model's rather than an oversight. A registry with no
    /// grants refuses everyone, because grants only widen and a union of nothing
    /// is nothing. A registry with no *policy* behaves exactly as it does today,
    /// because these policies are constraints and an absent constraint
    /// constrains nothing.
    pub fn open(kind: super::RegistryKind, registry: &str) -> Self {
        Self {
            kind,
            registry: PolicyNode::new(Tier::Registry, format!("registry:{registry}")),
            namespaces: Vec::new(),
        }
    }

    /// The config-declared part of `package`'s path, registry-first.
    ///
    /// The caller appends the package- and version-tier nodes from the `policy`
    /// table, in that order, before calling [`PolicyPath::resolve`].
    pub fn path_for(&self, package: &str) -> PolicyPath {
        let mut nodes = vec![self.registry.clone()];
        for (prefix, node) in &self.namespaces {
            if super::namespace_matches(self.kind, prefix, package) {
                nodes.push(node.clone());
            }
        }
        PolicyPath::new(nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versioning(semver: bool, monotonic: bool, immutable: Immutable) -> VersioningRules {
        VersioningRules {
            enforce_semver: semver,
            allow_prerelease: true,
            version_pattern: None,
            immutable,
            monotonic,
            dry_run: false,
        }
    }

    fn path(nodes: Vec<PolicyNode>) -> PolicyPath {
        PolicyPath::new(nodes)
    }

    /// Deepest wins for a single value, and the source says which node.
    #[test]
    fn visibility_takes_the_deepest_declaration() {
        let mut registry = PolicyNode::new(Tier::Registry, "registry:npm1");
        registry.visibility = Some(Visibility::Public);
        let mut ns = PolicyNode::new(Tier::Namespace, "namespace:@acme");
        ns.visibility = Some(Visibility::Team);
        let pkg = PolicyNode::new(Tier::Package, "package:@acme/cards");

        let resolved = path(vec![registry, ns, pkg]).resolve();
        assert_eq!(resolved.visibility, Visibility::Team);
        assert_eq!(
            resolved.sources.visibility.as_deref(),
            Some("namespace:@acme")
        );
    }

    /// A node that declares nothing inherits. Absence is not nothing.
    #[test]
    fn a_node_that_declares_nothing_inherits() {
        let mut registry = PolicyNode::new(Tier::Registry, "registry:npm1");
        registry.versioning = Some(versioning(true, false, Immutable::Never));
        let ns = PolicyNode::new(Tier::Namespace, "namespace:@acme");

        let resolved = path(vec![registry, ns]).resolve();
        assert!(resolved.versioning.enforce_semver);
        assert_eq!(
            resolved.sources.versioning.as_deref(),
            Some("registry:npm1")
        );
    }

    /// `versioning` is wholesale: a deeper block replaces the parent's entirely,
    /// which is the only way "this one package follows a different convention"
    /// is expressible.
    #[test]
    fn versioning_composes_wholesale_not_per_field() {
        let mut registry = PolicyNode::new(Tier::Registry, "registry:npm1");
        registry.versioning = Some(VersioningRules {
            enforce_semver: true,
            allow_prerelease: true,
            version_pattern: Some("^v".to_owned()),
            immutable: Immutable::Released,
            monotonic: true,
            dry_run: false,
        });
        let mut pkg = PolicyNode::new(Tier::Package, "package:vendored");
        pkg.versioning = Some(versioning(false, false, Immutable::Never));

        let resolved = path(vec![registry, pkg]).resolve();
        assert!(!resolved.versioning.enforce_semver, "replaced, not merged");
        assert_eq!(
            resolved.versioning.version_pattern, None,
            "and the pattern goes with it"
        );
        assert!(!resolved.versioning.monotonic);
        assert_eq!(resolved.versioning.immutable, Immutable::Never);
    }

    /// `rules` is the exception: a node overriding one gate leaves the others
    /// alone. A wholesale override here would make a forgotten gate a silently
    /// disabled one, which is the fail-open direction.
    #[test]
    fn rules_compose_per_gate_not_wholesale() {
        let over = |gate: &str, v: i64| RuleOverride {
            gate: gate.to_owned(),
            settings: serde_json::json!({ "n": v }),
        };
        let mut registry = PolicyNode::new(Tier::Registry, "registry:npm1");
        registry.rules = vec![over("release_age", 3600), over("cve_gate", 1)];
        let mut ns = PolicyNode::new(Tier::Namespace, "namespace:@acme");
        ns.rules = vec![over("release_age", 0)];

        let resolved = path(vec![registry, ns]).resolve();
        assert_eq!(
            resolved.rules.len(),
            2,
            "cve_gate survives: {:?}",
            resolved.rules
        );
        let age = resolved
            .rules
            .iter()
            .find(|r| r.gate == "release_age")
            .unwrap();
        assert_eq!(age.settings["n"], 0, "the namespace's value wins");
        let cve = resolved
            .rules
            .iter()
            .find(|r| r.gate == "cve_gate")
            .unwrap();
        assert_eq!(
            cve.settings["n"], 1,
            "and the untouched gate keeps the registry's"
        );

        // …and `explain` can say where each came from.
        let source = |g: &str| {
            resolved
                .sources
                .rules
                .iter()
                .find(|(gate, _)| gate == g)
                .map(|(_, node)| node.as_str())
        };
        assert_eq!(source("release_age"), Some("namespace:@acme"));
        assert_eq!(source("cve_gate"), Some("registry:npm1"));
    }

    /// A pre-release is not a narrower audience by default.
    ///
    /// The reading that surprises nobody: setting a package to `team` must not
    /// leave its pre-releases public, even though the pre-release value was
    /// never mentioned.
    #[test]
    fn prerelease_visibility_follows_visibility_when_undeclared() {
        let mut pkg = PolicyNode::new(Tier::Package, "package:p");
        pkg.visibility = Some(Visibility::Team);

        let resolved = path(vec![pkg]).resolve();
        assert_eq!(resolved.prerelease_visibility, Visibility::Team);
        assert_eq!(
            resolved.sources.prerelease_visibility, None,
            "it is not a declaration"
        );
    }

    /// …and a declared one wins, which is `beta_channel`'s whole purpose.
    #[test]
    fn a_declared_prerelease_visibility_wins() {
        let mut ns = PolicyNode::new(Tier::Namespace, "namespace:@acme");
        ns.visibility = Some(Visibility::Public);
        ns.prerelease_visibility = Some(Visibility::Team);

        let resolved = path(vec![ns]).resolve();
        assert_eq!(resolved.visibility, Visibility::Public);
        assert_eq!(resolved.prerelease_visibility, Visibility::Team);
    }

    /// The sharp edge, warned about rather than prevented.
    #[test]
    fn dropping_an_inherited_constraint_warns() {
        let mut registry = PolicyNode::new(Tier::Registry, "registry:npm1");
        registry.versioning = Some(versioning(true, true, Immutable::Always));
        let mut ns = PolicyNode::new(Tier::Namespace, "namespace:@acme");
        ns.versioning = Some(versioning(false, false, Immutable::Never));

        let warnings = path(vec![registry, ns]).narrowing_warnings();
        let text = warnings
            .iter()
            .map(|(_, w)| w.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(text.contains("enforce_semver"), "{text}");
        assert!(text.contains("monotonic"), "{text}");
        assert!(text.contains("relaxes `immutable`"), "{text}");
        assert!(warnings.iter().all(|(k, _)| k == "namespace:@acme"));
    }

    /// **The warnings have to survive `resolve`.**
    ///
    /// `narrowing_warnings` walks a [`PolicyPath`], and nothing outside this
    /// module ever holds one — `resolve_policy` builds the path, resolves it and
    /// drops it. So a warning that is only reachable from the path is a warning
    /// no operator can ever be shown, which is what this one was until
    /// 2026-09-01. Carrying it on the result is what makes it reportable, and
    /// this is the test that keeps it there.
    #[test]
    fn resolving_carries_the_narrowing_warnings_onto_the_result() {
        let mut registry = PolicyNode::new(Tier::Registry, "registry:npm1");
        registry.versioning = Some(versioning(true, true, Immutable::Always));
        let mut ns = PolicyNode::new(Tier::Namespace, "namespace:@acme");
        ns.versioning = Some(versioning(false, false, Immutable::Never));

        let resolved = path(vec![registry, ns]).resolve();
        assert!(
            !resolved.narrowing.is_empty(),
            "a resolved policy that dropped a parent's constraint must say so"
        );
        assert!(resolved
            .narrowing
            .iter()
            .all(|(k, _)| k == "namespace:@acme"));
    }

    /// A deeper block that keeps every constraint is silent — otherwise the test
    /// above would pass on a warning that fires on every override.
    #[test]
    fn restating_the_parents_constraints_is_silent() {
        let mut registry = PolicyNode::new(Tier::Registry, "registry:npm1");
        registry.versioning = Some(versioning(true, true, Immutable::Released));
        let mut ns = PolicyNode::new(Tier::Namespace, "namespace:@acme");
        ns.versioning = Some(versioning(true, true, Immutable::Always));

        let path = path(vec![registry, ns]);
        assert!(
            path.narrowing_warnings().is_empty(),
            "tightening is not narrowing"
        );
        assert!(
            path.resolve().narrowing.is_empty(),
            "and the result agrees with the path"
        );
    }

    /// Several namespaces may match one package, and the last one wins for the
    /// deepest-wins policies — which is config order, so the operator who wrote
    /// the more specific block second gets the more specific answer.
    #[test]
    fn the_last_matching_namespace_wins() {
        let mut broad = PolicyNode::new(Tier::Namespace, "namespace:@acme");
        broad.visibility = Some(Visibility::Internal);
        let mut narrow = PolicyNode::new(Tier::Namespace, "namespace:@acme/billing");
        narrow.visibility = Some(Visibility::Team);

        let resolved = path(vec![broad, narrow]).resolve();
        assert_eq!(resolved.visibility, Visibility::Team);
    }

    /// An empty path is the defaults, not a refusal. A registry with no policy
    /// configured behaves exactly as it does today.
    #[test]
    fn an_empty_path_resolves_to_the_defaults() {
        let resolved = PolicyPath::default().resolve();
        assert_eq!(resolved.visibility, Visibility::Public);
        assert_eq!(resolved.versioning.immutable, Immutable::Never);
        assert!(!resolved.versioning.monotonic);
        assert!(resolved.quota.is_none());
        assert!(resolved.rules.is_empty());
    }

    /// A namespace's policy reaches what is under it, and stops at the segment
    /// boundary — the `digital` versus `digital.pipeline-tools` bug, as a test.
    #[test]
    fn a_namespace_policy_matches_on_segment_boundaries() {
        use super::super::RegistryKind;

        let mut ns = PolicyNode::new(Tier::Namespace, "namespace:com.acme");
        ns.visibility = Some(Visibility::Team);
        let tiers = RegistryPolicyTiers {
            kind: RegistryKind::Maven,
            registry: PolicyNode::new(Tier::Registry, "registry:m1"),
            namespaces: vec![("com.acme".to_owned(), ns)],
        };

        // Maven separates with `:`, so the namespace covers `com.acme:cards`…
        assert_eq!(
            tiers.path_for("com.acme:cards").resolve().visibility,
            Visibility::Team
        );
        // …and not a differently-named group that merely shares a prefix.
        assert_eq!(
            tiers
                .path_for("com.acme-internal:cards")
                .resolve()
                .visibility,
            Visibility::Public
        );
    }

    /// A registry with no policy declared behaves exactly as it does today.
    ///
    /// The asymmetry with grants, pinned: `RegistryGrants::closed()` refuses
    /// everyone, because a union of nothing is nothing. An absent *constraint*
    /// constrains nothing, so this must be the opposite.
    #[test]
    fn an_open_registry_constrains_nothing() {
        use super::super::RegistryKind;

        let resolved = RegistryPolicyTiers::open(RegistryKind::Npm, "npm1")
            .path_for("@acme/cards")
            .resolve();
        assert_eq!(resolved, ResolvedPolicy::default());
    }
}
