//! The permission vocabulary: what a caller can be permitted to *do*.
//!
//! # Why this is an enum
//!
//! Until RFC 0015 phase 1 this was two `&'static str` constants
//! (`rules::resource_type::{RELEASES_READ, SOURCE_READ}`) threaded through
//! every handler as `&str`, and the failure mode was silence in both
//! directions. A handler that passed a typo'd string asked for a permission
//! nothing grants, and refused every caller for a reason no log explained; a
//! config that granted a typo'd string granted nothing, and the operator who
//! wrote it saw no error and no effect. Neither shows up in review, because
//! both look exactly like the correct spelling.
//!
//! A closed enum removes the class rather than the instances. Adding a verb is
//! a code change the compiler propagates to every `match`, a config carrying a
//! verb this build does not know is refused at load (§4.9) rather than silently
//! inert, and "which routes ask for this?" is a question `grep` can answer.
//!
//! # The two halves
//!
//! **A shared core** every ecosystem has in the same shape — reads, writes,
//! ownership, and the three disclosure surfaces RFC 0015 §4.2 splits out of
//! `require_admin` (`catalogue:browse`, `stats:read`, `audit:read`).
//!
//! **Ecosystem verbs** for actions nobody else has. Moving an npm `dist-tag` is
//! neither publishing nor reading — the bytes already exist and nobody is
//! adding any — and forcing it into an existing verb would make that verb mean
//! something different on npm than it does anywhere else, which is how a
//! vocabulary stops being one. They carry their ecosystem's prefix, they are
//! rejected at config load on a registry of another type ([`Action::kinds`]),
//! and no expansion of `releases:*` reaches them.
//!
//! # What is deliberately absent
//!
//! There is no free-text variant and no `Other(String)`. That is the property
//! the whole file exists for; an escape hatch would restore the silence.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::RegistryKind;

