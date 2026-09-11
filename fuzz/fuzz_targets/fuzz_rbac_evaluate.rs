#![no_main]

use std::collections::HashMap;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use tokio::runtime::Runtime;

use batlehub_core::{
    entities::Action,
    entities::{expand_patterns, Identity, PackageId, PackageMetadata, Role, WildcardScope},
    rules::{RbacRule, Rule, RuleContext, RuleDecision},
};

static RT: OnceLock<Runtime> = OnceLock::new();

fn metadata() -> PackageMetadata {
    PackageMetadata {
        id: PackageId::new("test", "pkg", "1.0"),
        published_at: None,
        download_url: None,
        checksum: None,
        is_signed: None,
        extra: serde_json::Value::Null,
        // Not read by the rule under test — it carries an upstream
        // `Cache-Control`, and this fuzzer is about the decision, not the cache.
        // Written out rather than `..Default::default()` on purpose: an exhaustive
        // literal is what makes a new field on `PackageMetadata` stop this target
        // compiling, and the CI job that builds these bins is what turns that into
        // a prompt rather than silent non-coverage.
        cache_control: None,
    }
}

fn allowed(rt: &Runtime, rule: &RbacRule, role: &Role, groups: &[String], action: Action) -> bool {
    let identity = Identity {
        user_id: None,
        role: role.clone(),
        auth_provider: None,
        groups: groups.to_vec(),
    };
    let meta = metadata();
    let ctx = RuleContext {
        identity: &identity,
        package: &meta,
        action,
        cache_entry: None,
        requested_version: None,
    };
    matches!(rt.block_on(rule.evaluate(&ctx)), RuleDecision::Allow)
}

fuzz_target!(|data: &[u8]| {
    let rt = RT.get_or_init(|| Runtime::new().unwrap());
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(groups): arbitrary::Result<Vec<String>> = u.arbitrary() else {
        return;
    };
    // The fuzzed *string* moved. Until RFC 0015 phase 1 an arbitrary
    // `resource_type` reached `evaluate` directly, and that was the surface
    // worth fuzzing; a closed `Action` makes it unrepresentable there. The
    // arbitrary strings now enter one layer earlier — as config patterns handed
    // to `from_patterns`, which parses and expands them — so that is what this
    // target feeds, and the verb under evaluation is picked from the enum.
    let Ok(patterns): arbitrary::Result<Vec<String>> = u.arbitrary() else {
        return;
    };
    let Ok(action_idx): arbitrary::Result<u8> = u.arbitrary() else {
        return;
    };

    let action = Action::ALL[action_idx as usize % Action::ALL.len()];

    // A malformed pattern is a config-load error, not a panic and not a silent
    // grant — which is the property under test. Returning on `Err` is the
    // assertion: reaching `evaluate` at all means the patterns parsed.
    let Ok(rule) = RbacRule::from_patterns(HashMap::from([
        (Role::Anonymous, vec!["releases:read".to_owned()]),
        (Role::User, patterns.clone()),
        (Role::Admin, vec!["*".to_owned()]),
    ])) else {
        return;
    };
    let Ok(rule) = rule.with_group_patterns(HashMap::from([
        ("*:team-a".to_owned(), vec!["releases:read".to_owned()]),
        ("oidc:team-b".to_owned(), patterns.clone()),
    ])) else {
        return;
    };

    // The reference model. Built from `expand_patterns`, the same function the
    // rule's constructor calls, so what is being checked is not the parser but
    // the decision: role inheritance (a role holds every lower role's verbs)
    // and group matching (an exact key, or `*:<name>` for any provider).
    let user_set = expand_patterns(&patterns, WildcardScope::Legacy).expect("parsed above");
    let admin_set = expand_patterns(&["*".to_owned()], WildcardScope::Legacy).expect("literal");
    let by_role = |role: &Role| -> bool {
        match role {
            Role::Anonymous => action == Action::ReleasesRead,
            Role::User => action == Action::ReleasesRead || user_set.contains(&action),
            Role::Admin => {
                action == Action::ReleasesRead
                    || user_set.contains(&action)
                    || admin_set.contains(&action)
            }
        }
    };
    let by_group = groups.iter().any(|g| {
        let suffix = g.find(':').map(|i| &g[i + 1..]);
        (g == "oidc:team-b" && user_set.contains(&action))
            || (g == "*:team-a" && action == Action::ReleasesRead)
            || (suffix == Some("team-a") && action == Action::ReleasesRead)
    });

    let anon = allowed(rt, &rule, &Role::Anonymous, &groups, action);
    let user = allowed(rt, &rule, &Role::User, &groups, action);
    let admin = allowed(rt, &rule, &Role::Admin, &groups, action);

    // Exact agreement with the model, for every role at once.
    for (role, got) in [
        (Role::Anonymous, anon),
        (Role::User, user),
        (Role::Admin, admin),
    ] {
        assert_eq!(
            got,
            by_role(&role) || by_group,
            "{role:?} × {action} with groups {groups:?} and user patterns {patterns:?}"
        );
    }

    // Two properties that hold whatever the model says. A higher role never
    // loses a verb a lower one holds — otherwise promotion is a demotion.
    assert!(!anon || user, "Anonymous may {action} but User may not");
    assert!(!user || admin, "User may {action} but Admin may not");
    // Membership only ever widens: an identity allowed with no groups is still
    // allowed with any. A group that could *revoke* a role's verb would make
    // the config's group section a deny-list nobody documented.
    for role in [Role::Anonymous, Role::User, Role::Admin] {
        let without = allowed(rt, &rule, &role, &[], action);
        let with = allowed(rt, &rule, &role, &groups, action);
        assert!(
            !without || with,
            "{role:?} loses {action} by joining {groups:?}"
        );
    }
});
