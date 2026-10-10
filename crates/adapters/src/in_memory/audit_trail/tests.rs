//! `services::audit_trail` against the in-memory store: the seal chain, its
//! verification, and every lifecycle change keeping it verifiable.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};

use batlehub_core::entities::{
    AccessAction, AccessEvent, AccessResult, CallerNet, Identity, PackageId, Role, SealKind,
};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{AuditTrailStore, PackageRepository};
use batlehub_core::services::audit_trail::{AuditPolicy, AuditTrailService};
use batlehub_core::services::signature::VsxSigningKey;

use super::AlwaysLeader;
use crate::in_memory::InMemoryPackageRepository;

const SEED: &str = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";
const STEP: i64 = 300;

fn t0() -> DateTime<Utc> {
    // Aligned to the window, so the arithmetic below reads in whole windows.
    Utc.timestamp_opt(1_790_000_100 - 1_790_000_100 % STEP, 0)
        .unwrap()
}

fn at(secs: i64) -> DateTime<Utc> {
    t0() + chrono::Duration::seconds(secs)
}

fn admin() -> Identity {
    Identity {
        user_id: Some("admin".into()),
        role: Role::Admin,
        auth_provider: None,
        groups: vec![],
    }
}

fn download(user: &str, secs: i64) -> AccessEvent {
    let mut e = AccessEvent::allowed_download(
        PackageId::new("npm", "left-pad", "1.3.0"),
        Some(user.into()),
        Role::User,
    );
    e.ip_address = Some("203.0.113.77".into());
    e.user_agent = Some("npm/10.9.0".into());
    e.timestamp = at(secs);
    e
}

fn sign_in(user: &str, secs: i64) -> AccessEvent {
    let mut e = AccessEvent::about_identity(
        AccessAction::SignIn,
        Some(user.into()),
        Role::User,
        AccessResult::Allowed,
        CallerNet {
            ip: Some("198.51.100.4".into()),
            user_agent: Some("firefox".into()),
        },
        Some("provider=authentik".into()),
    );
    e.timestamp = at(secs);
    e
}

fn policy() -> AuditPolicy {
    AuditPolicy {
        sealing: Some((
            Duration::from_secs(STEP as u64),
            VsxSigningKey::from_seed_hex(SEED, None).unwrap(),
        )),
        erasure_key: Some("an-erasure-key-that-is-long-enough-0123".into()),
        ..AuditPolicy::default()
    }
}

async fn setup(
    policy: AuditPolicy,
    rows: Vec<AccessEvent>,
) -> (Arc<InMemoryPackageRepository>, AuditTrailService) {
    let repo = InMemoryPackageRepository::new();
    for r in rows {
        repo.record_access(r).await.unwrap();
    }
    let svc = AuditTrailService::new(repo.clone(), repo.clone(), Arc::new(AlwaysLeader), policy);
    (repo, svc)
}

/// Rows in the first three windows, sealed by a tick four windows later.
async fn sealed_trail() -> (Arc<InMemoryPackageRepository>, AuditTrailService) {
    let rows = vec![
        download("alice", 10),
        sign_in("alice", 20),
        download("bob", STEP + 5),
        download("alice", 2 * STEP + 5),
    ];
    let (repo, svc) = setup(policy(), rows).await;
    // Chain empty: the first tick starts two windows back from `now`.
    let first = svc.seal_tick(at(2 * STEP + 2 * STEP)).await.unwrap();
    assert_eq!(
        first.len(),
        1,
        "an empty chain seals the newest closed window only"
    );
    (repo, svc)
}

#[tokio::test]
async fn a_window_is_sealed_only_once_it_is_a_window_in_the_past() {
    let (_repo, svc) = setup(policy(), vec![download("alice", 10)]).await;
    let sealed = svc.seal_tick(at(STEP + 1)).await.unwrap();
    assert!(
        sealed.iter().all(|r| r.window_end <= at(1)),
        "nothing still open is sealed"
    );
}

#[tokio::test]
async fn a_sealed_trail_verifies_and_an_edited_row_names_its_window() {
    let (repo, svc) = sealed_trail().await;
    // Two more windows close: they chain onto the first.
    svc.seal_tick(at(6 * STEP)).await.unwrap();
    let report = svc.verify(None, None, None).await.unwrap();
    assert!(report.ok, "{:?}", report.failures);
    assert!(report.windows_checked >= 2);
    assert!(!report.truncation_checked);

    // What an attacker with SQL access would do.
    let victim = repo
        .seal_records()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.row_count > 0)
        .unwrap();
    {
        let mut events = repo.events.write().await;
        let row = events
            .iter_mut()
            .find(|e| e.timestamp >= victim.window_start && e.timestamp < victim.window_end)
            .unwrap();
        row.user_id = Some("someone-else".into());
    }
    let report = svc.verify(None, None, None).await.unwrap();
    assert!(!report.ok);
    assert_eq!(report.failures[0].window_start, Some(victim.window_start));
}