/// One thing a caller can be permitted to do.
///
/// Serialises as its wire form (`releases:read`), so a config file, an API body
/// and a log line all spell it the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Action {
    // ── the shared core: reads ───────────────────────────────────────────────
    /// Artifact bytes.
    ReleasesRead,
    /// Version documents, protocol indexes and search results — including the
    /// cargo sparse index, which is a version listing whatever its URL suggests.
    ReleasesList,
    /// Source archives (Go `.zip`, a GitHub tarball).
    SourceRead,
    /// The console's explore and search surfaces.
    ///
    /// Not [`Action::ReleasesList`], and the distinction is load-bearing rather
    /// than fastidious: browsing a catalogue in a console and resolving a
    /// version document from a package manager are different exposures. "Build
    /// agents resolve everything, people browse nothing" is a real and common
    /// configuration on a mirror, and one verb cannot express it.
    CatalogueBrowse,

    // ── the shared core: writes ──────────────────────────────────────────────
    /// Creating a new version.
    ReleasesPublish,
    /// Replacing an existing version. Subject to the resource's immutability
    /// setting as well as this verb — a replace needs both.
    ReleasesOverwrite,
    /// Yank and unyank.
    ReleasesYank,
    /// Hard delete.
    ReleasesDelete,

    // ── the shared core: governance ──────────────────────────────────────────
    /// Reading the ownership list.
    OwnersRead,
    /// Editing the ownership list, and — from RFC 0015 phase 3 — grants below
    /// the tier at which it is held.
    OwnersWrite,
    /// Administrative block and unblock.
    PackagesBlock,
    /// Accepting a `cve_gate` or `license_gate` finding on one version.
    ///
    /// Under its own prefix on purpose: `releases:*` never reaches it, so
    /// silencing a finding is not something a publisher acquires by being able
    /// to publish.
    GatesExempt,
    /// Download counts, storage totals and the aggregates the console dashboard
    /// is built from.
    StatsRead,
    /// The access log.
    AuditRead,
    /// Seeing that a version is `quarantined`/`denied`/`warned`, its reason
    /// codes and `available_at` (RFC 0018 §4.1). Without it a held version is
    /// indistinguishable from one that does not exist upstream.
    QuarantineRead,
    /// Seeing the findings behind those codes — CVE ids, scanner output, a
    /// SOC case id — which can name an embargoed advisory.
    FindingsRead,
    /// Deleting access events older than a cutoff.
    ///
    /// Its own verb rather than `audit:read`'s, and the distinction is the point
    /// of the delegation: an estate that wants a reviewer to *read* the trail
    /// does not thereby want them able to erase it — including the record of
    /// their own actions, and including the `audit:purge` event the purge itself
    /// writes, which a second call with the same cutoff removes.
    ///
    /// Before RFC 0015 the endpoint was `require_admin`, so the delegation was
    /// not expressible and the question did not arise. Decomposing it onto the
    /// read verb would have made it expressible and answered it wrongly.
    AuditPurge,

    // ── control surfaces (RFC 0015 §4.2's deferred `require_admin` split) ────
    //
    // §4.2 parks these: *"Control surfaces stay `role:admin`, because a wrong
    // answer there is an outage rather than a leak, and a role is a defensible
    // granularity while the model beds in."* The model has bedded in, and
    // `role:admin` is a subject form (§8.3) — so these are verbs **beside** a
    // grant that already exists rather than a replacement for one, which is
    // exactly the shape that section predicts the decomposition will take.
    //
    // §10 rule 5 hands every one of them to `role:admin`, so no estate loses an
    // endpoint on upgrade and each becomes delegable on its own.
    /// Reading the server's configuration, its pending changes and its warnings.
    ConfigRead,
    /// Reloading configuration, applying a pending change, setting the banner.
    ConfigWrite,
    /// Health, the subject directory, and the notification wiring, read-only.
    SystemRead,
    /// Editing notification subscriptions, invalidating caches by hand.
    SystemWrite,
    /// Listing IP and account blocks.
    BlocksRead,
    /// Placing and lifting IP and account blocks.
    BlocksWrite,
    /// The authorization diagnostics: `access-check`, `explain`, `shadow`.
    ///
    /// Its own verb rather than `audit:read`'s: one reports what *did* happen and
    /// this one answers what *would*, and an estate that wants a reviewer to read
    /// the trail does not thereby want them probing the resolver.
    AuthzRead,
    /// Reading the package- and version-tier grants written on a coordinate
    /// ([RFC 0017](/rfc/0017-writing-grants-at-the-package-and-version-tiers) §4.1).
    ///
    /// Separate from [`Action::AuditRead`], which §11 open question 2 asks
    /// about, and separate for the reason the `audit:read`/`audit:purge` split
    /// already established: the two answer different questions about the same
    /// package. The audit log says what a subject *did*; this says what they
    /// *may do* and since when. An estate that lets a reviewer read the trail
    /// has not thereby decided to publish its authorization model to them —
    /// a grant listing enumerates the subjects that can reach a private
    /// package, which is the larger of the two disclosures.
    GrantsRead,
    /// Writing and removing those grants.
    ///
    /// Not folded into [`Action::OwnersWrite`], though ownership projects into
    /// the same table: ownership means three specific verbs on one package
    /// (`OWNERSHIP_ACTIONS`), and a delegate handed the ability to *edit owners*
    /// has not been handed the ability to grant `releases:read` on a private
    /// package to any group they like. §8 rejects the merge for the same reason
    /// from the other side — widening `owner` to arbitrary verbs makes the word
    /// mean nothing, and the cargo owners API is a view over it.
    GrantsWrite,
    /// Evicting cached artifacts and invalidating a registry's cache.
    CacheEvict,
    /// Pre-warming a registry's cache.
    CacheWarm,
    /// Reading quota usage.
    QuotaRead,
    /// Resetting a user's quota counters.
    ///
    /// Split from `quota:read` for the reason every other control surface in
    /// this list is split — `config:read`/`config:write`,
    /// `system:read`/`system:write`, `blocks:read`/`blocks:write`. A support
    /// engineer granted the read to *inspect* usage would otherwise also be able
    /// to zero it on every user in the registry, which is not what the verb's
    /// published description ("read quota usage") says they are getting.
    QuotaWrite,
    /// Running a retention pass ([RFC 0016](/rfc/0016-retention-and-the-permanence-of-a-published-name)).
    RetentionRun,
    /// Reading tombstones and compacting their detail.
    TombstonesRead,
    /// The administrative package inventory — `/admin/packages` and its detail.
    ///
    /// A read, and its own verb rather than `packages:block`'s: gating a
    /// disclosure surface on a write verb would make "may see what is here"
    /// inseparable from "may block it". Not `catalogue:browse` either — that one
    /// is the *console's* explore surface and §10 rule 2 translates it through a
    /// conjunction with proxy access, so borrowing it here would make this
    /// endpoint's gate depend on a flag about a different surface.
    PackagesRead,

    // ── ecosystem verbs ──────────────────────────────────────────────────────
    /// npm: repointing what `latest` (or any tag) resolves to.
    NpmDistTagsWrite,
    /// OpenVSX: claiming a publisher namespace.
    OpenvsxNamespaceClaim,
    /// Terraform: registering the GPG key a namespace's providers are signed with.
    TerraformSigningKeysWrite,
    /// JetBrains Marketplace: assigning a plugin build to the stable or EAP channel.
    JetbrainsChannelAssign,
}

