//! `[registries.rbac]` → registry-tier grants.
//!
//! RFC 0015 §10. Every existing config must keep its exact meaning, and the
//! mechanism for believing that is a **translation with a differential test**,
//! not a rewrite: §11.3 runs both evaluators over the cartesian product of every
//! fixture config, every subject shape and every verb, and fails on any
//! disagreement. This module is the translation half.
//!
//! # Why this is the riskiest file in the phase
//!
//! §7 opens with it: *"A translation that widens any existing config is a silent
//! privilege escalation across every deployment."* Nothing here fails loudly.
//! A rule that grants one verb too many produces a server that starts, serves,
//! and is wrong — and the wrongness is invisible unless something compares the
//! two answers.
//!
//! # The rules that are not carries
//!
//! Five of the ten are mechanical. Four are not, and each exists because one of
//! this document's own changes breaks a field-for-field translation in a way
//! that is silent rather than loud:
//!
//! - **Rule 2** — `RbacConfig` has a *fourth* field. `explore` is a per-registry
//!   gate on the console's browse and search surfaces, and it has no target
//!   other than `catalogue:browse`. **It is not translated here**, and the
//!   reason is worth reading before adding it: see "Rule 2 is not this
//!   module's" below.
//! - **Rule 3** — the wildcard's meaning grew. Handled at load in phase 1
//!   ([`WildcardScope`](crate::entities::WildcardScope)), so by the time a
//!   pattern reaches here it is already the four-verb read set.
//! - **Rule 4** — a read verb split. `releases:list` is new, and *both* of
//!   today's verbs authorise some listing document, so both must gain it.
//! - **Rule 5** — today's write authority is expressed nowhere in
//!   `[registries.rbac]` at all, so no reading of that block reproduces it.
//!
//! # Rule 2 is not this module's
//!
//! `explore` looks like a fourth permission list and is not. `RbacRule` has no
//! opinion about it whatsoever: the gate is computed in
//! `server/src/hot_config.rs`, which builds per-role *sets of registries* and
//! then intersects them with proxy access. A role reaches the console only when
//! **both** hold —
//!
//! ```text
//! (has_anonymous || has_group) && rbac.explore.anonymous
//! (has_user      || has_group) && rbac.explore.user
//! (has_admin     || has_group) && rbac.explore.admin
//! ```
//!
//! — with the role tiers cumulative, and then only for a caller whose
//! `accessible_registries_for` already contains the registry.
//!
//! A first attempt at this module read §10 rule 2 as "flag true → grant
//! `catalogue:browse` to that role" and the §11.3 harness immediately reported
//! **19 widenings**: every fixture where a role has the flag set but no proxy
//! access of its own. The flag alone was never sufficient.
//!
//! Translating it correctly needs the inputs `AccessConfig` is built from, which
//! live in the server crate, so rule 2 lands with the config wiring rather than
//! here. Leaving it out is a *narrowing* of an unwired library rather than a
//! widening of a running server — the translation is not yet on any request
//! path — and it keeps the harness's claim exactly co-extensive with what this
//! module does. `translate_rbac` therefore never emits `catalogue:browse`, and
//! [`explore_grants_are_not_this_modules_job`] pins that so it is a decision
//! rather than an omission.

use std::collections::HashMap;

use crate::entities::{
    Action, GrantMap, GroupProvider, Node, RegistryGrants, RegistryKind, Role, SubjectMatcher,
    SubjectParseError, Tier,
};

/// Whether a registry accepts publishes, which decides rule 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// `local` or `hybrid` — publishes land here.
    Accepts,
    /// `proxy` — nothing is published, so none of today's write authority
    /// exists to translate.
    Refuses,
}

/// The four `[registries.rbac.explore]` flags, as booleans per role.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExploreFlags {
    pub anonymous: bool,
    pub user: bool,
    pub admin: bool,
}