#[tokio::test]
async fn a_truncated_tail_verifies_alone_and_fails_against_the_siem_head() {
    let (repo, svc) = sealed_trail().await;
    svc.seal_tick(at(6 * STEP)).await.unwrap();
    let head = svc.verify(None, None, None).await.unwrap().head.unwrap();
    repo.seals.write().await.pop();
    assert!(
        svc.verify(None, None, None).await.unwrap().ok,
        "a shorter chain is valid on its own"
    );
    let report = svc.verify(None, None, Some(&head)).await.unwrap();
    assert!(!report.ok && report.truncation_checked);
}

#[tokio::test]
async fn pseudonymisation_takes_access_rows_only_and_keeps_the_chain_valid() {
    let (repo, _) = sealed_trail().await;
    let svc = AuditTrailService::new(
        repo.clone(),
        repo.clone(),
        Arc::new(AlwaysLeader),
        AuditPolicy {
            pseudonymise_after: Some(chrono::Duration::seconds(1)),
            ..policy()
        },
    );
    svc.seal_tick(at(6 * STEP)).await.unwrap();
    let report = svc.lifecycle_tick(at(6 * STEP)).await.unwrap();
    assert!(report.leader && report.pseudonymised > 0);

    let events = repo.events.read().await.clone();
    let sealed_until = repo
        .seal_records()
        .await
        .unwrap()
        .iter()
        .map(|r| r.window_end)
        .max()
        .unwrap();
    for e in events.iter().filter(|e| e.timestamp < sealed_until) {
        if e.action == AccessAction::Download {
            assert_eq!(e.ip_address.as_deref(), Some("203.0.113.0/24"));
            assert_eq!(e.user_agent, None);
            assert!(e.user_id.is_some(), "the user stays; only precision goes");
        }
        if e.action == AccessAction::SignIn {
            assert_eq!(
                e.ip_address.as_deref(),
                Some("198.51.100.4"),
                "security rows keep their IP"
            );
        }
    }
    let kinds: Vec<_> = repo
        .seal_records()
        .await
        .unwrap()
        .iter()
        .map(|r| r.kind)
        .collect();
    assert!(kinds.contains(&SealKind::Amend));
    assert!(svc.verify(None, None, None).await.unwrap().ok);
    assert!(
        events
            .iter()
            .any(|e| e.action == AccessAction::AuditLifecycleRun
                && e.user_id.as_deref() == Some(Identity::SYSTEM_USER_ID)),
        "a run that changed rows is itself an event, written as `system`"
    );
}

#[tokio::test]
async fn retention_expires_by_class_and_records_what_it_removed() {
    let (repo, _) = sealed_trail().await;
    let svc = AuditTrailService::new(
        repo.clone(),
        repo.clone(),
        Arc::new(AlwaysLeader),
        AuditPolicy {
            access_retention: Some(chrono::Duration::seconds(1)),
            ..policy()
        },
    );
    svc.seal_tick(at(6 * STEP)).await.unwrap();
    let report = svc.lifecycle_tick(at(6 * STEP)).await.unwrap();
    assert!(report.expired > 0);
    let events = repo.events.read().await.clone();
    assert!(events.iter().all(|e| e.action != AccessAction::Download));
    assert!(
        events.iter().any(|e| e.action == AccessAction::SignIn),
        "security class is kept"
    );
    assert!(repo
        .seal_records()
        .await
        .unwrap()
        .iter()
        .any(|r| r.kind == SealKind::Expire));
    assert!(svc.verify(None, None, None).await.unwrap().ok);
}

#[tokio::test]
async fn a_purge_removes_traffic_and_never_a_purge_record() {
    let (repo, svc) = sealed_trail().await;
    svc.seal_tick(at(6 * STEP)).await.unwrap();
    for _ in 0..2 {
        svc.purge_access_before(at(10 * STEP), &admin(), CallerNet::unknown())
            .await
            .unwrap();
    }
    let events = repo.events.read().await.clone();
    assert!(events.iter().all(|e| !e.action.is_access_class()));
    let purges: Vec<_> = events
        .iter()
        .filter(|e| e.action == AccessAction::AuditPurge)
        .collect();
    assert_eq!(
        purges.len(),
        2,
        "the second purge did not remove the first one's row"
    );
    assert!(purges[0].detail.as_deref().unwrap().contains("deleted="));
    assert!(svc.verify(None, None, None).await.unwrap().ok);
}