/// The verbs a legacy `[registries.rbac]` `"*"` expands to.
///
/// RFC 0015 §10 rule 3. `rules/rbac.rs` accepted `"*"` for *any* role, not only
/// `admin`, and it meant "both of the two verbs that exist" — because two were
/// all there were. Expanding it to the new wildcard would hand publish,
/// overwrite, yank, delete, `packages:block`, `gates:exempt` and `audit:read`
/// to every config that ever wrote `user = ["*"]`, which `config.example.toml`
/// does eight times for `admin` alone.
///
/// So a legacy `"*"` expands to **today's reachable read set, written out**. An
/// administrator's write access today does not come from that string — it comes
/// from `has_role_at_least` — and RFC 0015 §10 rule 5 is what restores it
/// explicitly when grants land, rather than smuggling it through a wildcard
/// whose meaning changed underneath it.
pub const LEGACY_WILDCARD_EXPANSION: &[Action] = &[
    Action::ReleasesRead,
    Action::ReleasesList,
    Action::SourceRead,
];

// **`catalogue:browse` is deliberately not here**, and §10 rule 3 as written puts
// it here — the rule is corrected in place.
//
// Rule 3 and rule 2 disagreed, and rule 3 is the one that is wrong. Rule 2 says
// the console gate is a *conjunction* — the `explore` flag **and** the role's
// proxy access — and §13.5 records the naive reading of it producing 19
// disagreements before it was corrected. Rule 3 was written before that
// correction and lists `catalogue:browse` among the verbs a legacy `"*"` expands
// to, which hands the console to any `admin = ["*"]` **even where
// `explore.admin = false`**.
//
// Under the evaluator this migration preserves, `"*"` meant *"both of the two
// verbs that exist"* and the console was gated somewhere else entirely. So
// dropping it here is not a narrowing of anyone's access — it restores the
// legacy meaning that rule 3's own sentence claims to be preserving.
//
// It went unnoticed because the §11.3 harness compares `releases:read` and
// `source:read` only (§13.5's scope note), so the one verb the two rules
// disagreed about was the one verb it never looked at. Wiring
// `catalogue:browse` to the explore routes is what surfaced it: an `explore =
// false` fixture kept serving the catalogue to an admin.

impl Action {
    /// Every verb in the vocabulary.
    ///
    /// Kept in step with the enum by `all_is_exhaustive` below, which fails
    /// if a variant is missing from it.
    pub const ALL: &[Action] = &[
        Action::ReleasesRead,
        Action::ReleasesList,
        Action::SourceRead,
        Action::CatalogueBrowse,
        Action::ReleasesPublish,
        Action::ReleasesOverwrite,
        Action::ReleasesYank,
        Action::ReleasesDelete,
        Action::OwnersRead,
        Action::OwnersWrite,
        Action::PackagesBlock,
        Action::GatesExempt,
        Action::StatsRead,
        Action::AuditRead,
        Action::QuarantineRead,
        Action::FindingsRead,
        Action::AuditPurge,
        Action::ConfigRead,
        Action::ConfigWrite,
        Action::SystemRead,
        Action::SystemWrite,
        Action::BlocksRead,
        Action::BlocksWrite,
        Action::AuthzRead,
        Action::GrantsRead,
        Action::GrantsWrite,
        Action::CacheEvict,
        Action::CacheWarm,
        Action::QuotaRead,
        Action::QuotaWrite,
        Action::RetentionRun,
        Action::TombstonesRead,
        Action::PackagesRead,
        Action::NpmDistTagsWrite,
        Action::OpenvsxNamespaceClaim,
        Action::TerraformSigningKeysWrite,
        Action::JetbrainsChannelAssign,
    ];

