//! RFC 0002 (recast): the flag store and the exposure report on a real
//! database — the upsert's `xmax` trick, the tombstone, the join over
//! `access_events` and the keyset cursor are all Postgres semantics the
//! in-memory double proves nothing about.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_advisory

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use batlehub_adapters::db::PgAdvisoryRepository;
use batlehub_core::{
    entities::{
        ExposureCursor, ExposureQuery, ExposureWhen, FlagEffect, FlagFilter, FlagKind, PackageFlag,
        RegistryScanState,
    },
    ports::AdvisoryRepository,
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

async fn fixture(url: &str) -> (PgPool, String) {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let registry = format!("flags-t{}-{id}", std::process::id());
    let pool = PgPool::connect(url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    // `(source, external_id)` is the flag's identity, so the ids below carry
    // the registry name: a second run against the same database must not
    // find the first run's row and read an insert as an update.
    for table in ["package_flags", "access_events", "registry_scan_state"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE registry = $1"
        )))
        .bind(&registry)
        .execute(&pool)
        .await
        .expect("clear");
    }
    (pool, registry)
}

fn flag(registry: &str, external_id: &str, version: &str, effect: FlagEffect) -> PackageFlag {
    let now = Utc::now();
    PackageFlag {
        id: Uuid::new_v4(),
        source: "soc".into(),
        external_id: format!("{registry}/{external_id}"),
        registry: registry.into(),
        package_name: "left-pad".into(),
        version: version.into(),
        kind: FlagKind::Malware,
        effect,
        severity: None,
        summary: "steals tokens".into(),
        url: None,
        first_seen: now,
        updated_at: now,
        expires_at: None,
        revoked_at: None,
    }
}

/// One allowed download in the access log, as the proxy records it.
async fn pull(pool: &PgPool, registry: &str, user: &str, version: &str, at: chrono::DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO access_events
            (id, user_id, user_role, registry, package_name, package_version, action, outcome, created_at)
         VALUES ($1, $2, 'user', $3, 'left-pad', $4, 'download', 'allowed', $5)",
    )
    .bind(Uuid::new_v4())
    .bind(user)
    .bind(registry)
    .bind(version)
    .bind(at)
    .execute(pool)
    .await
    .expect("insert access event");
}