#[tokio::test]
async fn erasure_renames_the_subject_everywhere_and_keeps_the_chain_valid() {
    let (repo, svc) = sealed_trail().await;
    svc.seal_tick(at(6 * STEP)).await.unwrap();
    // An open-window row too: erasure is not limited to sealed windows.
    repo.record_access(download("alice", 7 * STEP))
        .await
        .unwrap();
    svc.export("alice", &admin(), CallerNet::unknown())
        .await
        .unwrap();

    let report = svc
        .erase("alice", false, &admin(), CallerNet::unknown())
        .await
        .unwrap();
    assert!(report.pseudonym.starts_with("erased:"));
    assert_eq!(
        report.pseudonym,
        svc.pseudonym("alice").unwrap(),
        "stable: rows stay linkable"
    );

    let events = repo.events.read().await.clone();
    let serialised = serde_json::to_string(&events).unwrap();
    assert!(
        !serialised.contains("alice"),
        "the name is gone, from actors and export subjects"
    );
    assert!(events
        .iter()
        .any(|e| e.user_id.as_deref() == Some(report.pseudonym.as_str())));
    let erase_row = events
        .iter()
        .find(|e| e.action == AccessAction::GdprErase)
        .unwrap();
    assert_eq!(erase_row.user_id.as_deref(), Some("admin"));
    assert!(svc.verify(None, None, None).await.unwrap().ok);
}

fn grant(subject: &str, action: AccessAction, detail_tail: &str, secs: i64) -> AccessEvent {
    let mut e = AccessEvent::about_identity(
        action,
        Some("admin".into()),
        Role::Admin,
        AccessResult::Allowed,
        CallerNet::unknown(),
        Some(format!("subject=user:{subject}{detail_tail}")),
    );
    e.timestamp = at(secs);
    e
}

/// RFC 0036 §13 "Still owed": a grant an admin wrote *about* the subject sits
/// under the admin's `user_id` and names the subject only in `detail`.
#[tokio::test]
async fn erasure_renames_a_subject_named_by_an_admins_grant_rows_and_nobody_else() {
    let (repo, svc) = sealed_trail().await;
    for e in [
        grant(
            "alice",
            AccessAction::GrantWrite,
            " actions=read,write",
            2 * STEP + 1,
        ),
        grant("alice", AccessAction::GrantRevoke, "", 3 * STEP + 1),
        grant(
            "alice2",
            AccessAction::GrantWrite,
            " actions=read",
            3 * STEP + 2,
        ),
    ] {
        repo.record_access(e).await.unwrap();
    }
    svc.seal_tick(at(6 * STEP)).await.unwrap();

    let report = svc
        .erase("alice", false, &admin(), CallerNet::unknown())
        .await
        .unwrap();
    let p = &report.pseudonym;
    let details: Vec<String> = repo
        .events
        .read()
        .await
        .iter()
        .filter(|e| {
            matches!(
                e.action,
                AccessAction::GrantWrite | AccessAction::GrantRevoke
            )
        })
        .filter_map(|e| e.detail.clone())
        .collect();
    assert!(
        details.contains(&format!("subject=user:{p} actions=read,write")),
        "{details:?}"
    );
    assert!(
        details.contains(&format!("subject=user:{p}")),
        "{details:?}"
    );
    assert!(
        details.contains(&"subject=user:alice2 actions=read".to_owned()),
        "a longer id that starts with the subject's is someone else: {details:?}"
    );
    assert!(
        svc.verify(None, None, None).await.unwrap().ok,
        "renaming sealed rows amends the chain"
    );
}

#[tokio::test]
async fn erasure_without_a_key_is_refused_before_it_touches_anything() {
    let (repo, svc) = setup(
        AuditPolicy {
            erasure_key: None,
            ..policy()
        },
        vec![download("alice", 10)],
    )
    .await;
    let err = svc
        .erase("alice", false, &admin(), CallerNet::unknown())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotSupported(_)));
    assert_eq!(
        repo.events.read().await[0].user_id.as_deref(),
        Some("alice")
    );
}

#[tokio::test]
async fn without_sealing_the_lifecycle_still_runs_and_writes_no_record() {
    let (repo, svc) = setup(
        AuditPolicy {
            sealing: None,
            pseudonymise_after: Some(chrono::Duration::seconds(1)),
            ..policy()
        },
        vec![download("alice", 10)],
    )
    .await;
    let report = svc.lifecycle_tick(at(STEP)).await.unwrap();
    assert_eq!(report.pseudonymised, 1);
    assert!(repo.seal_records().await.unwrap().is_empty());
    assert!(matches!(
        svc.verify(None, None, None).await,
        Err(CoreError::NotSupported(_))
    ));
}