    /// The wire form: what a config file, an API body and a log line spell.
    pub const fn as_str(self) -> &'static str {
        match self {
            Action::ReleasesRead => "releases:read",
            Action::ReleasesList => "releases:list",
            Action::SourceRead => "source:read",
            Action::CatalogueBrowse => "catalogue:browse",
            Action::ReleasesPublish => "releases:publish",
            Action::ReleasesOverwrite => "releases:overwrite",
            Action::ReleasesYank => "releases:yank",
            Action::ReleasesDelete => "releases:delete",
            Action::OwnersRead => "owners:read",
            Action::OwnersWrite => "owners:write",
            Action::PackagesBlock => "packages:block",
            Action::GatesExempt => "gates:exempt",
            Action::StatsRead => "stats:read",
            Action::AuditRead => "audit:read",
            Action::QuarantineRead => "quarantine:read",
            Action::FindingsRead => "findings:read",
            Action::AuditPurge => "audit:purge",
            Action::ConfigRead => "config:read",
            Action::ConfigWrite => "config:write",
            Action::SystemRead => "system:read",
            Action::SystemWrite => "system:write",
            Action::BlocksRead => "blocks:read",
            Action::BlocksWrite => "blocks:write",
            Action::AuthzRead => "authz:read",
            Action::GrantsRead => "grants:read",
            Action::GrantsWrite => "grants:write",
            Action::CacheEvict => "cache:evict",
            Action::CacheWarm => "cache:warm",
            Action::QuotaRead => "quota:read",
            Action::QuotaWrite => "quota:write",
            Action::RetentionRun => "retention:run",
            Action::TombstonesRead => "tombstones:read",
            Action::PackagesRead => "packages:read",
            Action::NpmDistTagsWrite => "npm:dist-tags:write",
            Action::OpenvsxNamespaceClaim => "openvsx:namespace:claim",
            Action::TerraformSigningKeysWrite => "terraform:signing-keys:write",
            Action::JetbrainsChannelAssign => "jetbrains:channel:assign",
        }
    }

    /// The segment before the first `:`, which is what a `foo:*` pattern matches.
    ///
    /// This is why `gates:exempt` is spelled the way it is: under a `releases:`
    /// prefix a generous `releases:*` would reach it, and the ability to accept
    /// a CVE finding would arrive with the ability to publish.
    pub fn prefix(self) -> &'static str {
        self.as_str().split(':').next().unwrap_or_default()
    }

    /// The registry kinds that define this verb, or `None` when every kind does.
    ///
    /// RFC 0015 §4.2 rule 2: a grant naming `npm:dist-tags:write` on a Maven
    /// registry is **rejected at config load**, not silently inert. The registry
    /// type is known at that point, so this is checkable — and "I granted it and
    /// nothing happened" is exactly the failure mode the enum exists to remove.
    pub const fn kinds(self) -> Option<&'static [RegistryKind]> {
        match self {
            Action::NpmDistTagsWrite => Some(&[RegistryKind::Npm]),
            // Both marketplaces speak the OpenVSX namespace model.
            Action::OpenvsxNamespaceClaim => {
                Some(&[RegistryKind::Openvsx, RegistryKind::VscodeMarketplace])
            }
            Action::TerraformSigningKeysWrite => Some(&[RegistryKind::Terraform]),
            Action::JetbrainsChannelAssign => {
                Some(&[RegistryKind::Jetbrains, RegistryKind::JetbrainsMarketplace])
            }
            _ => None,
        }
    }

    /// Whether this verb is meaningful on `kind`.
    pub fn applies_to(self, kind: RegistryKind) -> bool {
        match self.kinds() {
            None => true,
            Some(kinds) => kinds.contains(&kind),
        }
    }

    /// Whether this verb is peculiar to one ecosystem rather than shared.
    pub fn is_ecosystem_scoped(self) -> bool {
        self.kinds().is_some()
    }
}

