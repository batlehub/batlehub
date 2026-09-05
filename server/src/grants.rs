//! Config → grant nodes.
//!
//! RFC 0015 §4.1 and §10. This is where a registry's `[registries.rbac]`,
//! `[registries.grants]` and `[[registries.namespaces]]` become the
//! registry- and namespace-tier [`Node`]s that
//! [`resolve`](batlehub_core::entities::resolve) walks.
//!
//! # Why rule 2 lives here and not in `translate_rbac`
//!
//! §10 rule 2 translates `[registries.rbac.explore]` into `catalogue:browse`,
//! and the rule as this document originally wrote it — *"a role whose flag is
//! `true` gains it; a role whose flag is `false` does not"* — is a
//! specification for a widening. `explore` alone never granted console access.
//! `hot_config::compute_access` gates it on a **conjunction** with the
//! registry's proxy tier, cumulative across roles:
//!
//! ```text
//! (has_anonymous || has_group) && rbac.explore.anonymous
//! (has_user      || has_group) && rbac.explore.user
//! (has_admin     || has_group) && rbac.explore.admin
//! ```
//!
//! and then intersects the result with the caller's own `accessible_registries_for`.
//! Implementing the flag alone produced 19 disagreements the first time the
//! §11.3 differential harness looked at it.
//!
//! `translate_rbac` cannot express that, because the second half of the
//! condition is about the registry's *access tiers* rather than about the rbac
//! permission lists it reads. So it emits no `catalogue:browse` at all, and this
//! module — which has the whole `RegistryConfig` in hand — adds it.
//!
//! # Grants union with the rbac translation, they do not replace it
//!
//! §10: *"`[registries.rbac]` remains accepted indefinitely and is documented as
//! the shorthand it becomes. There is no flag day."* A registry may carry both
//! blocks, and the registry node's grants are the union — which is the only
//! reading consistent with §4.3, where a grant only ever adds.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use batlehub_config::schema::{
    AppConfig, NamespaceConfig, QuotaConfig, QuotaEnforcement, RegistryConfig, RegistryMode,
    RuleConfig, VersioningPolicy,
};
use batlehub_core::entities::{
    expand_patterns, expand_patterns_for, namespace_separator, Action, GrantMap, Node, PolicyNode,
    QuotaRules, RegistryGrants, RegistryKind, RegistryPolicyTiers, RuleOverride, SubjectMatcher,
    Tier, VersioningRules, Visibility, WildcardScope,
};
use batlehub_core::services::authz::translate::{
    build_grants, ExploreFlags, NamespaceSpec, RbacSnapshot, WriteMode,
};