/// Everything the translation reads, decoupled from `batlehub-config` so `core`
/// does not depend on the config crate.
#[derive(Debug, Clone, Default)]
pub struct RbacSnapshot {
    /// Already expanded by phase 1, so rule 3 is applied before this point.
    pub anonymous: Vec<Action>,
    pub user: Vec<Action>,
    pub admin: Vec<Action>,
    /// Keyed exactly as the config file writes them.
    pub groups: HashMap<String, Vec<Action>>,
    pub explore: ExploreFlags,
}

/// A `[[registries.namespaces]]` block, reduced to what grant-building needs.
///
/// The config type lives in `batlehub-config`, which `core` does not depend on,
/// and phase 4's `visibility`/`versioning`/`rules` are not here because they are
/// not grants.
#[derive(Debug, Clone)]
pub struct NamespaceSpec {
    pub match_prefix: String,
    /// `None` inherits; `Some(empty)` seals; `Some(non-empty)` adds.
    pub grants: Option<GrantMap>,
    /// RFC 0015 §4.7 — this namespace's grants are in shadow until the date.
    pub shadow: Option<crate::entities::DryRun>,
}

/// Build a registry's whole grant hierarchy: the registry node and its
/// namespaces.
///
/// This is the one builder. `server` adapts a `RegistryConfig` into its
/// arguments and the web test fixtures build them directly, so both reach the
/// same code — which matters more than it sounds. When `RbacRule` came out of
/// the chain (§5.1), a fixture that kept building the rule while production
/// resolved grants would have gone on passing while testing a path nobody runs.
pub fn build_grants(
    registry: &str,
    kind: RegistryKind,
    rbac: &RbacSnapshot,
    explicit: Option<&GrantMap>,
    namespaces: &[NamespaceSpec],
    write_mode: WriteMode,
    registry_shadow: Option<crate::entities::DryRun>,
) -> Result<RegistryGrants, SubjectParseError> {
    let mut registry_grants = translate_rbac(rbac, write_mode)?;

    // §10 rule 2, with the half `translate_rbac` cannot see.
    for (subject, browses) in explore_subjects(rbac) {
        if browses {
            registry_grants = registry_grants.grant(subject, [Action::CatalogueBrowse]);
        }
    }

    if let Some(explicit) = explicit {
        for (subject, actions) in explicit.entries() {
            registry_grants = registry_grants.grant(subject.clone(), actions.iter().copied());
        }
    }

    Ok(RegistryGrants {
        kind,
        registry: Node::new(
            Tier::Registry,
            format!("registry:{registry}"),
            Some(registry_grants),
        )
        .shadowed(registry_shadow),
        namespaces: namespaces
            .iter()
            .map(|ns| {
                (
                    ns.match_prefix.clone(),
                    Node::new(
                        Tier::Namespace,
                        format!("namespace:{}", ns.match_prefix),
                        ns.grants.clone(),
                    )
                    .shadowed(ns.shadow.clone()),
                )
            })
            .collect(),
    })
}

/// Which subjects reach the console on this registry, per §10 rule 2's
/// conjunction.
///
/// A direct transcription of `hot_config::compute_access`, deliberately: the two
/// have to agree, and the way to make that checkable is for this to read like
/// the thing it mirrors rather than like a cleverer version of it.
///
/// The flag alone was never sufficient — implementing it that way produced 19
/// disagreements the first time the §11.3 harness looked (§13.5).
fn explore_subjects(rbac: &RbacSnapshot) -> Vec<(SubjectMatcher, bool)> {
    let has_anonymous = !rbac.anonymous.is_empty();
    let has_user = has_anonymous || !rbac.user.is_empty();
    let has_admin = has_user || !rbac.admin.is_empty();
    // A registry reachable only through `[registries.rbac.groups]` has all three
    // role tiers empty, and its members do have proxy access — so the group case
    // widens each tier, exactly as `compute_access` does.
    let has_group = !rbac.groups.is_empty();

    vec![
        (
            SubjectMatcher::Anyone,
            (has_anonymous || has_group) && rbac.explore.anonymous,
        ),
        (
            SubjectMatcher::Role(Role::User),
            (has_user || has_group) && rbac.explore.user,
        ),
        (
            SubjectMatcher::Role(Role::Admin),
            (has_admin || has_group) && rbac.explore.admin,
        ),
    ]
}