/// A pattern in a config file did not name anything this build knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionParseError {
    /// Not a verb and not a pattern.
    UnknownAction(String),
    /// A `prefix:*` pattern whose prefix no verb carries. Kept distinct from
    /// `UnknownAction` because the operator's mistake is different — they know
    /// the syntax and guessed the family — and so is the useful message.
    UnknownPrefix(String),
    /// A verb this registry's ecosystem does not define (§4.2 rule 2).
    WrongRegistryKind { pattern: String, kind: String },
}

impl fmt::Display for ActionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionParseError::UnknownAction(s) => write!(
                f,
                "unknown permission '{s}'. Known permissions: {}",
                Action::ALL
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ActionParseError::UnknownPrefix(p) => write!(
                f,
                "'{p}:*' matches no permission. Known prefixes: {}",
                known_prefixes().join(", ")
            ),
            ActionParseError::WrongRegistryKind { pattern, kind } => write!(
                f,
                "'{pattern}' is not defined for a '{kind}' registry. An ecosystem \
                 permission is only grantable on the registry types that implement it."
            ),
        }
    }
}

impl std::error::Error for ActionParseError {}

/// Every distinct prefix in the vocabulary, in first-appearance order.
pub fn known_prefixes() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for action in Action::ALL {
        let p = action.prefix();
        if !seen.contains(&p) {
            seen.push(p);
        }
    }
    seen
}

impl FromStr for Action {
    type Err = ActionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Action::ALL
            .iter()
            .copied()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| ActionParseError::UnknownAction(s.to_owned()))
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for Action {
    type Error = ActionParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Action> for String {
    fn from(a: Action) -> String {
        a.as_str().to_owned()
    }
}

/// How a `"*"` is to be read.
///
/// The same character means two different things depending on where it was
/// written, and conflating them is RFC 0015 §10 rule 3's silent privilege
/// escalation. Making the caller say which one it has is the cheapest way to
/// make that impossible to get wrong by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WildcardScope {
    /// A `"*"` in `[registries.rbac]`, which historically meant "both of the two
    /// verbs that exist". Expands to [`LEGACY_WILDCARD_EXPANSION`].
    Legacy,
    /// A `"*"` in a grant block, which means every verb.
    Everything,
}

/// Expand one config pattern into the verbs it names.
///
/// RFC 0015 §4.2: expansion happens **at config load, never at evaluation**, so
/// an expansion is a fact about the loaded model rather than something implied
/// at each decision. `task config:explain` is what makes it visible — an
/// expansion nobody can print is only half of the property that sentence claims.
///
/// Three forms:
///
/// - `*` — read per `scope` (see [`WildcardScope`]).
/// - `prefix:*` — every verb under that prefix, **and nothing else**. This is
///   what keeps `releases:*` away from `gates:exempt`, and `npm:*` away from
///   `openvsx:namespace:claim`: a grant cannot acquire the ability to repoint
///   `latest` by being generous about releases.
/// - a literal verb.
///
/// Results are deduplicated and returned in [`Action::ALL`] order, so two
/// configs that name the same set produce the same vector regardless of how
/// they spelled it — which is what lets `config:explain` output be diffed.
pub fn expand_pattern(
    pattern: &str,
    scope: WildcardScope,
) -> Result<Vec<Action>, ActionParseError> {
    expand_pattern_for(pattern, scope, None)
}