/// The top-level `[grants]` block, if any — RFC 0015 §4.1's instance tier.
///
/// Parsed here rather than in `translate` because the wildcard has to be read
/// the **new** way (`WildcardScope::Everything`): this block is a grants block,
/// not `[registries.rbac]`, so §10 rule 3's legacy reading does not apply to it.
/// Kind-neutral, because the instance tier is above every registry and so above
/// any one ecosystem — an ecosystem verb named here is refused for the same
/// reason §4.2 rule 2 refuses one on the wrong registry type.
pub(crate) fn build_instance_grants(cfg: &AppConfig) -> Result<Option<GrantMap>> {
    let Some(raw) = &cfg.grants else {
        return Ok(None);
    };
    if raw.is_empty() {
        // Same reasoning as a registry-tier seal: the instance node has no
        // ancestor, so an empty map stops nothing and grants nothing. Saying so
        // beats letting an operator believe they closed the server.
        bail!(
            "`[grants]` is empty. A seal stops a node inheriting from its ancestors, and the              instance tier has none — so this grants nothing and blocks nothing. Remove the              block, or name the subjects and verbs you meant to grant."
        );
    }
    let out = merge(GrantMap::new(), raw, None, "[grants]")?;
    // An ecosystem verb cannot be held above every ecosystem.
    let offenders: Vec<String> = out
        .entries()
        .iter()
        .flat_map(|(_, actions)| actions.iter())
        .filter(|a| a.is_ecosystem_scoped())
        .map(|a| a.to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if !offenders.is_empty() {
        bail!(
            "`[grants]` grants {} at the instance tier, which sits above every registry and so              above any one ecosystem. Grant an ecosystem permission on the registry that              defines it.",
            offenders.join(", ")
        );
    }
    Ok(Some(out))
}

/// Build one registry's grant hierarchy from its config.
pub(super) fn build_registry_grants(reg: &RegistryConfig) -> Result<RegistryGrants> {
    let kind: Option<RegistryKind> = reg.registry_type.parse().ok();
    let write_mode = match reg.mode {
        RegistryMode::Proxy => WriteMode::Refuses,
        RegistryMode::Local | RegistryMode::Hybrid => WriteMode::Accepts,
    };

    // ── the rbac translation (rules 1, 3, 4, 5) ──────────────────────────────
    let expand = |patterns: &[String], what: &str| -> Result<Vec<Action>> {
        expand_patterns(patterns, WildcardScope::Legacy)
            .with_context(|| format!("registry '{}': [registries.rbac].{what}", reg.name))
    };
    let snapshot = RbacSnapshot {
        anonymous: expand(&reg.rbac.anonymous, "anonymous")?,
        user: expand(&reg.rbac.user, "user")?,
        admin: expand(&reg.rbac.admin, "admin")?,
        groups: reg
            .rbac
            .groups
            .iter()
            .map(|(k, v)| expand(v, &format!("groups.\"{k}\"")).map(|a| (k.clone(), a)))
            .collect::<Result<HashMap<_, _>>>()?,
        explore: ExploreFlags {
            anonymous: reg.rbac.explore.anonymous,
            user: reg.rbac.explore.user,
            admin: reg.rbac.explore.admin,
        },
    };
    // ── explicit `[registries.grants]` ───────────────────────────────────────
    let explicit = match &reg.grants {
        None => None,
        Some(m) if m.is_empty() => {
            // A registry has no ancestor, so there is nothing for a seal to stop
            // inheriting from. Accepting it silently would leave an operator
            // believing they had closed the registry while `[registries.rbac]`
            // kept answering; rejecting says which knob they wanted.
            bail!(
                "registry '{}': `[registries.grants]` is empty. A seal stops a node \
                 inheriting from its ancestors, and a registry has none — so this grants \
                 nothing and blocks nothing. To close the registry, empty \
                 `[registries.rbac]` instead; to seal a subtree, put `grants = {{}}` on a \
                 `[[registries.namespaces]]` block.",
                reg.name
            );
        }
        Some(m) => Some(merge(
            GrantMap::new(),
            m,
            kind,
            &format!("registry '{}': [registries.grants]", reg.name),
        )?),
    };

    // ── namespaces ───────────────────────────────────────────────────────────
    let mut specs = Vec::new();
    // §4.9 at registry tier, before the namespaces beneath it: `private` says
    // nothing here, and an unsatisfiable pattern refuses every publish.
    let node = format!("registry '{}'", reg.name);
    check_visibility_tier(reg.visibility, Tier::Registry, &node, "visibility")?;
    check_visibility_tier(
        reg.prerelease_visibility,
        Tier::Registry,
        &node,
        "prerelease_visibility",
    )?;
    if let Some(versioning) = &reg.versioning {
        validate_versioning(versioning, &node, reg)?;
    }

    let mut seen: Vec<&str> = Vec::new();
    for ns in &reg.namespaces {
        validate_namespace(reg, ns, kind, &mut seen)?;
        specs.push(NamespaceSpec {
            match_prefix: ns.match_prefix.clone(),
            grants: namespace_grants(reg, ns, kind)?,
            shadow: shadow_for(
                ns.grants_shadow.as_ref(),
                &format!("registry '{}', namespace \"{}\"", reg.name, ns.match_prefix),
            )?,
        });
    }

    // An unparseable `registry_type` is already a config-validation failure;
    // `Generic` is the separator-neutral fallback (`/`) rather than a guess.
    let built = build_grants(
        &reg.name,
        kind.unwrap_or(RegistryKind::Generic),
        &snapshot,
        explicit.as_ref(),
        &specs,
        write_mode,
        shadow_for(reg.grants_shadow.as_ref(), &node)?,
    )
    .map_err(|e| anyhow::anyhow!("registry '{}': {e}", reg.name))?;

    check_scoping_node(&built.registry, reg, kind)?;
    for (_, node) in &built.namespaces {
        check_scoping_node(node, reg, kind)?;
    }
    Ok(built)
}

/// Config → the other five policies (RFC 0015 §4.1).
///
/// The twin of [`build_registry_grants`], and deliberately shaped the same way:
/// the registry node, then one node per `[[registries.namespaces]]` block, in
/// config order. Validation is not repeated here — `build_registry_grants` runs
/// it for both, so the two cannot disagree about whether a config is legal.
pub(super) fn build_policy_tiers(reg: &RegistryConfig) -> RegistryPolicyTiers {
    let kind = reg.registry_type.parse().unwrap_or(RegistryKind::Generic);

    let mut registry = PolicyNode::new(Tier::Registry, format!("registry:{}", reg.name));
    registry.visibility = reg.visibility;
    registry.prerelease_visibility = reg
        .prerelease_visibility
        // §10 rule 6: `[registries.beta_channel]` translates to exactly this
        // setting. `enabled = true` meant "pre-releases are for members only",
        // and `team` is what that becomes — the members being the group the
        // rule-5 translation gave `releases:read` to.
        .or_else(|| {
            reg.beta_channel
                .as_ref()
                .filter(|b| b.enabled)
                .map(|_| Visibility::Team)
        });
    registry.versioning = reg.versioning.as_ref().map(versioning_rules);
    registry.quota = reg.quota.as_ref().map(quota_rules);
    registry.rules = rule_overrides(&reg.rules);
    // RFC 0014 §13 O6: the registry-tier `on_confirmed`. Validation already
    // refused anything but the two values, so a parse failure here is
    // unreachable and reads as "absent".
    registry.on_confirmed = reg.on_confirmed.as_deref().and_then(|s| s.parse().ok());

    let namespaces = reg
        .namespaces
        .iter()
        .map(|ns| {
            let mut node =
                PolicyNode::new(Tier::Namespace, format!("namespace:{}", ns.match_prefix));
            node.visibility = ns.visibility;
            node.prerelease_visibility = ns.prerelease_visibility;
            node.versioning = ns.versioning.as_ref().map(versioning_rules);
            node.quota = ns.quota.as_ref().map(quota_rules);
            node.rules = ns.rules.as_deref().map(rule_overrides).unwrap_or_default();
            (ns.match_prefix.clone(), node)
        })
        .collect();

    RegistryPolicyTiers {
        kind,
        registry,
        namespaces,
    }
}

fn versioning_rules(v: &VersioningPolicy) -> VersioningRules {
    VersioningRules {
        enforce_semver: v.enforce_semver,
        allow_prerelease: v.allow_prerelease,
        version_pattern: v.version_pattern.clone(),
        immutable: v.immutable,
        monotonic: v.monotonic,
        dry_run: v.dry_run,
    }
}

fn quota_rules(q: &QuotaConfig) -> QuotaRules {
    QuotaRules {
        max_bytes_per_user: q.max_storage_bytes_per_user,
        max_packages_per_user: q.max_packages_per_user,
        warn_threshold_pct: Some(q.warn_threshold_pct),
        block: matches!(q.enforcement, QuotaEnforcement::Block),
    }
}

/// One override per gate, keyed by the `kind` tag the TOML uses.
///
/// The settings are carried as JSON rather than as the typed config: this layer
/// composes overrides and does not interpret them, and the rule that consumes
/// one knows its own shape. Serialising through `serde_json` keeps the tag and
/// the fields together without a second enum that would have to be kept in step
/// with `RuleConfig`.
fn rule_overrides(rules: &[RuleConfig]) -> Vec<RuleOverride> {
    rules
        .iter()
        .filter_map(|r| {
            let value = serde_json::to_value(r).ok()?;
            let gate = value.get("kind")?.as_str()?.to_owned();
            Some(RuleOverride {
                gate,
                settings: value,
            })
        })
        .collect()
}

/// §4.7/§4.9 — a shadow, or the reason it may not be written.
///
/// The only check left for the validator: `until` is required by the type, so a
/// shadow with no expiry is a parse error before this runs. What a type cannot
/// say is that the date must be in the *future* — a shadow that expired before
/// the server started is a config that reads as fail-open and enforces, which is
/// the one state an operator must never be left guessing about.
///
/// Rejecting rather than warning, and §4.7 gives the reason: *"a shadow mode
/// that cannot be forgotten is the entire point — the failure this guards
/// against is not a wrong decision, it is a right decision nobody revisited."*
/// A warning is exactly what a right decision nobody revisited looks like.
fn shadow_for(
    cfg: Option<&batlehub_config::schema::GrantsShadowConfig>,
    node: &str,
) -> Result<Option<batlehub_core::entities::DryRun>> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };
    let today = chrono::Utc::now().date_naive();
    if cfg.until < today {
        bail!(
            "{node}: grants_shadow.until is {}, which is already past. A shadow serves every \
             request its grants would refuse, so one that has expired is a config that reads as \
             fail-open and is not — set a future date or remove the block.",
            cfg.until
        );
    }
    Ok(Some(batlehub_core::entities::DryRun { until: cfg.until }))
}

