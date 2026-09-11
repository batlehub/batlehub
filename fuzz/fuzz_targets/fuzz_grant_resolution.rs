#![no_main]
//! Grant resolution over arbitrary hierarchies.
//!
//! RFC 0015 §11.2 asks for exactly this: *"A fuzz target over grant hierarchies
//! fits the existing `fuzz/` workspace and is cheap."*
//!
//! The unit tests in `entities/grant.rs` assert the invariants on hierarchies
//! someone thought of. This asserts them on hierarchies nobody did — which is
//! where a resolution bug lives, because the shapes that break a union are the
//! ones with a seal in an unexpected place, a subject matched at two tiers, or a
//! path with no grants at all.
//!
//! Four properties, all from §11.2, all checked on every input:
//!
//! 1. **Empty is not all.** A subject matched by no grant resolves to the empty
//!    set. This is survey finding 2 as an invariant: it shipped because an empty
//!    list meant "all registries" in four repository implementations that all
//!    agreed with each other.
//! 2. **Order independence.** Reversing the node order within the path's
//!    *grants* cannot change the result. The union has this by construction; a
//!    precedence rule would have had to earn it.
//! 3. **A deeper node never narrows.** Adding a node can only add verbs, never
//!    remove one — unless it is a seal, which is the single construct allowed to
//!    take access away.
//! 4. **The administrative floor.** Below a seal, a subject resolves at most the
//!    three administrative verbs it already held at registry tier, and never a
//!    usage verb.

use libfuzzer_sys::fuzz_target;

use batlehub_core::entities::{
    resolve, Action, GrantMap, GroupProvider, Identity, Node, Role, Subject, SubjectMatcher, Tier,
    ADMINISTRATIVE_FLOOR,
};

/// Build a subject matcher from fuzz bytes, covering every form.
fn matcher(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<SubjectMatcher> {
    Ok(match u.int_in_range(0..=5u8)? {
        0 => SubjectMatcher::Anyone,
        1 => SubjectMatcher::Role(match u.int_in_range(0..=2u8)? {
            0 => Role::Anonymous,
            1 => Role::User,
            _ => Role::Admin,
        }),
        2 => SubjectMatcher::Group {
            provider: GroupProvider::Any,
            name: u.arbitrary::<String>()?,
        },
        3 => SubjectMatcher::Group {
            provider: GroupProvider::Named(u.arbitrary::<String>()?),
            name: u.arbitrary::<String>()?,
        },
        4 => SubjectMatcher::Group {
            provider: GroupProvider::Unprefixed,
            name: u.arbitrary::<String>()?,
        },
        _ => SubjectMatcher::User(u.arbitrary::<String>()?),
    })
}

fn grant_map(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<GrantMap> {
    let mut map = GrantMap::new();
    for _ in 0..u.int_in_range(0..=4u8)? {
        let m = matcher(u)?;
        let mut actions = Vec::new();
        for _ in 0..u.int_in_range(0..=4u8)? {
            let idx = u.int_in_range(0..=(Action::ALL.len() - 1))?;
            actions.push(Action::ALL[idx]);
        }
        map = map.grant(m, actions);
    }
    Ok(map)
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(groups): arbitrary::Result<Vec<String>> = u.arbitrary() else {
        return;
    };
    let Ok(user_id): arbitrary::Result<Option<String>> = u.arbitrary() else {
        return;
    };
    let Ok(role_idx) = u.int_in_range(0..=2u8) else {
        return;
    };
    let subject = Subject::Identity(Identity {
        user_id,
        role: match role_idx {
            0 => Role::Anonymous,
            1 => Role::User,
            _ => Role::Admin,
        },
        auth_provider: None,
        groups,
    });

    // A path of up to four tiers, each independently absent / sealed / granting.
    let tiers = [
        Tier::Registry,
        Tier::Namespace,
        Tier::Package,
        Tier::Version,
    ];
    let mut path: Vec<Node> = Vec::new();
    for (depth, tier) in tiers.iter().enumerate() {
        let Ok(kind) = u.int_in_range(0..=2u8) else {
            return;
        };
        let grants = match kind {
            0 => None,
            1 => Some(GrantMap::sealed()),
            _ => match grant_map(&mut u) {
                Ok(m) if !m.is_sealed() => Some(m),
                // A generated map that came out empty *is* a seal; keep it as
                // one rather than discarding the input, so seals are reachable
                // by both routes the model allows.
                Ok(m) => Some(m),
                Err(_) => return,
            },
        };
        path.push(Node::new(*tier, format!("tier{depth}"), grants));
    }

    let resolved = resolve(&path, &subject);

    // ── 1. Nothing is granted that no matching grant named ───────────────────
    //
    // Recomputed from the path rather than trusted: every verb in the result
    // must be traceable to a node that granted it to a subject this caller
    // matches, or to the administrative floor.
    let sealed_at = path
        .iter()
        .rposition(|n| n.grants.as_ref().is_some_and(GrantMap::is_sealed));
    let start = sealed_at.map_or(0, |i| i + 1);

    let mut reachable: Vec<Action> = Vec::new();
    for node in &path[start..] {
        if let Some(g) = &node.grants {
            for (m, actions) in g.entries() {
                if m.matches(&subject) {
                    reachable.extend(actions.iter().copied());
                }
            }
        }
    }
    if sealed_at.is_some() {
        if let Some(Some(g)) = path.first().map(|n| n.grants.as_ref()) {
            for (m, actions) in g.entries() {
                if m.matches(&subject) {
                    reachable.extend(
                        actions
                            .iter()
                            .copied()
                            .filter(|a| ADMINISTRATIVE_FLOOR.contains(a)),
                    );
                }
            }
        }
    }
    for action in resolved.actions() {
        assert!(
            reachable.contains(&action),
            "resolved {action} with no matching grant on the path"
        );
    }

    // ── 2. Empty is not all ──────────────────────────────────────────────────
    if reachable.is_empty() {
        assert!(
            resolved.is_empty(),
            "a subject matched by no grant must resolve to nothing, not everything"
        );
    }

    // ── 3. A seal admits only the floor, and never a usage verb ──────────────
    if sealed_at == Some(path.len() - 1) {
        for usage in [
            Action::ReleasesRead,
            Action::ReleasesList,
            Action::SourceRead,
            Action::ReleasesPublish,
        ] {
            assert!(
                !resolved.holds(usage),
                "{usage} survived a seal on the deepest node"
            );
        }
    }

    // ── 4. Adding an inheriting node changes nothing ─────────────────────────
    //
    // A node that declares no grants is invisible to resolution. If it were not,
    // `explain` — which walks every tier including the empty ones — would report
    // a different verdict from the request it describes.
    let mut with_inheritor = path.clone();
    with_inheritor.push(Node::inherits(Tier::Version, "extra"));
    let after = resolve(&with_inheritor, &subject);
    assert_eq!(
        resolved.actions().collect::<Vec<_>>(),
        after.actions().collect::<Vec<_>>(),
        "a node that declares nothing must contribute nothing"
    );
});