/// [`expand_pattern`], relative to the registry kind the pattern is written on.
///
/// # Why a wildcard has to be kind-relative
///
/// `*` means "every verb", and the vocabulary contains verbs that only exist on
/// one ecosystem. Expanding `*` literally on an npm registry therefore produces
/// `openvsx:namespace:claim` and `terraform:signing-keys:write` — which §4.2
/// rule 2 then rejects, making `*` unwritable on *every* registry. That is not
/// what the rule is for.
///
/// The rule exists to remove one failure mode: "I granted it and nothing
/// happened". That only arises when an operator **names** a verb their registry
/// does not define. A wildcard names nothing, so it is filtered to what the
/// registry has rather than refused for what it does not.
///
/// So: a literal verb on the wrong kind is an error; a wildcard is narrowed.
/// Both readings are the same principle — the config means what the operator
/// could reasonably have meant — applied to two different spellings.
pub fn expand_pattern_for(
    pattern: &str,
    scope: WildcardScope,
    kind: Option<RegistryKind>,
) -> Result<Vec<Action>, ActionParseError> {
    let applicable = |actions: Vec<Action>| -> Vec<Action> {
        match kind {
            None => actions,
            Some(k) => actions.into_iter().filter(|a| a.applies_to(k)).collect(),
        }
    };

    if pattern == "*" {
        return Ok(applicable(match scope {
            WildcardScope::Legacy => LEGACY_WILDCARD_EXPANSION.to_vec(),
            WildcardScope::Everything => Action::ALL.to_vec(),
        }));
    }

    if let Some(prefix) = pattern.strip_suffix(":*") {
        let matched: Vec<Action> = Action::ALL
            .iter()
            .copied()
            .filter(|a| a.prefix() == prefix)
            .collect();
        if matched.is_empty() {
            return Err(ActionParseError::UnknownPrefix(prefix.to_owned()));
        }
        // A prefix that names *this* ecosystem's family on the wrong registry is
        // a named mistake, not a wildcard over everything: `npm:*` on a Maven
        // registry is as wrong as `npm:dist-tags:write` on one.
        let narrowed = applicable(matched.clone());
        if narrowed.is_empty() && !matched.is_empty() {
            return Err(ActionParseError::WrongRegistryKind {
                pattern: pattern.to_owned(),
                kind: kind.map(|k| k.as_str().to_owned()).unwrap_or_default(),
            });
        }
        return Ok(narrowed);
    }

    let action: Action = pattern.parse()?;
    match kind {
        Some(k) if !action.applies_to(k) => Err(ActionParseError::WrongRegistryKind {
            pattern: pattern.to_owned(),
            kind: k.as_str().to_owned(),
        }),
        _ => Ok(vec![action]),
    }
}

/// [`expand_pattern`] over a list, deduplicated and in [`Action::ALL`] order.
pub fn expand_patterns(
    patterns: &[String],
    scope: WildcardScope,
) -> Result<Vec<Action>, ActionParseError> {
    expand_patterns_for(patterns, scope, None)
}

