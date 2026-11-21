use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use batlehub_adapters::rate_limit::InMemoryRateLimitStore;
use batlehub_config::schema::{GroupRateLimitConfig, RateLimitConfig, RateLimitEnforcement};

use super::store::{merge_failure, RateLimitService};

fn svc_from(registry: &str, cfg: RateLimitConfig) -> RateLimitService {
    let mut m = HashMap::new();
    m.insert(registry.to_owned(), cfg);
    let store = Arc::new(InMemoryRateLimitStore::new());
    RateLimitService::new(&m, store)
}

#[tokio::test]
async fn user_allowed_within_limit() {
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 10,
            window_secs: 60,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![],
        },
    );
    for _ in 0..10 {
        assert!(svc
            .check("r", "u1", &[])
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false));
    }
}

#[tokio::test]
async fn user_blocked_after_limit() {
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 2,
            window_secs: 60,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![],
        },
    );
    assert!(svc
        .check("r", "u1", &[])
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    assert!(svc
        .check("r", "u1", &[])
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    assert!(svc
        .check("r", "u1", &[])
        .await
        .map(|r| r.is_err())
        .unwrap_or(false));
}

#[tokio::test]
async fn user_buckets_are_independent() {
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 1,
            window_secs: 60,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![],
        },
    );
    assert!(svc
        .check("r", "u1", &[])
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    // Different user still allowed.
    assert!(svc
        .check("r", "u2", &[])
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    // u1 is blocked.
    assert!(svc
        .check("r", "u1", &[])
        .await
        .map(|r| r.is_err())
        .unwrap_or(false));
}

#[tokio::test]
async fn no_config_returns_none() {
    let store = Arc::new(InMemoryRateLimitStore::new());
    let svc = RateLimitService::new(&HashMap::new(), store);
    assert!(svc.check("r", "u1", &[]).await.is_none());
}

#[tokio::test]
async fn group_bucket_shared_by_members() {
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 100,
            window_secs: 60,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![GroupRateLimitConfig {
                name: "ci-bots".to_owned(),
                requests_per_window: 2,
                window_secs: 60,
                enforcement: None,
            }],
        },
    );

    let groups = vec!["ci-bots".to_owned()];
    // Both bot1 and bot2 draw from the shared "ci-bots" pool (limit = 2).
    assert!(svc
        .check("r", "bot1", &groups)
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    assert!(svc
        .check("r", "bot2", &groups)
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    // Pool exhausted — third request (from any group member) is blocked.
    assert!(svc
        .check("r", "bot3", &groups)
        .await
        .map(|r| r.is_err())
        .unwrap_or(false));
}

#[tokio::test]
async fn non_group_member_not_affected_by_group_limit() {
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 100,
            window_secs: 60,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![GroupRateLimitConfig {
                name: "ci-bots".to_owned(),
                requests_per_window: 1,
                window_secs: 60,
                enforcement: None,
            }],
        },
    );

    // Exhaust the ci-bots pool.
    let bot_groups = vec!["ci-bots".to_owned()];
    assert!(svc
        .check("r", "bot1", &bot_groups)
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    assert!(svc
        .check("r", "bot1", &bot_groups)
        .await
        .map(|r| r.is_err())
        .unwrap_or(false));

    // A user not in ci-bots is unaffected by the group limit.
    assert!(svc
        .check("r", "regular-user", &[])
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
}

#[tokio::test]
async fn user_and_group_both_checked() {
    // User limit = 3; group limit = 1.
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 3,
            window_secs: 60,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![GroupRateLimitConfig {
                name: "g".to_owned(),
                requests_per_window: 1,
                window_secs: 60,
                enforcement: None,
            }],
        },
    );
    let groups = vec!["g".to_owned()];
    // First request OK — both user bucket and group bucket have tokens.
    assert!(svc
        .check("r", "u1", &groups)
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false));
    // Second request: group bucket exhausted (limit=1), blocks even though user bucket still has tokens.
    assert!(svc
        .check("r", "u1", &groups)
        .await
        .map(|r| r.is_err())
        .unwrap_or(false));
}