fn namespace_grants(
    reg: &RegistryConfig,
    ns: &NamespaceConfig,
    kind: Option<RegistryKind>,
) -> Result<Option<GrantMap>> {
    Ok(match &ns.grants {
        None => None,
        // The seal. Meaningful here, unlike at registry tier: there *is* an
        // ancestor to stop inheriting from.
        Some(m) if m.is_empty() => Some(GrantMap::sealed()),
        Some(m) => Some(merge(
            GrantMap::new(),
            m,
            kind,
            &format!(
                "registry '{}': [[registries.namespaces]] match = \"{}\"",
                reg.name, ns.match_prefix
            ),
        )?),
    })
}

/// Parse and union a `subject → [pattern]` map into `base`.
fn merge(
    base: GrantMap,
    raw: &HashMap<String, Vec<String>>,
    kind: Option<RegistryKind>,
    where_: &str,
) -> Result<GrantMap> {
    let mut out = base;
    // Sorted, so a config with two subjects produces the same node whichever
    // order the TOML parser handed them over in — which is what makes
    // `explain-config` output diffable.
    let mut keys: Vec<&String> = raw.keys().collect();
    keys.sort();
    for key in keys {
        let subject = SubjectMatcher::parse(key).map_err(|e| anyhow::anyhow!("{where_}: {e}"))?;
        // `WildcardScope::Everything`: a `*` written in a *grants* block is the
        // new wildcard and means every verb. Only a `*` in `[registries.rbac]`
        // carries the legacy reading (§10 rule 3), and that one never reaches
        // here.
        // Kind-aware: a `*` is narrowed to what this registry defines, while a
        // verb named explicitly on the wrong ecosystem is still an error.
        let actions = expand_patterns_for(&raw[key], WildcardScope::Everything, kind)
            .map_err(|e| anyhow::anyhow!("{where_}: subject '{key}': {e}"))?;
        out = out.grant(subject, actions);
    }
    Ok(out)
}

/// RFC 0015 §4.2 rule 2: an ecosystem verb is rejected on a registry of another
/// type, not silently inert.
fn check_scoping_node(node: &Node, reg: &RegistryConfig, kind: Option<RegistryKind>) -> Result<()> {
    match &node.grants {
        Some(grants) => check_scoping(grants, reg, kind),
        None => Ok(()),
    }
}

fn check_scoping(
    grants: &GrantMap,
    reg: &RegistryConfig,
    kind: Option<RegistryKind>,
) -> Result<()> {
    let Some(kind) = kind else {
        return Ok(());
    };
    let offenders: Vec<String> = grants
        .entries()
        .iter()
        .flat_map(|(_, actions)| actions.iter())
        .filter(|a| !a.applies_to(kind))
        .map(|a| a.to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }
    bail!(
        "registry '{}': grants {} on a '{}' registry, which does not define {}. An \
         ecosystem permission is only grantable on the registry types that implement it.",
        reg.name,
        offenders.join(", "),
        reg.registry_type,
        if offenders.len() == 1 { "it" } else { "them" },
    )
}

