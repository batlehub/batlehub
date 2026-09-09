//! RFC 0008 §10, the external half: the miss log and the bundle history on a
//! real database.
//!
//! Three things here are Postgres semantics the in-memory double proves
//! nothing about — the upsert's `xmax = 0` test for "was this an insert",
//! which decides whether the per-registry cap sweep runs at all; the cap's
//! own `OFFSET` delete; and the retention purge's `last_seen` cutoff. The
//! fourth is the primary key, which is the whole of "recorded once per
//! `(registry, storage_key)`" — a rule expressed as a constraint rather than
//! as caller discipline, so it holds under the concurrency mise's retries
//! actually produce.
//!
//!   DATABASE_URL=postgresql://… cargo test -p batlehub-adapters --test pg_air_gap

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use sqlx::PgPool;

use batlehub_adapters::db::{PgBundleHistory, PgMissRecorder};
use batlehub_core::{
    entities::{BundleImport, ContentMiss, MissFilter, MissKind},
    ports::{BundleHistory, MissRecorder},
};

fn db_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

static TEST_ID: AtomicU64 = AtomicU64::new(0);

/// A pool and a registry name nothing else in the database uses.
///
/// The registry is part of the primary key, so a second run against the same
/// database would otherwise find the first run's rows and read an insert as a
/// bump — which is exactly the distinction under test.
async fn fixture(url: &str) -> (PgPool, String) {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let registry = format!("airgap-t{}-{id}", std::process::id());
    let pool = PgPool::connect(url).await.expect("connect");
    batlehub_adapters::migrations::embedded_migrator()
        .run(&pool)
        .await
        .expect("migrations");
    sqlx::query("DELETE FROM missing_content WHERE registry = $1")
        .bind(&registry)
        .execute(&pool)
        .await
        .expect("clean");
    (pool, registry)
}

fn miss(registry: &str, key: &str, kind: MissKind) -> ContentMiss {
    ContentMiss {
        registry: registry.to_owned(),
        storage_key: key.to_owned(),
        kind,
        coordinate: Some(format!("{registry}:{key}")),
        requested_version: None,
        held_versions: Vec::new(),
    }
}

/// mise retries. The log must not grow with the retries, and the counter is
/// what tells an operator which gap actually hurts.
#[tokio::test]
async fn a_retried_miss_bumps_one_row_and_moves_its_last_seen() {
    let Some(url) = db_url() else { return };
    let (pool, registry) = fixture(&url).await;
    let rec = PgMissRecorder::new(pool.clone());

    let first = Utc::now() - Duration::hours(2);
    let m = miss(&registry, "lodash/1.1.0/tarball", MissKind::Artifact);
    rec.record(&m, first).await.unwrap();
    let later = Utc::now();
    for _ in 0..4 {
        rec.record(&m, later).await.unwrap();
    }

    let filter = MissFilter {
        registry: Some(registry.clone()),
        ..Default::default()
    };
    let rows = rec.list(&filter).await.unwrap();
    assert_eq!(rows.len(), 1, "one key, one row: {rows:?}");
    assert_eq!(rows[0].count, 5);
    assert_eq!(rows[0].kind, MissKind::Artifact);
    assert_eq!(
        rows[0].first_seen.timestamp(),
        first.timestamp(),
        "the first sighting is not overwritten by the retries"
    );
    assert!(rows[0].last_seen > rows[0].first_seen);
    assert_eq!(rec.count(&filter).await.unwrap(), 1);
}

