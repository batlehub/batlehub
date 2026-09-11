#![no_main]

use std::sync::OnceLock;
use std::time::Duration;

use chrono::Utc;
use libfuzzer_sys::fuzz_target;
use tokio::runtime::Runtime;

use batlehub_core::{
    entities::Action,
    entities::{Identity, PackageId, PackageMetadata, Role},
    rules::{ReleaseAgeGateRule, Rule, RuleContext, RuleDecision},
};

static RT: OnceLock<Runtime> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    let rt = RT.get_or_init(|| Runtime::new().unwrap());
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(timestamp_secs): arbitrary::Result<i64> = u.arbitrary() else {
        return;
    };
    let Ok(has_timestamp): arbitrary::Result<bool> = u.arbitrary() else {
        return;
    };
    let Ok(min_age_secs): arbitrary::Result<u64> = u.arbitrary() else {
        return;
    };
    let Ok(deny_missing): arbitrary::Result<bool> = u.arbitrary() else {
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

    // `None` is the upstream that publishes no date, which is its own branch of
    // the rule — `deny_missing_timestamp` decides it, and the bypass list wins
    // over that too.
    let published_at = if has_timestamp {
        let Some(ts) = chrono::DateTime::from_timestamp(timestamp_secs, 0) else {
            return;
        };
        Some(ts)
    } else {
        None
    };

    // Cap at one year to keep durations meaningful.
    let min_age = Duration::from_secs(min_age_secs.min(365 * 24 * 3600));

    let rule = ReleaseAgeGateRule::new(min_age, vec![Role::Admin])
        .with_deny_missing_timestamp(deny_missing);

    let bypassed = role == Role::Admin;
    let identity = Identity {
        user_id: None,
        role: role.clone(),
        auth_provider: None,
        groups: vec![],
    };
    let meta = PackageMetadata {
        id: PackageId::new("github", "owner/repo", "v1.0.0"),
        published_at,
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
        requested_version: None,
    };

    // The rule reads the clock itself, so the oracle brackets it: an age
    // measured before the call is a lower bound on what the rule saw, one
    // measured after is an upper bound. A release that is old enough on the
    // early reading is old enough for the rule; one still too young on the late
    // reading was too young for the rule. Only an input that crosses the
    // threshold *during* the call is undecidable, and that one is skipped.
    // A publish date in the future reads as age zero, which is what
    // `to_std().unwrap_or_default()` in the rule does with a negative delta.
    let age_at = |t: chrono::DateTime<Utc>| -> Option<Duration> {
        published_at.map(|p| (t - p).to_std().unwrap_or_default())
    };
    let before = age_at(Utc::now());
    let decision = rt.block_on(rule.evaluate(&ctx));
    let after = age_at(Utc::now());

    let is_allow = matches!(decision, RuleDecision::Allow);
    match (before, after) {
        (None, _) | (_, None) => {
            let expect_allow = !deny_missing || bypassed;
            assert_eq!(
                is_allow, expect_allow,
                "no timestamp, deny_missing_timestamp={deny_missing}, {role:?}"
            );
        }
        (Some(lo), Some(hi)) => {
            if bypassed {
                assert!(is_allow, "Admin is in the bypass list and was denied");
            } else if lo >= min_age {
                assert!(
                    is_allow,
                    "age {lo:?} >= min {min_age:?} was denied for {role:?}"
                );
            } else if hi < min_age {
                assert!(
                    !is_allow,
                    "age {hi:?} < min {min_age:?} was allowed for {role:?}"
                );
            }
        }
    }
});