#[tokio::test]
async fn upsert_updates_in_place_and_a_revoke_keeps_the_tombstone() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let repo = PgAdvisoryRepository::new(pool.clone());

    let first = flag(&reg, "CASE-1", "1.3.1", FlagEffect::Gate);
    let (id, created) = repo.upsert_flag(first.clone()).await.unwrap();
    assert!(created);
    assert_eq!(id, first.id);

    // A re-push: same row, new effect, first_seen kept.
    let mut again = flag(&reg, "CASE-1", "1.3.1", FlagEffect::HardBlock);
    again.first_seen = Utc::now() + Duration::hours(1);
    let (id2, created) = repo.upsert_flag(again).await.unwrap();
    assert!(!created);
    assert_eq!(id2, id);
    let stored = repo
        .get_flag("soc", &format!("{reg}/CASE-1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.effect, FlagEffect::HardBlock);
    assert_eq!(stored.first_seen.timestamp(), first.first_seen.timestamp());

    let live = repo
        .live_flags_for_package(&reg, "left-pad", Utc::now())
        .await
        .unwrap();
    assert_eq!(live.len(), 1);

    assert!(repo
        .revoke_flag("soc", &format!("{reg}/CASE-1"), Utc::now())
        .await
        .unwrap());
    assert!(!repo
        .revoke_flag("soc", &format!("{reg}/CASE-1"), Utc::now())
        .await
        .unwrap());
    assert!(repo
        .live_flags_for_package(&reg, "left-pad", Utc::now())
        .await
        .unwrap()
        .is_empty());
    let dead = FlagFilter {
        registry: Some(reg.clone()),
        include_dead: true,
        ..Default::default()
    };
    assert_eq!(repo.count_flags(&dead).await.unwrap(), 1);
    let alive = FlagFilter {
        registry: Some(reg.clone()),
        ..Default::default()
    };
    assert_eq!(repo.count_flags(&alive).await.unwrap(), 0);

    // A re-push revives the tombstone.
    let (_, created) = repo
        .upsert_flag(flag(&reg, "CASE-1", "1.3.1", FlagEffect::Warn))
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(repo.count_flags(&alive).await.unwrap(), 1);

    let coverage = repo.source_coverage(Utc::now()).await.unwrap();
    assert!(coverage
        .iter()
        .any(|c| c.source == "soc" && c.live_flags >= 1));
}

#[tokio::test]
async fn exposure_joins_pulls_to_flags_and_pages_by_keyset() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let repo = PgAdvisoryRepository::new(pool.clone());

    let t0 = Utc::now() - Duration::hours(3);
    // alice pulled 1.3.1 twice before the flag and once after; bob pulled
    // 1.3.0 once, which the `*` flag covers and the exact one does not.
    for (user, version, at) in [
        ("alice", "1.3.1", t0),
        ("alice", "1.3.1", t0 + Duration::minutes(10)),
        ("bob", "1.3.0", t0 + Duration::minutes(20)),
    ] {
        pull(&pool, &reg, user, version, at).await;
    }
    let mut exact = flag(&reg, "EXACT", "1.3.1", FlagEffect::HardBlock);
    exact.first_seen = t0 + Duration::hours(1);
    let mut any = flag(&reg, "ANY", "*", FlagEffect::Inform);
    any.first_seen = t0 + Duration::hours(1);
    repo.upsert_flag(exact.clone()).await.unwrap();
    repo.upsert_flag(any.clone()).await.unwrap();
    pull(&pool, &reg, "alice", "1.3.1", t0 + Duration::hours(2)).await;

    let all = repo
        .list_exposure(&ExposureQuery {
            registry: Some(reg.clone()),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    // alice×1.3.1×EXACT, alice×1.3.1×ANY, bob×1.3.0×ANY.
    assert_eq!(all.rows.len(), 3, "{:?}", all.rows);
    assert!(all.next.is_none());
    let alice_exact = all
        .rows
        .iter()
        .find(|r| r.consumer == "alice" && r.external_id.ends_with("/EXACT"))
        .unwrap();
    assert_eq!(alice_exact.pulls, 3);
    assert_eq!(alice_exact.pulls_before_flag, 2);
    assert_eq!(alice_exact.effect, FlagEffect::HardBlock);
    // Newest pull first: alice's rows (last pull t0+2h) precede bob's.
    assert_eq!(all.rows[2].consumer, "bob");

    // Filters.
    let hard = repo
        .list_exposure(&ExposureQuery {
            registry: Some(reg.clone()),
            min_effect: Some(FlagEffect::Gate),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(hard.rows.len(), 1);
    let before = repo
        .list_exposure(&ExposureQuery {
            registry: Some(reg.clone()),
            when: ExposureWhen::BeforeFlag,
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(before.rows.len(), 3, "every row has a pull before the flag");
    let after = repo
        .list_exposure(&ExposureQuery {
            registry: Some(reg.clone()),
            when: ExposureWhen::AfterFlag,
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        after.rows.len(),
        2,
        "only alice pulled after it: {:?}",
        after.rows
    );

    // Keyset: two pages of two and one, no overlap, no gap.
    let p1 = repo
        .list_exposure(&ExposureQuery {
            registry: Some(reg.clone()),
            limit: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(p1.rows.len(), 2);
    let cursor = ExposureCursor::decode(p1.next.as_deref().expect("more follows")).unwrap();
    let p2 = repo
        .list_exposure(&ExposureQuery {
            registry: Some(reg.clone()),
            limit: 2,
            after: Some(cursor),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(p2.rows.len(), 1);
    assert!(p2.next.is_none());
    let mut keys: Vec<(String, String)> = p1
        .rows
        .iter()
        .chain(&p2.rows)
        .map(|r| (r.consumer.clone(), r.external_id.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 3);
}

#[tokio::test]
async fn scan_state_is_one_row_per_registry() {
    let Some(url) = db_url() else {
        eprintln!("skipping: DATABASE_URL is not set");
        return;
    };
    let (pool, reg) = fixture(&url).await;
    let repo = PgAdvisoryRepository::new(pool);
    for n in [1u64, 2] {
        repo.record_scan_state(&RegistryScanState {
            registry: reg.clone(),
            last_scan_at: Utc::now(),
            artifacts_scanned: n,
            findings: 0,
            errors: 0,
        })
        .await
        .unwrap();
    }
    let rows = repo.list_scan_state().await.unwrap();
    let mine: Vec<_> = rows.iter().filter(|r| r.registry == reg).collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].artifacts_scanned, 2);
}