#[tokio::test]
async fn group_enforcement_overrides_parent() {
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 10,
            window_secs: 60,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![GroupRateLimitConfig {
                name: "vip".to_owned(),
                requests_per_window: 2,
                window_secs: 60,
                enforcement: Some(RateLimitEnforcement::Warn),
            }],
        },
    );
    let groups = vec!["vip".to_owned()];
    svc.check("r", "u1", &groups).await.unwrap().ok();
    svc.check("r", "u1", &groups).await.unwrap().ok();
    // Group bucket exhausted — but enforcement is Warn, so the error carries Warn.
    let result = svc.check("r", "u1", &groups).await.unwrap();
    assert!(result.is_err());
    let (_, _, enforcement, _) = result.unwrap_err();
    assert_eq!(enforcement, RateLimitEnforcement::Warn);
}

#[test]
fn merge_failure_block_beats_warn() {
    let warn = (
        Duration::from_secs(10),
        100u32,
        RateLimitEnforcement::Warn,
        0u64,
    );
    let block = (
        Duration::from_secs(5),
        50u32,
        RateLimitEnforcement::Block,
        0u64,
    );

    let (_, _, e, _) = merge_failure(Some(warn.clone()), block.clone());
    assert_eq!(e, RateLimitEnforcement::Block);

    let (_, _, e, _) = merge_failure(Some(block.clone()), warn.clone());
    assert_eq!(e, RateLimitEnforcement::Block);
}

#[test]
fn merge_failure_longer_wait_wins() {
    let short = (
        Duration::from_secs(5),
        100u32,
        RateLimitEnforcement::Block,
        0u64,
    );
    let long = (
        Duration::from_secs(30),
        50u32,
        RateLimitEnforcement::Block,
        0u64,
    );

    let (wait, _, _, _) = merge_failure(Some(short), long);
    assert_eq!(wait, Duration::from_secs(30));
}

// ── usage(): the caller's counters, read without spending a request ──────────

#[tokio::test]
async fn usage_reads_the_buckets_check_counts_without_spending_one() {
    use super::store::RateLimitScope;
    let svc = svc_from(
        "r",
        RateLimitConfig {
            requests_per_window: 10,
            window_secs: 3600,
            enforcement: RateLimitEnforcement::Block,
            groups: vec![GroupRateLimitConfig {
                name: "ci".into(),
                requests_per_window: 100,
                window_secs: 3600,
                enforcement: Some(RateLimitEnforcement::Warn),
            }],
        },
    );
    let groups = vec!["ci".to_owned(), "no-ceiling".to_owned()];
    for _ in 0..3 {
        svc.check("r", "u1", &groups).await.unwrap().unwrap();
    }

    let usage = svc.usage("r", "u1", &groups).await.unwrap().unwrap();
    assert_eq!(
        usage.len(),
        2,
        "the user bucket and the one group with a ceiling"
    );
    assert_eq!(usage[0].scope, RateLimitScope::User);
    assert_eq!((usage[0].used, usage[0].limit), (3, 10));
    assert!(matches!(usage[0].enforcement, RateLimitEnforcement::Block));
    assert_eq!(usage[1].scope, RateLimitScope::Group("ci".into()));
    assert_eq!((usage[1].used, usage[1].limit), (3, 100));
    assert!(matches!(usage[1].enforcement, RateLimitEnforcement::Warn));

    let again = svc.usage("r", "u1", &groups).await.unwrap().unwrap();
    assert_eq!(again[0].used, 3, "reading the usage spends nothing");
}

#[tokio::test]
async fn usage_is_none_for_a_registry_without_a_limit() {
    let svc = RateLimitService::new(&HashMap::new(), Arc::new(InMemoryRateLimitStore::new()));
    assert!(svc.usage("r", "u1", &[]).await.is_none());
}