#[tokio::test]
async fn the_listing_narrows_by_kind_and_orders_by_how_much_a_gap_hurts() {
    let Some(url) = db_url() else { return };
    let (pool, registry) = fixture(&url).await;
    let rec = PgMissRecorder::new(pool.clone());
    let now = Utc::now();

    rec.record(
        &miss(&registry, "quiet/1.0/tarball", MissKind::Artifact),
        now,
    )
    .await
    .unwrap();
    let loud = miss(&registry, "loud/1.0/tarball", MissKind::Artifact);
    for _ in 0..3 {
        rec.record(&loud, now).await.unwrap();
    }
    rec.record(&miss(&registry, "listing", MissKind::Document), now)
        .await
        .unwrap();

    let all = rec
        .list(&MissFilter {
            registry: Some(registry.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(
        all[0].storage_key, "loud/1.0/tarball",
        "most-asked first, because that is the gap to close next: {all:?}"
    );

    let docs = rec
        .list(&MissFilter {
            registry: Some(registry.clone()),
            kind: Some(MissKind::Document),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].kind, MissKind::Document);

    // Paging, which the admin surface uses and which an unstable ORDER BY
    // would make return the same row twice.
    let page = rec
        .list(&MissFilter {
            registry: Some(registry.clone()),
            limit: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
    let page2 = rec
        .list(&MissFilter {
            registry: Some(registry.clone()),
            limit: 2,
            offset: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_ne!(page2[0].storage_key, page[0].storage_key);
}

/// `miss_retention_days` as the sweep the admin endpoint runs: rows last seen
/// before the cutoff go, and a row asked for since stays however old its
/// first sighting is.
#[tokio::test]
async fn the_purge_forgets_by_last_seen_and_can_be_narrowed_to_one_registry() {
    let Some(url) = db_url() else { return };
    let (pool, registry) = fixture(&url).await;
    let (_, other) = fixture(&url).await;
    let rec = PgMissRecorder::new(pool.clone());

    let old = Utc::now() - Duration::days(120);
    let now = Utc::now();
    rec.record(
        &miss(&registry, "stale/1.0/tarball", MissKind::Artifact),
        old,
    )
    .await
    .unwrap();
    // Old first sighting, asked for again today: not stale.
    let revived = miss(&registry, "revived/1.0/tarball", MissKind::Artifact);
    rec.record(&revived, old).await.unwrap();
    rec.record(&revived, now).await.unwrap();
    rec.record(
        &miss(&other, "elsewhere/1.0/tarball", MissKind::Artifact),
        old,
    )
    .await
    .unwrap();

    let cutoff = Utc::now() - Duration::days(90);
    let deleted = rec.purge(cutoff, Some(&registry)).await.unwrap();
    assert_eq!(deleted, 1, "only the one nobody has asked for since");

    let left = rec
        .list(&MissFilter {
            registry: Some(registry.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].storage_key, "revived/1.0/tarball");

    // The other registry was named out of the purge, and kept its row.
    assert_eq!(
        rec.count(&MissFilter {
            registry: Some(other.clone()),
            ..Default::default()
        })
        .await
        .unwrap(),
        1
    );
    assert_eq!(rec.purge(cutoff, Some(&other)).await.unwrap(), 1);
}

/// An import is idempotent by id: carrying the same bundle across twice is
/// one history, not two.
#[tokio::test]
async fn the_bundle_history_is_keyed_by_id_and_reads_newest_first() {
    let Some(url) = db_url() else { return };
    let (pool, registry) = fixture(&url).await;
    let history = PgBundleHistory::new(pool.clone());

    let mk = |id: &str, at: chrono::DateTime<Utc>| BundleImport {
        bundle_id: format!("{registry}-{id}"),
        signer_key: "a".repeat(64),
        imported_at: at,
        imported_by: Some("operator".into()),
        entries: 3,
        blobs: 2,
        rejected: 1,
        rejected_sample: Some("one blob did not hash to its name".into()),
        created_from: Some("mise-plan.json".into()),
    };

    let first = mk("one", Utc::now() - Duration::hours(1));
    let second = mk("two", Utc::now());
    history.record(&first).await.unwrap();
    history.record(&second).await.unwrap();
    // The same id again, as a re-import would.
    history.record(&first).await.unwrap();

    assert!(history.seen(&first.bundle_id).await.unwrap());
    assert!(!history.seen("never-carried").await.unwrap());

    let items: Vec<BundleImport> = history
        .list(50)
        .await
        .unwrap()
        .into_iter()
        .filter(|b| b.bundle_id.starts_with(&registry))
        .collect();
    assert_eq!(
        items.len(),
        2,
        "an id is a bundle, however often it arrives"
    );
    assert_eq!(items[0].bundle_id, second.bundle_id, "newest first");
    assert_eq!(items[0].blobs, 2);
    assert_eq!(items[0].rejected, 1);
    assert_eq!(
        items[0].rejected_sample.as_deref(),
        Some("one blob did not hash to its name"),
        "the line an operator reads before deciding whether to care"
    );
}

/// RFC 0008-bis §4.4: the two columns round-trip, the requested version is
/// kept across a request that named none, and the held set is the latest.
#[tokio::test]
async fn the_requested_version_and_the_held_set_are_stored_and_kept() {
    let Some(url) = db_url() else { return };
    let (pool, registry) = fixture(&url).await;
    let recorder = PgMissRecorder::new(pool.clone());
    let now = Utc::now();
    let mut first = miss(&registry, "left-pad (versions)", MissKind::Document);
    first.requested_version = Some("1.2.0".into());
    first.held_versions = vec!["1.3.0".into()];
    recorder.record(&first, now).await.expect("record");
    let mut second = miss(&registry, "left-pad (versions)", MissKind::Document);
    second.held_versions = vec!["1.3.0".into(), "1.3.1".into()];
    recorder
        .record(&second, now + Duration::minutes(1))
        .await
        .expect("record again");
    let rows = recorder
        .list(&MissFilter {
            registry: Some(registry.clone()),
            ..Default::default()
        })
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].count, 2);
    assert_eq!(rows[0].requested_version.as_deref(), Some("1.2.0"));
    assert_eq!(rows[0].held_versions, vec!["1.3.0", "1.3.1"]);
}
