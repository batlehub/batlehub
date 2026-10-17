#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use tokio::runtime::Runtime;

use batlehub_core::{
    entities::Action,
    entities::{Identity, PackageId, PackageMetadata, Role},
    rules::{DenyLatestRule, Rule, RuleContext, RuleDecision},
};

static RT: OnceLock<Runtime> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    let rt = RT.get_or_init(|| Runtime::new().unwrap());
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(version_str): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(role_idx): arbitrary::Result<u8> = u.arbitrary() else {
        return;
    };

    let role = match role_idx % 3 {
        0 => Role::Anonymous,
        1 => Role::User,
        _ => Role::Admin,
    };

    // The bypass list is an arbitrary subset of the roles — empty included,
    // which is its own branch (nobody bypasses) — so the "least-privileged
    // listed role is the bar" comparison is exercised from every side, not
    // only from `[Admin]`.
    let Ok(bypass_bits): arbitrary::Result<u8> = u.arbitrary() else {
        return;
    };
    let bypass: Vec<Role> = [Role::Anonymous, Role::User, Role::Admin]
        .into_iter()
        .enumerate()
        .filter(|(i, _)| bypass_bits & (1 << i) != 0)
        .map(|(_, r)| r)
        .collect();
    let rule = DenyLatestRule::new(bypass.clone());
    // A request with no version at all (a listing, a name-only lookup) is not a
    // request for "latest", whatever the package's own version string says.
    let Ok(has_requested): arbitrary::Result<bool> = u.arbitrary() else {
        return;
    };

    let identity = Identity {
        user_id: None,
        role,
        auth_provider: None,
        groups: vec![],
    };
    let meta = PackageMetadata {
        id: PackageId::new("npm", "pkg", &version_str),
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
    };
    let ctx = RuleContext {
        identity: &identity,
        package: &meta,
        action: Action::ReleasesRead,
        cache_entry: None,
        requested_version: has_requested.then_some(version_str.as_str()),
    };

    let decision = rt.block_on(rule.evaluate(&ctx));

    // Security invariant: only the exact string "latest" triggers a deny.
    // Unicode homoglyphs or whitespace variations must NOT bypass the deny
    // and must NOT accidentally block legitimate version strings.
    //
    // The oracle has to model the bypass list the rule was built with, or it
    // contradicts the rule's own unit tests (`bypass_role_allows_admin`): an
    // admin asking for "latest" is *allowed*, that is what the list is for.
    // The first input libFuzzer ever ran through this target in CI decoded to
    // exactly that case and was reported as a finding.
    //
    // Restated as "at or above *some* listed role" rather than the rule's
    // "at or above the minimum", so the two are independent spellings of RFC
    // 0015 §5.1's bar.
    if has_requested && version_str == "latest" {
        let bypassed = bypass.iter().any(|b| identity.has_role_at_least(b));
        if bypassed {
            assert!(
                matches!(decision, RuleDecision::Allow),
                "{:?} is at or above a bypass role in {bypass:?} and must be allowed \"latest\"",
                identity.role
            );
        } else {
            assert!(
                decision.is_deny(),
                "\"latest\" must be blocked for {:?}, below every bypass role in {bypass:?}",
                identity.role
            );
        }
    } else {
        assert!(
            matches!(decision, RuleDecision::Allow),
            "non-\"latest\" request {:?} must always be allowed",
            has_requested.then_some(&version_str)
        );
    }
});