/// §4.9: a namespace `match` that cannot occur, and two blocks with the same
/// match, are both config errors.
fn validate_namespace<'a>(
    reg: &RegistryConfig,
    ns: &'a NamespaceConfig,
    kind: Option<RegistryKind>,
    seen: &mut Vec<&'a str>,
) -> Result<()> {
    if ns.match_prefix.trim().is_empty() {
        bail!(
            "registry '{}': a [[registries.namespaces]] block has an empty `match`, which \
             matches no package at all",
            reg.name
        );
    }
    if seen.contains(&ns.match_prefix.as_str()) {
        // Two blocks with the same match are not a union written twice — they
        // are two answers to "what policy applies here", and phase 4 attaches
        // `visibility` and `versioning` to this node, where a second block would
        // be a genuine contradiction rather than a harmless duplicate.
        bail!(
            "registry '{}': two [[registries.namespaces]] blocks both match \"{}\"; \
             merge them",
            reg.name,
            ns.match_prefix
        );
    }
    seen.push(&ns.match_prefix);

    if let Some(kind) = kind {
        let sep = namespace_separator(kind);
        if ns.match_prefix.ends_with(sep) {
            bail!(
                "registry '{}': namespace match \"{}\" ends with the '{}' separator this \
                 ecosystem uses, so it can never match — matching appends the separator \
                 itself. Write \"{}\".",
                reg.name,
                ns.match_prefix,
                sep,
                ns.match_prefix.trim_end_matches(sep)
            );
        }
    }

    // §4.9: `private` is a package- and version-tier value. At namespace tier
    // "only grants written at this node or below" says what `grants = {}`
    // already says properly, and accepting it here would give sealing a second,
    // weaker spelling.
    let node = format!("registry '{}', namespace \"{}\"", reg.name, ns.match_prefix);
    check_visibility_tier(ns.visibility, Tier::Namespace, &node, "visibility")?;
    check_visibility_tier(
        ns.prerelease_visibility,
        Tier::Namespace,
        &node,
        "prerelease_visibility",
    )?;

    if let Some(versioning) = &ns.versioning {
        validate_versioning(versioning, &node, reg)?;
    }
    Ok(())
}

/// §4.9: `visibility = "private"` is rejected above the package tier.
fn check_visibility_tier(
    visibility: Option<Visibility>,
    tier: Tier,
    node: &str,
    field: &str,
) -> Result<()> {
    let Some(v) = visibility else {
        return Ok(());
    };
    if v.is_valid_at(tier) {
        return Ok(());
    }
    bail!(
        "{node}: {field} = \"private\" is a package- and version-tier value. Here it either \
         says nothing or says what `grants = {{}}` already says properly — seal the node with \
         an empty grants block instead."
    )
}