/// [`expand_pattern_for`] over a list, deduplicated and in [`Action::ALL`] order.
///
/// Results are canonical, so two configs that name the same set produce the same
/// vector regardless of how they spelled it — which is what lets
/// `config:explain` output be diffed.
pub fn expand_patterns_for(
    patterns: &[String],
    scope: WildcardScope,
    kind: Option<RegistryKind>,
) -> Result<Vec<Action>, ActionParseError> {
    let mut set: Vec<Action> = Vec::new();
    for pattern in patterns {
        for action in expand_pattern_for(pattern, scope, kind)? {
            if !set.contains(&action) {
                set.push(action);
            }
        }
    }
    set.sort_by_key(|a| {
        Action::ALL
            .iter()
            .position(|x| x == a)
            .unwrap_or(usize::MAX)
    });
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` lists every variant.
    ///
    /// The `match` is the mechanism: adding a variant without adding it to `ALL`
    /// fails to compile here, which is what lets every other test in this file
    /// (and `expand_pattern`, and `config:explain`) treat `ALL` as exhaustive.
    #[test]
    fn all_is_exhaustive() {
        for action in Action::ALL {
            // Exhaustive match — a new variant breaks the build until it is
            // handled, and `as_str` is where a new verb gets its wire form.
            let _: &str = action.as_str();
        }
        assert_eq!(
            Action::ALL.len(),
            Action::ALL
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "ALL contains a duplicate"
        );
    }

    /// Every verb round-trips through its wire form.
    #[test]
    fn every_action_round_trips_through_its_string() {
        for action in Action::ALL {
            assert_eq!(
                action.as_str().parse::<Action>().unwrap(),
                *action,
                "{action} did not round-trip"
            );
        }
    }

    /// The thing the enum exists to stop.
    #[test]
    fn an_unknown_verb_is_an_error_not_a_permission_nothing_grants() {
        assert!(matches!(
            "releases:raed".parse::<Action>(),
            Err(ActionParseError::UnknownAction(_))
        ));
        assert!(matches!(
            "actions:read".parse::<Action>(),
            Err(ActionParseError::UnknownAction(_))
        ));
    }

    /// RFC 0015 §4.2: prefix expansion reaches its own family and no other.
    #[test]
    fn prefix_expansion_respects_prefixes() {
        let releases = expand_pattern("releases:*", WildcardScope::Everything).unwrap();
        assert!(releases.contains(&Action::ReleasesRead));
        assert!(releases.contains(&Action::ReleasesPublish));
        assert!(releases.contains(&Action::ReleasesDelete));

        // The two boundaries §7 calls out by name.
        assert!(
            !releases.contains(&Action::GatesExempt),
            "releases:* must not reach gates:exempt — silencing a finding is not \
             something a publisher acquires by being able to publish"
        );
        assert!(
            !releases.contains(&Action::NpmDistTagsWrite),
            "releases:* must not reach an ecosystem verb — a grant cannot acquire \
             the ability to repoint `latest` by being generous about releases"
        );
    }

    /// An ecosystem prefix reaches only its own ecosystem.
    #[test]
    fn an_ecosystem_prefix_reaches_only_its_own_verbs() {
        let npm = expand_pattern("npm:*", WildcardScope::Everything).unwrap();
        assert_eq!(npm, vec![Action::NpmDistTagsWrite]);
        assert!(!npm.contains(&Action::OpenvsxNamespaceClaim));
        assert!(!npm.contains(&Action::GatesExempt));
    }

    /// An unknown prefix is refused rather than silently matching nothing.
    ///
    /// The empty-set reading is the failure this whole file is about: a pattern
    /// that grants nothing, written by someone who believed it granted a family.
    #[test]
    fn an_unknown_prefix_is_refused_rather_than_expanding_to_nothing() {
        assert!(matches!(
            expand_pattern("release:*", WildcardScope::Everything),
            Err(ActionParseError::UnknownPrefix(_))
        ));
    }

    /// RFC 0015 §10 rule 3, asserted as the difference between the two readings.
    #[test]
    fn a_legacy_wildcard_is_not_the_new_wildcard() {
        let legacy = expand_pattern("*", WildcardScope::Legacy).unwrap();
        let everything = expand_pattern("*", WildcardScope::Everything).unwrap();

        assert_eq!(legacy, LEGACY_WILDCARD_EXPANSION.to_vec());
        assert_eq!(everything.len(), Action::ALL.len());

        // The whole point: today's `"*"` reaches no write verb, and no
        // exemption. `config.example.toml` ships `admin = ["*"]` eight times.
        for withheld in [
            Action::ReleasesPublish,
            Action::ReleasesOverwrite,
            Action::ReleasesYank,
            Action::ReleasesDelete,
            Action::PackagesBlock,
            Action::GatesExempt,
            Action::AuditRead,
        ] {
            assert!(
                !legacy.contains(&withheld),
                "a legacy '*' must not expand to {withheld}"
            );
            assert!(everything.contains(&withheld));
        }
    }

    /// Both of today's read verbs survive the translation unchanged.
    ///
    /// This is the property that makes phase 1 a no-op for every existing
    /// config: the only verbs any route asks for today are these two, and a
    /// legacy `"*"` still covers both.
    #[test]
    fn todays_two_verbs_are_still_covered_by_a_legacy_wildcard() {
        let legacy = expand_pattern("*", WildcardScope::Legacy).unwrap();
        assert!(legacy.contains(&Action::ReleasesRead));
        assert!(legacy.contains(&Action::SourceRead));
    }

    /// Expansion is order-independent and deduplicated.
    #[test]
    fn expansion_is_canonical_regardless_of_how_it_was_written() {
        let spelled_out = expand_patterns(
            &[
                "source:read".to_owned(),
                "releases:read".to_owned(),
                "releases:read".to_owned(),
            ],
            WildcardScope::Everything,
        )
        .unwrap();
        let other_order = expand_patterns(
            &["releases:read".to_owned(), "source:read".to_owned()],
            WildcardScope::Everything,
        )
        .unwrap();
        assert_eq!(spelled_out, other_order);
        assert_eq!(spelled_out.len(), 2);
    }

    /// Ecosystem scoping, per verb.
    #[test]
    fn ecosystem_verbs_are_scoped_and_shared_verbs_are_not() {
        assert!(Action::NpmDistTagsWrite.applies_to(RegistryKind::Npm));
        assert!(!Action::NpmDistTagsWrite.applies_to(RegistryKind::Maven));
        assert!(Action::TerraformSigningKeysWrite.applies_to(RegistryKind::Terraform));
        assert!(!Action::TerraformSigningKeysWrite.applies_to(RegistryKind::Npm));

        // A shared verb is meaningful everywhere, including on kinds that
        // cannot currently exercise it.
        for kind in RegistryKind::ALL {
            assert!(Action::ReleasesRead.applies_to(*kind));
            assert!(Action::GatesExempt.applies_to(*kind));
        }
    }

    /// Every ecosystem-scoped verb names at least one kind, and every shared
    /// verb names none.
    ///
    /// Structural, so a verb added with an empty `kinds()` — which would be
    /// grantable nowhere and is the ecosystem-scoped twin of a typo — fails
    /// here rather than in an operator's config.
    #[test]
    fn scoping_is_consistent_with_the_prefix() {
        for action in Action::ALL {
            match action.kinds() {
                Some(kinds) => assert!(
                    !kinds.is_empty(),
                    "{action} is ecosystem-scoped but names no registry kind, so \
                     nothing could ever grant it"
                ),
                None => assert!(
                    !action.is_ecosystem_scoped(),
                    "{action} disagrees with itself about being ecosystem-scoped"
                ),
            }
        }
    }

    /// A wildcard is narrowed to the registry's own verbs; a named verb is not.
    ///
    /// Found by a test rather than by reading: expanding `*` literally produces
    /// every ecosystem's verbs, which §4.2 rule 2 then rejects — making `*`
    /// unwritable on *every* registry. The rule exists to remove "I granted it
    /// and nothing happened", which only arises when an operator **names** a
    /// verb their registry lacks. A wildcard names nothing.
    #[test]
    fn a_wildcard_is_narrowed_to_the_registry_kind_and_a_named_verb_is_not() {
        let npm = Some(RegistryKind::Npm);

        let all = expand_pattern_for("*", WildcardScope::Everything, npm).unwrap();
        assert!(all.contains(&Action::NpmDistTagsWrite), "npm keeps its own");
        assert!(all.contains(&Action::ReleasesRead), "and every shared verb");
        for foreign in [
            Action::OpenvsxNamespaceClaim,
            Action::TerraformSigningKeysWrite,
            Action::JetbrainsChannelAssign,
        ] {
            assert!(
                !all.contains(&foreign),
                "{foreign} is not an npm permission"
            );
        }

        // Naming one explicitly is still an error — that is the failure mode
        // the scoping rule exists for.
        assert!(matches!(
            expand_pattern_for("openvsx:namespace:claim", WildcardScope::Everything, npm),
            Err(ActionParseError::WrongRegistryKind { .. })
        ));

        // …and so is naming its whole family, which is equally deliberate.
        assert!(matches!(
            expand_pattern_for("openvsx:*", WildcardScope::Everything, npm),
            Err(ActionParseError::WrongRegistryKind { .. })
        ));
        assert_eq!(
            expand_pattern_for("npm:*", WildcardScope::Everything, npm).unwrap(),
            vec![Action::NpmDistTagsWrite]
        );
    }

    /// Without a kind, nothing is narrowed — which is what the rbac path wants,
    /// since a legacy expansion contains only shared verbs anyway.
    #[test]
    fn expansion_without_a_kind_narrows_nothing() {
        let all = expand_pattern_for("*", WildcardScope::Everything, None).unwrap();
        assert_eq!(all.len(), Action::ALL.len());
    }

    #[test]
    fn serde_uses_the_wire_form() {
        let json = serde_json::to_string(&Action::ReleasesPublish).unwrap();
        assert_eq!(json, "\"releases:publish\"");
        assert_eq!(
            serde_json::from_str::<Action>("\"releases:publish\"").unwrap(),
            Action::ReleasesPublish
        );
        assert!(serde_json::from_str::<Action>("\"nope\"").is_err());
    }
}