/// Translate one registry's `[registries.rbac]` into its registry-tier grants.
pub fn translate_rbac(
    rbac: &RbacSnapshot,
    write_mode: WriteMode,
) -> Result<GrantMap, SubjectParseError> {
    let mut map = GrantMap::new();

    // ── Rule 1: the three role fields ────────────────────────────────────────
    //
    // `anonymous` becomes `*` rather than `role:anonymous`, which is what §10
    // writes and is also what today's role-inheritance walk means: a permission
    // listed under `anonymous` is held by everyone, because `is_permitted`
    // checks Anonymous for a User and for an Admin too.
    map = map.grant(SubjectMatcher::Anyone, with_list(&rbac.anonymous));
    map = map.grant(SubjectMatcher::Role(Role::User), with_list(&rbac.user));
    map = map.grant(SubjectMatcher::Role(Role::Admin), with_list(&rbac.admin));

    // ── Rule 1, groups ───────────────────────────────────────────────────────
    //
    // The key is translated by *shape*, not normalised. See `GroupProvider`: a
    // bare key and a `*:` key match different things today, and merging them
    // widens every config that uses the bare form.
    for (key, actions) in &rbac.groups {
        map = map.grant(group_subject(key)?, with_list(actions));
    }

    // Rule 2 (`explore` → `catalogue:browse`) is deliberately absent. See the
    // module docs: the flag is only half of the condition, and the other half
    // lives in `AccessConfig`.

    // ── Rule 5: today's write authority, written out ─────────────────────────
    //
    // Publish is `has_role_at_least(&Role::User)` at `publish.rs:151`, and yank,
    // unyank, unlist and delete are the same check at six sites in
    // `lifecycle.rs` — none of it expressed in `[registries.rbac]`, so no
    // reading of that block reproduces it. It has to be written out, or the
    // translation takes away authority every local registry has today.
    if write_mode == WriteMode::Accepts {
        map = map.grant(
            SubjectMatcher::Role(Role::User),
            [
                Action::ReleasesPublish,
                Action::ReleasesOverwrite,
                Action::ReleasesYank,
                Action::ReleasesDelete,
            ],
        );
    }

    // `require_admin` today, so `role:admin` and nothing wider. `stats.rs:72`
    // is among them, which is why the dashboard stays admin-only on upgrade and
    // only becomes grantable when an operator writes the grant.
    //
    // `gates:exempt` goes to nobody: it is new, and §4.2's shadow release is how
    // an estate discovers it needs one.
    map = map.grant(
        SubjectMatcher::Role(Role::Admin),
        [
            Action::PackagesBlock,
            Action::OwnersRead,
            Action::OwnersWrite,
            Action::StatsRead,
            Action::AuditRead,
        ],
    );

    // RFC 0018 §4.1: `quarantine:read` for `user` and `admin`, `findings:read`
    // for `admin`. So adding a `[security]` section does not hide errors from
    // existing developers, and the findings — which can name an embargoed
    // advisory or a SOC case — stay with the administrator. `anonymous` gets
    // neither: on a public mirror a held version is a plain 404 unless the
    // operator grants it.
    map = map.grant(SubjectMatcher::Role(Role::User), [Action::QuarantineRead]);
    // `flags:read` (RFC 0002 §13) goes with `findings:read`: a pushed flag
    // names its source, which is the same disclosure as a SOC case in a
    // finding, and the same reader.
    map = map.grant(
        SubjectMatcher::Role(Role::Admin),
        [
            Action::QuarantineRead,
            Action::FindingsRead,
            Action::FlagsRead,
        ],
    );

    // The registry-scoped half of §4.2's deferred `require_admin` split. Every
    // one of these guards an endpoint that is `require_admin` today, so granting
    // them to `role:admin` is what makes the decomposition a **rename of who
    // decides** rather than a change to who is allowed — §10's promise, applied
    // to twelve verbs at once.
    map = map.grant(
        SubjectMatcher::Role(Role::Admin),
        [
            Action::CacheEvict,
            Action::CacheWarm,
            Action::QuotaRead,
            Action::QuotaWrite,
            Action::RetentionRun,
            Action::TombstonesRead,
            Action::PackagesRead,
        ],
    );

    Ok(map)
}