/// §4.9's versioning rejections, for a `[…versioning]` block at any tier that
/// can carry one in TOML.
///
/// The two `pattern` checks are the interesting half, because an unsatisfiable
/// pattern is not a typo that fails loudly — it is a config that refuses **every
/// publish**, and it does so at the moment someone tries to publish rather than
/// at the moment it was written.
fn validate_versioning(
    versioning: &VersioningPolicy,
    node: &str,
    reg: &RegistryConfig,
) -> Result<()> {
    if let Some(pattern) = &versioning.version_pattern {
        let compiled = regex::Regex::new(pattern).map_err(|e| {
            anyhow::anyhow!("{node}: version_pattern \"{pattern}\" is not a valid regex: {e}")
        })?;

        // "A pattern that cannot match any string the ecosystem permits as a
        // version." Undecidable in general, so this tests the pattern against a
        // corpus of shapes every ecosystem this proxies actually publishes. A
        // pattern matching none of them is not proof it is unsatisfiable, but it
        // is proof it refuses everything an ordinary release looks like, which
        // is the config error worth catching.
        const ORDINARY_VERSIONS: &[&str] = &[
            "1.0.0",
            "0.1.0",
            "1.2.3",
            "10.20.30",
            "1.0.0-beta.1",
            "1.0.0-rc1",
            "v1.0.0",
            "1.0",
            "1.0.0.1",
            "1.0-SNAPSHOT",
            "2024.1.1",
            "1.0.0+build.1",
        ];
        if !ORDINARY_VERSIONS.iter().any(|v| compiled.is_match(v)) {
            bail!(
                "{node}: version_pattern \"{pattern}\" matches none of the ordinary version \
                 shapes ({}), so every publish would be refused. If that is deliberate, seal \
                 the node with an empty grants block rather than with a pattern nothing \
                 satisfies.",
                ORDINARY_VERSIONS.join(", ")
            );
        }

        // "`enforce_semver = true` with a pattern that no semver string can
        // satisfy — the pair is unsatisfiable and every publish would be
        // refused." Same corpus, narrowed to what `enforce_semver` would let
        // through in the first place.
        if versioning.enforce_semver {
            let semver_ok: Vec<&str> = ORDINARY_VERSIONS
                .iter()
                .copied()
                .filter(|v| semver::Version::parse(v).is_ok())
                .collect();
            if !semver_ok.iter().any(|v| compiled.is_match(v)) {
                bail!(
                    "{node}: enforce_semver = true with version_pattern \"{pattern}\", which \
                     matches no semver string ({}). The pair is unsatisfiable and every \
                     publish would be refused.",
                    semver_ok.join(", ")
                );
            }
        }
    }

    // §4.9: "`monotonic = true` on a registry in `proxy` mode, where nothing is
    // published and the setting can only mislead." A rejection rather than a
    // warning, and §4.9 says why the two differ: nothing in today's config can
    // produce this, so no existing instance can be broken by refusing it, and a
    // new operator writing it by hand is better told immediately.
    if versioning.monotonic && reg.mode == RegistryMode::Proxy {
        bail!(
            "{node}: monotonic = true on a proxy-mode registry, which accepts no publishes. \
             The setting has nothing to order and can only mislead."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use batlehub_core::entities::{resolve, Identity, Role, Subject};

    fn registry(toml_body: &str) -> RegistryConfig {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            registries: Vec<RegistryConfig>,
        }
        let w: Wrapper =
            toml::from_str(&format!("[[registries]]\n{toml_body}")).expect("valid registry toml");
        w.registries.into_iter().next().unwrap()
    }

    fn subject(role: Role, groups: &[&str]) -> Subject {
        Subject::Identity(Identity {
            user_id: Some("u".to_owned()),
            role,
            auth_provider: None,
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
        })
    }

    fn holds_at_registry(reg: &RegistryConfig, subj: &Subject, action: Action) -> bool {
        let grants = build_registry_grants(reg).expect("builds");
        resolve(&[grants.registry], subj).holds(action)
    }

    /// §10 rule 2's conjunction: the flag alone is not enough.
    ///
    /// This is the widening the differential harness caught. A role with
    /// `explore = true` and no proxy permissions of its own reaches nothing
    /// today, and must reach nothing afterwards.
    #[test]
    fn the_explore_flag_alone_does_not_grant_the_console() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.rbac]
               anonymous = []
               user = []
               admin = ["releases:read"]
               [registries.rbac.explore]
               anonymous = true
               user = true
               admin = true"#,
        );
        // `anonymous` and `user` tiers are empty and there are no groups, so
        // neither reaches the console — however the flag reads.
        assert!(!holds_at_registry(
            &reg,
            &subject(Role::Anonymous, &[]),
            Action::CatalogueBrowse
        ));
        // The admin tier is non-empty, so the admin flag does apply.
        assert!(holds_at_registry(
            &reg,
            &subject(Role::Admin, &[]),
            Action::CatalogueBrowse
        ));
    }

    /// …and when the tier *is* non-empty, a `false` flag still withholds it.
    #[test]
    fn a_false_explore_flag_withholds_the_console() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.rbac]
               anonymous = []
               user = ["releases:read"]
               admin = ["releases:read"]
               [registries.rbac.explore]
               anonymous = false
               user = false
               admin = true"#,
        );
        assert!(!holds_at_registry(
            &reg,
            &subject(Role::User, &[]),
            Action::CatalogueBrowse
        ));
        assert!(holds_at_registry(
            &reg,
            &subject(Role::Admin, &[]),
            Action::CatalogueBrowse
        ));
    }

    /// A group-only registry reaches the console, which is the case
    /// `compute_access` widened each tier for.
    #[test]
    fn a_group_only_registry_still_reaches_the_console() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.rbac]
               anonymous = []
               user = []
               admin = []
               [registries.rbac.groups]
               "team-a" = ["releases:read"]
               [registries.rbac.explore]
               anonymous = true
               user = true
               admin = true"#,
        );
        assert!(holds_at_registry(
            &reg,
            &subject(Role::Anonymous, &["team-a"]),
            Action::CatalogueBrowse
        ));
    }

    /// An explicit `[registries.grants]` block unions with the rbac
    /// translation — §10's "no flag day".
    #[test]
    fn explicit_grants_union_with_the_rbac_translation() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.rbac]
               anonymous = []
               user = ["releases:read"]
               admin = ["*"]
               [registries.grants]
               "user:alice" = ["source:read"]"#,
        );
        let alice = Subject::Identity(Identity {
            user_id: Some("alice".to_owned()),
            role: Role::User,
            auth_provider: None,
            groups: vec![],
        });
        assert!(holds_at_registry(&reg, &alice, Action::SourceRead));
        assert!(
            holds_at_registry(&reg, &alice, Action::ReleasesRead),
            "the rbac block must keep working beside a grants block"
        );
    }

    /// A `*` in a grants block is the *new* wildcard.
    ///
    /// The legacy reading is confined to `[registries.rbac]` (§10 rule 3), and
    /// this is the boundary between them: a grants block is new syntax, written
    /// by someone who has read the new vocabulary.
    #[test]
    fn a_wildcard_in_a_grants_block_means_every_verb() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.grants]
               "user:root" = ["*"]"#,
        );
        let root = Subject::Identity(Identity {
            user_id: Some("root".to_owned()),
            role: Role::Anonymous,
            auth_provider: None,
            groups: vec![],
        });
        assert!(holds_at_registry(&reg, &root, Action::GatesExempt));
        assert!(holds_at_registry(&reg, &root, Action::ReleasesDelete));
    }

    /// A namespace grant reaches packages under it, on segment boundaries only.
    #[test]
    fn a_namespace_grant_reaches_its_own_packages_and_no_others() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [[registries.namespaces]]
               match = "@acme/billing"
               [registries.namespaces.grants]
               "group:*:payments" = ["releases:publish"]"#,
        );
        let grants = build_registry_grants(&reg).expect("builds");
        let payments = subject(Role::User, &["oidc1:payments"]);

        let inside = grants.path_for("@acme/billing/cards");
        assert!(resolve(&inside, &payments).holds(Action::ReleasesPublish));

        let neighbour = grants.path_for("@acme/billing-internal");
        assert!(
            !resolve(&neighbour, &payments).holds(Action::ReleasesPublish),
            "a hyphen is not a separator"
        );

        let elsewhere = grants.path_for("@other/thing");
        assert!(!resolve(&elsewhere, &payments).holds(Action::ReleasesPublish));
    }

    /// A namespace seal stops the registry's grants, and the floor survives.
    #[test]
    fn a_namespace_seal_stops_inheritance_but_not_the_floor() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.rbac]
               anonymous = ["releases:read"]
               user = []
               admin = ["*"]
               [[registries.namespaces]]
               match = "@acme/secret"
               grants = {}"#,
        );
        let grants = build_registry_grants(&reg).expect("builds");
        let path = grants.path_for("@acme/secret/thing");
        let anon = subject(Role::Anonymous, &[]);
        assert!(!resolve(&path, &anon).holds(Action::ReleasesRead));

        // The admin keeps the administrative floor — rule 5 grants
        // `owners:write` and `audit:read` to `role:admin` at registry tier.
        let admin = subject(Role::Admin, &[]);
        let resolved = resolve(&path, &admin);
        assert!(resolved.holds(Action::OwnersWrite));
        assert!(resolved.holds(Action::AuditRead));
        assert!(!resolved.holds(Action::ReleasesRead));
    }

    // ── validation ───────────────────────────────────────────────────────────

    fn refusal(reg: &RegistryConfig, what: &str) -> String {
        match build_registry_grants(reg) {
            Ok(_) => panic!("{what}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn a_registry_tier_seal_is_refused_as_meaningless() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               grants = {}"#,
        );
        let err = refusal(&reg, "an empty registry grants block should not build");
        assert!(
            err.contains("no ancestor") || err.contains("has none"),
            "{err}"
        );
    }

    #[test]
    fn a_malformed_grant_subject_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.grants]
               "nope" = ["releases:read"]"#,
        );
        assert!(refusal(&reg, "a bad subject should not build").contains("nope"));
    }

    #[test]
    fn an_ecosystem_verb_is_refused_on_the_wrong_registry_type() {
        let reg = registry(
            r#"type = "maven"
               name = "m1"
               [registries.grants]
               "role:user" = ["npm:dist-tags:write"]"#,
        );
        let err = refusal(&reg, "npm:dist-tags:write is not a Maven permission");
        assert!(
            err.contains("npm:dist-tags:write") && err.contains("maven"),
            "{err}"
        );
    }

    #[test]
    fn two_namespaces_with_the_same_match_are_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [[registries.namespaces]]
               match = "@acme/a"
               [[registries.namespaces]]
               match = "@acme/a""#,
        );
        assert!(refusal(&reg, "duplicate matches should not build").contains("@acme/a"));
    }

    /// A `match` that ends in the separator can never match, because matching
    /// appends the separator itself.
    #[test]
    fn a_namespace_match_ending_in_the_separator_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [[registries.namespaces]]
               match = "@acme/billing/""#,
        );
        let err = refusal(&reg, "a trailing separator should not build");
        assert!(err.contains("separator"), "{err}");
    }

    /// The namespace tier carries policy as of phase 4, not only grants.
    ///
    /// This test used to assert the opposite — that `visibility` on a namespace
    /// did **not** parse — which was the phase-3 contract:
    /// `deny_unknown_fields` was what stopped an operator writing a policy,
    /// getting no error, and concluding it was in force. Phase 4 implements the
    /// keys, so the guard becomes the assertion.
    #[test]
    fn a_namespace_carries_phase_4_policy() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [[registries.namespaces]]
               match = "@acme/a"
               visibility = "team"
               prerelease_visibility = "team"
               [registries.namespaces.versioning]
               enforce_semver = true
               immutable = "released"
               monotonic = true"#,
        );
        let ns = &reg.namespaces[0];
        assert_eq!(ns.visibility, Some(Visibility::Team));
        assert_eq!(ns.prerelease_visibility, Some(Visibility::Team));
        let versioning = ns.versioning.as_ref().expect("versioning parses");
        assert_eq!(
            versioning.immutable,
            batlehub_config::schema::Immutable::Released
        );
        assert!(versioning.monotonic);
        // And it still builds, so the keys are read rather than merely accepted.
        build_registry_grants(&reg).expect("builds");
    }

    /// A `retention` block on a namespace is still refused: it is RFC 0016's,
    /// and phase 4 did not silently adopt it.
    #[test]
    fn a_namespace_retention_block_is_still_refused() {
        #[derive(Debug, serde::Deserialize)]
        struct Wrapper {
            #[allow(dead_code)]
            registries: Vec<RegistryConfig>,
        }
        let err = toml::from_str::<Wrapper>(
            r#"[[registries]]
               type = "npm"
               name = "n1"
               [[registries.namespaces]]
               match = "@acme/a"
               [registries.namespaces.retention]
               keep_if_pulled_days = 90"#,
        )
        .expect_err("retention is not a namespace key yet");
        assert!(err.to_string().contains("retention"), "{err}");
    }

    // ── §4.9 rejections ──────────────────────────────────────────────────────

    /// `private` says nothing above the package tier, and accepting it would
    /// give sealing a second, weaker spelling.
    #[test]
    fn private_visibility_is_refused_at_registry_and_namespace_tier() {
        for body in [
            r#"type = "npm"
               name = "n1"
               visibility = "private""#,
            r#"type = "npm"
               name = "n1"
               [[registries.namespaces]]
               match = "@acme/a"
               visibility = "private""#,
        ] {
            let err = refusal(
                &registry(body),
                "private above package tier should not build",
            );
            assert!(err.contains("package- and version-tier"), "{err}");
        }
    }

    /// A pattern no ordinary version satisfies refuses every publish, and does
    /// so at publish time rather than at the edit that introduced it.
    #[test]
    fn an_unsatisfiable_version_pattern_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [registries.versioning]
               version_pattern = "^nothing-shaped-like-a-version$""#,
        );
        let err = refusal(&reg, "a pattern matching no version should not build");
        assert!(err.contains("every publish would be refused"), "{err}");
    }

    /// An invalid regex is a config error, not a publish-time surprise.
    #[test]
    fn an_invalid_version_pattern_regex_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [registries.versioning]
               version_pattern = "^(unclosed""#,
        );
        let err = refusal(&reg, "an invalid regex should not build");
        assert!(err.contains("not a valid regex"), "{err}");
    }

    /// The pair is unsatisfiable: the pattern matches versions, just never one
    /// `enforce_semver` would let through.
    #[test]
    fn enforce_semver_with_a_non_semver_pattern_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [registries.versioning]
               enforce_semver = true
               version_pattern = "^\\d+\\.\\d+-SNAPSHOT$""#,
        );
        let err = refusal(&reg, "an unsatisfiable pair should not build");
        assert!(err.contains("matches no semver string"), "{err}");
    }

    /// …and the same pattern without `enforce_semver` is fine, which is what
    /// makes the test above about the *pair* rather than about the pattern.
    #[test]
    fn the_same_pattern_without_enforce_semver_builds() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [registries.versioning]
               version_pattern = "^\\d+\\.\\d+-SNAPSHOT$""#,
        );
        build_registry_grants(&reg).expect("a SNAPSHOT-only registry is a legitimate config");
    }

    /// A proxy-mode registry accepts no publishes, so there is nothing to order.
    #[test]
    fn monotonic_on_a_proxy_registry_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               [registries.versioning]
               monotonic = true"#,
        );
        let err = refusal(&reg, "monotonic on a proxy registry should not build");
        assert!(err.contains("accepts no publishes"), "{err}");
    }

    /// The same setting on a local registry is the point of the feature.
    #[test]
    fn monotonic_on_a_local_registry_builds() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [registries.versioning]
               monotonic = true"#,
        );
        build_registry_grants(&reg).expect("builds");
    }

    // ── policy tiers (§4.1) ──────────────────────────────────────────────────

    /// Config reaches the resolver, and composes by §4.1's rules once there.
    ///
    /// The unit tests in `entities::policy` prove the composition; this proves
    /// the wiring, which is the half that fails silently — a builder that reads
    /// the wrong field produces a resolver answering defaults for a config that
    /// declared something, and every composition test still passes.
    #[test]
    fn namespace_policy_reaches_the_resolver() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               visibility = "public"

               [registries.versioning]
               enforce_semver = true
               immutable = "always"

               [[registries.namespaces]]
               match = "@acme/billing"
               visibility = "team"

               [registries.namespaces.versioning]
               enforce_semver = false
               immutable = "never""#,
        );
        let tiers = build_policy_tiers(&reg);

        // Inside the namespace: its own values, wholesale.
        let inside = tiers.path_for("@acme/billing/cards").resolve();
        assert_eq!(inside.visibility, Visibility::Team);
        assert!(!inside.versioning.enforce_semver, "wholesale replacement");
        assert_eq!(
            inside.versioning.immutable,
            batlehub_core::entities::Immutable::Never
        );

        // Outside it: the registry's.
        let outside = tiers.path_for("@other/thing").resolve();
        assert_eq!(outside.visibility, Visibility::Public);
        assert!(outside.versioning.enforce_semver);
        assert_eq!(
            outside.versioning.immutable,
            batlehub_core::entities::Immutable::Always
        );
    }

    /// §10 rule 6: `[registries.beta_channel]` becomes `prerelease_visibility`.
    ///
    /// The translation, not a re-implementation — an existing config that says
    /// "pre-releases are for members only" must keep saying it after upgrade,
    /// which is the whole of §10's contract.
    #[test]
    fn beta_channel_translates_to_prerelease_visibility() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"

               [registries.beta_channel]
               enabled = true"#,
        );
        let resolved = build_policy_tiers(&reg).path_for("pkg").resolve();
        assert_eq!(resolved.prerelease_visibility, Visibility::Team);
        assert_eq!(
            resolved.visibility,
            Visibility::Public,
            "and it must not narrow releases too"
        );
    }

    /// A disabled beta channel translates to nothing, so pre-releases stay as
    /// visible as releases.
    #[test]
    fn a_disabled_beta_channel_translates_to_nothing() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"

               [registries.beta_channel]
               enabled = false"#,
        );
        let resolved = build_policy_tiers(&reg).path_for("pkg").resolve();
        assert_eq!(resolved.prerelease_visibility, Visibility::Public);
    }

    /// An explicit `prerelease_visibility` wins over the translated one, so an
    /// operator migrating off `beta_channel` is not fighting their own config.
    #[test]
    fn an_explicit_prerelease_visibility_wins_over_beta_channel() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               prerelease_visibility = "internal"

               [registries.beta_channel]
               enabled = true"#,
        );
        let resolved = build_policy_tiers(&reg).path_for("pkg").resolve();
        assert_eq!(resolved.prerelease_visibility, Visibility::Internal);
    }

    /// Gate overrides compose per rule, and the standing `release_age` finding
    /// is the case: first-party CI publishes into a namespace that sets
    /// `min_age_secs = 0`, and the registry's other gates keep running.
    #[test]
    fn a_namespace_rule_override_replaces_one_gate_and_leaves_the_rest() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"

               [[registries.rules]]
               kind = "release_age_gate"
               min_age_secs = 3600

               [[registries.rules]]
               kind = "deny_latest"
               enabled = true

               [[registries.namespaces]]
               match = "@acme"

               [[registries.namespaces.rules]]
               kind = "release_age_gate"
               min_age_secs = 0"#,
        );
        let resolved = build_policy_tiers(&reg)
            .path_for("@acme/ci-built")
            .resolve();

        let gate = |name: &str| {
            resolved
                .rules
                .iter()
                .find(|r| r.gate == name)
                .map(|r| r.settings.clone())
        };
        assert_eq!(
            gate("release_age_gate").unwrap()["min_age_secs"],
            0,
            "the namespace's quarantine wins"
        );
        assert!(
            gate("deny_latest").is_some(),
            "and the gate it did not mention keeps running: {:?}",
            resolved.rules
        );
    }

    // ── §4.7 shadow mode ─────────────────────────────────────────────────────

    /// A shadow that expired before the server started is refused, not warned
    /// about.
    ///
    /// §4.7: *"a shadow mode that cannot be forgotten is the entire point — the
    /// failure this guards against is not a wrong decision, it is a right
    /// decision nobody revisited."* A warning is exactly what a right decision
    /// nobody revisited looks like.
    #[test]
    fn a_shadow_with_a_past_expiry_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [registries.grants_shadow]
               until = "2020-01-01""#,
        );
        let err = refusal(&reg, "an expired shadow should not build");
        assert!(err.contains("already past"), "{err}");
        assert!(
            err.contains("fail-open"),
            "the message must name the consequence: {err}"
        );
    }

    /// The same on a namespace.
    #[test]
    fn a_namespace_shadow_with_a_past_expiry_is_refused() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [[registries.namespaces]]
               match = "@acme"
               [registries.namespaces.grants_shadow]
               until = "2020-01-01""#,
        );
        let err = refusal(&reg, "an expired namespace shadow should not build");
        assert!(err.contains("already past"), "{err}");
        assert!(err.contains("@acme"), "and name the node: {err}");
    }

    /// A future shadow builds, and reaches the node the resolver reads.
    ///
    /// The wiring half: a builder that validated the date and dropped the value
    /// would pass every rejection test above and shadow nothing.
    #[test]
    fn a_future_shadow_reaches_the_node() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local"
               [registries.grants_shadow]
               until = "2099-12-01"
               [[registries.namespaces]]
               match = "@acme"
               [registries.namespaces.grants_shadow]
               until = "2099-06-01""#,
        );
        let grants = build_registry_grants(&reg).expect("builds");
        assert_eq!(
            grants
                .registry
                .dry_run
                .as_ref()
                .map(|d| d.until.to_string()),
            Some("2099-12-01".to_owned())
        );
        assert_eq!(
            grants.namespaces[0]
                .1
                .dry_run
                .as_ref()
                .map(|d| d.until.to_string()),
            Some("2099-06-01".to_owned())
        );
    }

    /// No block is no shadow, which is the state every existing config is in.
    #[test]
    fn no_block_is_no_shadow() {
        let reg = registry(
            r#"type = "npm"
               name = "n1"
               mode = "local""#,
        );
        let grants = build_registry_grants(&reg).expect("builds");
        assert!(grants.registry.dry_run.is_none());
    }

    // ── The instance tier's config surface (§4.1, §13.12) ────────────────────

    /// Parse a whole `AppConfig` from a TOML body, for the top-level `[grants]`
    /// block that the registry helper above cannot reach.
    fn app_config(toml_body: &str) -> AppConfig {
        let base = r#"
[server]
host = "0.0.0.0"
port = 8080
[database]
type = "postgresql"
url = "postgresql://x/y"
[storage]
type = "filesystem"
path = "/tmp/x"
"#;
        toml::from_str(&format!("{base}{toml_body}")).expect("valid config toml")
    }

    /// No `[grants]` block is `None` — not an empty map.
    ///
    /// The distinction is §4.3's, one tier up: absence inherits and an empty map
    /// **seals**. A deployment that has never written an instance grant has to
    /// resolve exactly as it did before the tier existed, and returning
    /// `Some(empty)` here would seal every registry beneath it on upgrade.
    #[test]
    fn an_absent_grants_block_is_absence_rather_than_an_empty_map() {
        let cfg = app_config("");
        assert!(build_instance_grants(&cfg).expect("builds").is_none());
    }

    /// An **empty** `[grants]` block is refused rather than read as a seal.
    ///
    /// Same reasoning as the registry-tier seal one tier down (§13.5): the
    /// instance node has no ancestor, so an empty map stops nothing and grants
    /// nothing. Accepting it silently would leave an operator believing they had
    /// closed the server.
    #[test]
    fn an_empty_grants_block_is_a_config_error() {
        let cfg = app_config("[grants]\n");
        let err = build_instance_grants(&cfg).expect_err("an empty block is refused");
        assert!(
            err.to_string()
                .contains("grants nothing and blocks nothing"),
            "the error has to say why, not just that: {err}"
        );
    }

    /// A subject and its verbs round-trip, and reach exactly who they name.
    #[test]
    fn an_instance_grant_reaches_the_subject_it_names_and_nobody_else() {
        let cfg = app_config("[grants]\n\"group:*:sre\" = [\"config:read\", \"blocks:write\"]\n");
        let map = build_instance_grants(&cfg)
            .expect("builds")
            .expect("present");
        let node = Node::new(Tier::Instance, "instance", Some(map));

        let sre = subject(Role::User, &["oidc1:sre"]);
        assert!(resolve(std::slice::from_ref(&node), &sre).holds(Action::ConfigRead));
        assert!(resolve(std::slice::from_ref(&node), &sre).holds(Action::BlocksWrite));
        assert!(
            !resolve(std::slice::from_ref(&node), &sre).holds(Action::ConfigWrite),
            "a grant reaches what it names and no more"
        );

        let other = subject(Role::User, &["oidc1:dev"]);
        assert!(!resolve(std::slice::from_ref(&node), &other).holds(Action::ConfigRead));
    }

    /// An **ecosystem** verb cannot be held above every ecosystem.
    ///
    /// §4.2 rule 2 refuses one on the wrong registry type; the instance tier sits
    /// above *every* registry type, so there is no type it could be right for.
    /// Refused at load rather than silently inert — which is the failure mode the
    /// closed enum exists to remove.
    #[test]
    fn an_ecosystem_verb_is_refused_at_the_instance_tier() {
        let cfg = app_config("[grants]\n\"role:admin\" = [\"npm:dist-tags:write\"]\n");
        let err = build_instance_grants(&cfg).expect_err("refused");
        assert!(
            err.to_string().contains("above every registry"),
            "the message has to explain the tier, not just refuse: {err}"
        );
    }

    /// An unknown verb is a startup error here as everywhere else.
    #[test]
    fn an_unknown_verb_is_a_startup_error() {
        let cfg = app_config("[grants]\n\"role:admin\" = [\"config:raed\"]\n");
        assert!(build_instance_grants(&cfg).is_err());
    }

    /// A `*` in a **grants** block is the new wildcard, not §10 rule 3's legacy
    /// reading — that one belongs to `[registries.rbac]` and never reaches here.
    ///
    /// Kind-neutral at this tier, so the wildcard covers the shared vocabulary
    /// and the ecosystem verbs are excluded by the rule above rather than by the
    /// expansion.
    #[test]
    fn a_wildcard_in_the_instance_block_is_the_new_wildcard() {
        let cfg = app_config("[grants]\n\"role:admin\" = [\"config:*\"]\n");
        let map = build_instance_grants(&cfg)
            .expect("builds")
            .expect("present");
        let node = Node::new(Tier::Instance, "instance", Some(map));
        let admin = subject(Role::Admin, &[]);
        assert!(resolve(std::slice::from_ref(&node), &admin).holds(Action::ConfigRead));
        assert!(resolve(std::slice::from_ref(&node), &admin).holds(Action::ConfigWrite));
        assert!(
            !resolve(std::slice::from_ref(&node), &admin).holds(Action::SystemRead),
            "`config:*` reaches the config family and stops there"
        );
    }
}