/// The instance node §10 rule 5 implies for the control surfaces that name no
/// registry.
///
/// `config:*`, `system:*`, `blocks:*` and `authz:read` guard endpoints that are
/// `require_admin` today and have no registry in their path, so the tier they
/// attach to is the one above every registry (§4.1). Granting them to
/// `role:admin` here is what keeps every existing deployment's admin able to
/// reach exactly what they could reach before.
///
/// `explicit` is any `[grants]` block written at the top level of the config,
/// unioned on top — the same reading `build_grants` gives a registry's own
/// block, because a grant only ever adds.
pub fn instance_node(explicit: Option<&GrantMap>) -> Node {
    // **All twelve, not only the seven that name no registry.** The five
    // registry-scoped control verbs are granted here as well as at registry tier,
    // and that is what makes the two scopes composable rather than a fork: a
    // control endpoint that can name its registry checks the instance tier *and*
    // that registry, and one that cannot checks the instance tier alone. An
    // administrator passes either way, because rule 5 puts them at the top; a
    // delegate granted `cache:evict` on one registry passes only the first.
    //
    // Without the instance copy, an endpoint that could not cheaply reach its
    // registry name would refuse the administrator it has always served — the
    // decomposition breaking exactly what §10 promises it will not.
    let mut map = GrantMap::new().grant(
        SubjectMatcher::Role(Role::Admin),
        [
            Action::ConfigRead,
            Action::ConfigWrite,
            Action::SystemRead,
            Action::SystemWrite,
            Action::BlocksRead,
            Action::BlocksWrite,
            Action::AuthzRead,
            // RFC 0017 §4.1 — the grants editor's own two verbs, held here for
            // the same reason as every other control verb: rule 5 is what stops
            // an endpoint disappearing for administrators on upgrade. There is
            // no `require_admin` predecessor to preserve — the surface is new —
            // but an administrator who could already edit ownership, which
            // writes package-tier rows, must not need a config change to reach
            // the editor that writes the rest of them.
            Action::GrantsRead,
            Action::GrantsWrite,
            Action::CacheEvict,
            Action::CacheWarm,
            Action::QuotaRead,
            Action::QuotaWrite,
            Action::RetentionRun,
            Action::TombstonesRead,
            Action::PackagesRead,
            // The three §4.2 already split out of `require_admin`, so an
            // instance-wide endpoint that reads them (the audit log spans every
            // registry) resolves for an administrator without naming one.
            Action::AuditRead,
            Action::AuditPurge,
            Action::StatsRead,
            Action::PackagesBlock,
            // RFC 0002 (recast): the flag listing spans every registry too.
            Action::FlagsRead,
            // The governance and lifecycle verbs the admin API's own endpoints
            // ask for. `require_admin` conferred these on every registry,
            // including ones with no hierarchy of their own, so an administrator
            // has to hold them at the tier that speaks for all of them or the
            // decomposition takes away an endpoint §10 promises to preserve.
            //
            // Not a widening: rule 5 already grants `owners:read`/`owners:write`
            // to `role:admin` on every registry, and `releases:yank`/
            // `releases:delete` on every registry that accepts writes — a proxy
            // registry holds no local version for either to reach.
            Action::OwnersRead,
            Action::OwnersWrite,
            Action::ReleasesYank,
            Action::ReleasesDelete,
            // The three ecosystem verbs §4.2 named as having no equivalent
            // elsewhere, and which this branch implemented: registering a
            // Terraform namespace's signing key, assigning a JetBrains plugin
            // build to a channel, claiming an OpenVSX namespace.
            //
            // They are here because **no translation rule produces them**. They
            // are new verbs with no legacy config that means them, so a floor is
            // the only thing that can hold them — and without one the endpoints
            // are unreachable for every estate, answering `403` to the
            // administrator as readily as to anonymous. A feature nobody can
            // invoke is not a shipped feature, which is what the positive
            // control in `openvsx_namespace_claim_admin_claims_and_the_claim_is_stored`
            // caught.
            //
            // Administrative rather than self-service is the deliberate half.
            // OpenVSX upstream lets any signed-in account claim a free
            // namespace; here a namespace is the tier grants are written on, so
            // self-service claiming would let a user mint the scope their own
            // permissions are then read from. §4.3's delegation bounds: an
            // estate that wants upstream's behaviour grants
            // `openvsx:namespace:claim` to `role:user`, and that grant is the
            // record of the decision.
            //
            // Unlike `gates:exempt` below, granting these to `role:admin` takes
            // nothing away and hands over nothing that was previously withheld:
            // there was no way to do any of these three before this branch, so
            // there is no prior behaviour for the floor to contradict.
            Action::TerraformSigningKeysWrite,
            Action::JetbrainsChannelAssign,
            Action::OpenvsxNamespaceClaim,
            // `gates:exempt` is **deliberately absent**. §4.5: it "goes to
            // nobody: it is new, and §4.2's shadow release is how an estate
            // discovers it needs one", and §13.6 records the exemption endpoint
            // being the one handler in its module that is *not* `require_admin`
            // precisely so the grant is not decorative. Adding it here would
            // undo that in one line.
        ],
    );
    if let Some(explicit) = explicit {
        for (subject, actions) in explicit.entries() {
            map = map.grant(subject.clone(), actions.iter().copied());
        }
    }
    Node::new(Tier::Instance, "instance", Some(map))
}

/// Rule 4: both of today's read verbs gain `releases:list` together.
///
/// A listing names no single version, and *both* existing verbs authorise some
/// listing document today — handlers pass `releases:read` for the npm packument,
/// the NuGet flat index and Composer metadata, while the cargo sparse index goes
/// out under `source:read`. Splitting the new verb out of only one of them would
/// take working access away from whichever estates granted the other, and which
/// one that is varies by ecosystem rather than by intent.
fn with_list(actions: &[Action]) -> Vec<Action> {
    let mut out = actions.to_vec();
    let authorises_a_listing_today =
        out.contains(&Action::ReleasesRead) || out.contains(&Action::SourceRead);
    if authorises_a_listing_today && !out.contains(&Action::ReleasesList) {
        out.push(Action::ReleasesList);
    }
    out
}

/// A `[registries.rbac.groups]` key, as the subject that matches exactly what it
/// matches today.
fn group_subject(key: &str) -> Result<SubjectMatcher, SubjectParseError> {
    let (provider, name) = match key.split_once(':') {
        Some(("*", name)) => (GroupProvider::Any, name),
        Some((p, name)) => (GroupProvider::Named(p.to_owned()), name),
        None => (GroupProvider::Unprefixed, key),
    };
    if name.is_empty() {
        return Err(SubjectParseError(key.to_owned()));
    }
    Ok(SubjectMatcher::Group {
        provider,
        name: name.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{resolve, Identity, Node, Subject, Tier};

    fn subject(role: Role, groups: &[&str]) -> Subject {
        Subject::Identity(Identity {
            user_id: Some("u".to_owned()),
            role,
            auth_provider: None,
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
        })
    }

    fn holds(map: &GrantMap, subj: &Subject, action: Action) -> bool {
        let path = [Node::new(Tier::Registry, "registry:reg", Some(map.clone()))];
        resolve(&path, subj).holds(action)
    }

    fn snapshot() -> RbacSnapshot {
        RbacSnapshot {
            anonymous: vec![],
            user: vec![Action::ReleasesRead, Action::SourceRead],
            admin: crate::entities::LEGACY_WILDCARD_EXPANSION.to_vec(),
            ..Default::default()
        }
    }

    /// Rule 1: the three role fields land on the three subject forms, with
    /// inheritance intact.
    #[test]
    fn the_role_fields_translate_with_inheritance() {
        let map = translate_rbac(&snapshot(), WriteMode::Refuses).unwrap();
        assert!(holds(&map, &subject(Role::User, &[]), Action::ReleasesRead));
        assert!(holds(
            &map,
            &subject(Role::Admin, &[]),
            Action::ReleasesRead
        ));
        assert!(!holds(
            &map,
            &subject(Role::Anonymous, &[]),
            Action::ReleasesRead
        ));
    }

    /// Rule 4: both read verbs gain `releases:list`, and a subject holding
    /// neither does not.
    ///
    /// Asserted from **both** sides, because §11.3 names a fixture for each: a
    /// registry granting `source:read` but not `releases:read`, and one granting
    /// the reverse. Each must reach the listing documents its own verb reaches
    /// today, and the cargo sparse index is the coordinate where the two
    /// disagree.
    #[test]
    fn both_read_verbs_gain_releases_list() {
        let only_releases = RbacSnapshot {
            user: vec![Action::ReleasesRead],
            ..Default::default()
        };
        let only_source = RbacSnapshot {
            user: vec![Action::SourceRead],
            ..Default::default()
        };
        for snap in [only_releases, only_source] {
            let map = translate_rbac(&snap, WriteMode::Refuses).unwrap();
            assert!(holds(&map, &subject(Role::User, &[]), Action::ReleasesList));
        }

        let neither = RbacSnapshot {
            user: vec![Action::CatalogueBrowse],
            ..Default::default()
        };
        let map = translate_rbac(&neither, WriteMode::Refuses).unwrap();
        assert!(!holds(
            &map,
            &subject(Role::User, &[]),
            Action::ReleasesList
        ));
    }

    /// Rule 2 is deferred, and this pins that it is deferred rather than
    /// forgotten.
    ///
    /// The flags are carried on [`RbacSnapshot`] so the config wiring has them,
    /// and are deliberately not read: `explore` alone never granted console
    /// access, and translating it as though it did widened 19 fixture/subject
    /// combinations the moment the §11.3 harness looked. Whoever wires
    /// `AccessConfig` should delete this test and assert the conjunction
    /// instead.
    #[test]
    fn explore_grants_are_not_this_modules_job() {
        let snap = RbacSnapshot {
            user: vec![Action::ReleasesRead],
            explore: ExploreFlags {
                anonymous: true,
                user: true,
                admin: true,
            },
            ..Default::default()
        };
        let map = translate_rbac(&snap, WriteMode::Refuses).unwrap();
        for role in [Role::Anonymous, Role::User, Role::Admin] {
            assert!(
                !holds(&map, &subject(role.clone(), &[]), Action::CatalogueBrowse),
                "translate_rbac must not emit catalogue:browse for {role} — the \
                 explore flag is only half the condition"
            );
        }
    }

    /// Rule 5: write authority appears on registries that accept publishes and
    /// nowhere else.
    #[test]
    fn write_verbs_are_granted_only_where_publishes_land() {
        let local = translate_rbac(&snapshot(), WriteMode::Accepts).unwrap();
        let proxy = translate_rbac(&snapshot(), WriteMode::Refuses).unwrap();
        let user = subject(Role::User, &[]);

        for verb in [
            Action::ReleasesPublish,
            Action::ReleasesOverwrite,
            Action::ReleasesYank,
            Action::ReleasesDelete,
        ] {
            assert!(holds(&local, &user, verb), "{verb} on a local registry");
            assert!(!holds(&proxy, &user, verb), "{verb} on a proxy registry");
        }
    }

    /// The admin-only surfaces stay admin-only, and `gates:exempt` goes to
    /// nobody.
    #[test]
    fn require_admin_surfaces_translate_to_role_admin_and_no_wider() {
        let map = translate_rbac(&snapshot(), WriteMode::Accepts).unwrap();
        let user = subject(Role::User, &[]);
        let admin = subject(Role::Admin, &[]);

        for verb in [
            Action::PackagesBlock,
            Action::StatsRead,
            Action::AuditRead,
            Action::OwnersWrite,
        ] {
            assert!(holds(&map, &admin, verb), "{verb} for an admin");
            assert!(!holds(&map, &user, verb), "{verb} must stay admin-only");
        }

        assert!(
            !holds(&map, &admin, Action::GatesExempt),
            "gates:exempt is new and goes to nobody — §4.2's shadow release is \
             how an estate discovers it needs one"
        );
    }

    /// Group keys keep matching exactly what they match today.
    ///
    /// The bare-key row is the one that matters: translating `eng` to
    /// `group:*:eng` would make it start matching `oidc1:eng` on every
    /// deployment, which is §7's silent privilege escalation.
    #[test]
    fn group_keys_translate_by_shape_not_by_normalisation() {
        let snap = RbacSnapshot {
            groups: HashMap::from([
                ("eng".to_owned(), vec![Action::ReleasesRead]),
                ("*:qa".to_owned(), vec![Action::SourceRead]),
                ("oidc1:ops".to_owned(), vec![Action::CatalogueBrowse]),
            ]),
            ..Default::default()
        };
        let map = translate_rbac(&snap, WriteMode::Refuses).unwrap();

        // bare key → bare group only
        assert!(holds(
            &map,
            &subject(Role::Anonymous, &["eng"]),
            Action::ReleasesRead
        ));
        assert!(!holds(
            &map,
            &subject(Role::Anonymous, &["oidc1:eng"]),
            Action::ReleasesRead
        ));

        // `*:` key → any provider, but not a bare group
        assert!(holds(
            &map,
            &subject(Role::Anonymous, &["oidc9:qa"]),
            Action::SourceRead
        ));
        assert!(!holds(
            &map,
            &subject(Role::Anonymous, &["qa"]),
            Action::SourceRead
        ));

        // exact key → that provider only
        assert!(holds(
            &map,
            &subject(Role::Anonymous, &["oidc1:ops"]),
            Action::CatalogueBrowse
        ));
        assert!(!holds(
            &map,
            &subject(Role::Anonymous, &["oidc2:ops"]),
            Action::CatalogueBrowse
        ));
    }

    /// A group grant also picks up rule 4.
    #[test]
    fn a_group_grant_gains_releases_list_too() {
        let snap = RbacSnapshot {
            groups: HashMap::from([("eng".to_owned(), vec![Action::ReleasesRead])]),
            ..Default::default()
        };
        let map = translate_rbac(&snap, WriteMode::Refuses).unwrap();
        assert!(holds(
            &map,
            &subject(Role::Anonymous, &["eng"]),
            Action::ReleasesList
        ));
    }

    /// An empty `[registries.rbac]` translates to a map that grants an ordinary
    /// caller nothing.
    ///
    /// The admin row is not empty — rule 5 puts the `require_admin` surfaces
    /// there — so "translates to nothing" has to be asserted about the subject
    /// it is true of, or it is not asserted at all.
    #[test]
    fn an_empty_rbac_block_grants_an_ordinary_caller_nothing() {
        let map = translate_rbac(&RbacSnapshot::default(), WriteMode::Refuses).unwrap();
        let path = [Node::new(Tier::Registry, "registry:reg", Some(map))];
        assert!(resolve(&path, &subject(Role::Anonymous, &[])).is_empty());
    }
}

#[cfg(test)]
mod control_surface_tests {
    use super::*;
    use crate::entities::{resolve, Identity, Node, Subject, Tier};

    fn subject(role: Role) -> Subject {
        Subject::Identity(Identity {
            user_id: Some("u".to_owned()),
            role,
            auth_provider: None,
            groups: vec![],
        })
    }

    fn holds(node: &Node, role: Role, action: Action) -> bool {
        resolve(std::slice::from_ref(node), &subject(role)).holds(action)
    }

    /// §10's promise, for the twelve verbs that replaced `require_admin`: an
    /// administrator reaches on upgrade exactly what they reached before.
    ///
    /// Asserted over the whole control vocabulary rather than a sample, because
    /// the failure mode is one verb missing from the translation and the endpoint
    /// it guards refusing every caller — an outage that no other test in this
    /// file would see.
    #[test]
    fn every_control_verb_reaches_role_admin_at_the_instance_tier() {
        let node = instance_node(None);
        for action in [
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
            Action::PackagesBlock,
            Action::AuditRead,
            Action::AuditPurge,
            Action::StatsRead,
            Action::OwnersRead,
            Action::OwnersWrite,
            Action::ReleasesYank,
            Action::ReleasesDelete,
        ] {
            assert!(
                holds(&node, Role::Admin, action),
                "{action} guards an endpoint that was `require_admin`; without it at \
                 the instance tier an administrator loses that endpoint on upgrade"
            );
        }
    }

    /// …and nobody else reaches any of them.
    ///
    /// The other half of the same promise. `require_admin` refused a `role:user`,
    /// so the translation that replaces it has to as well — an instance grant is
    /// something an operator writes, never something the migration assumes.
    #[test]
    fn no_control_verb_reaches_role_user_by_translation() {
        let node = instance_node(None);
        for action in Action::ALL {
            assert!(
                !holds(&node, Role::User, *action),
                "{action} must not reach `role:user` at the instance tier: \
                 `require_admin` refused one and §10 keeps every config's meaning"
            );
            assert!(!holds(&node, Role::Anonymous, *action));
        }
    }

    /// §4.5: *"`gates:exempt` goes to nobody: it is new, and §4.2's shadow
    /// release is how an estate discovers it needs one."*
    ///
    /// The instance node is where that is easiest to undo by accident — it grants
    /// an administrator every other control verb, and one more line would look
    /// like consistency. §13.6 records the exemption endpoint being deliberately
    /// *not* `require_admin` so the grant is not decorative; this keeps it that
    /// way from the other side.
    #[test]
    fn gates_exempt_reaches_nobody_not_even_an_administrator() {
        let node = instance_node(None);
        assert!(!holds(&node, Role::Admin, Action::GatesExempt));
    }

    /// The escalation an existing test caught, as a unit assertion.
    ///
    /// §10 rule 5 grants `releases:yank` and `releases:delete` to `role:user` at
    /// **registry** tier, because that is what `has_role_at_least(&Role::User)`
    /// meant on the per-package lifecycle path. The administrative bulk endpoints
    /// use the same verbs and were `require_admin`, so they must resolve at the
    /// instance tier — checking them against the registry would hand every user
    /// an admin surface. This pins the asymmetry the handlers depend on.
    #[test]
    fn the_lifecycle_verbs_reach_role_user_at_registry_tier_and_not_at_instance() {
        let registry = Node::new(
            Tier::Registry,
            "registry:npm",
            Some(translate_rbac(&RbacSnapshot::default(), WriteMode::Accepts).unwrap()),
        );
        let instance = instance_node(None);
        for action in [Action::ReleasesYank, Action::ReleasesDelete] {
            assert!(
                holds(&registry, Role::User, action),
                "rule 5 gives {action} to role:user on a registry that accepts writes"
            );
            assert!(
                !holds(&instance, Role::User, action),
                "…and not at the instance tier, which is what keeps the admin bulk \
                 endpoints admin-only"
            );
        }
    }

    /// A proxy-mode registry grants no write verb, so a control endpoint scoped
    /// to one cannot be reached through rule 5 by anybody but an administrator.
    #[test]
    fn a_proxy_registry_grants_no_write_verb_to_role_user() {
        let node = Node::new(
            Tier::Registry,
            "registry:proxy",
            Some(translate_rbac(&RbacSnapshot::default(), WriteMode::Refuses).unwrap()),
        );
        for action in [
            Action::ReleasesPublish,
            Action::ReleasesYank,
            Action::ReleasesDelete,
            Action::ReleasesOverwrite,
        ] {
            assert!(!holds(&node, Role::User, action), "{action}");
        }
    }
}
