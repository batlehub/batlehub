//! RFC 0008 phase 1 on the wire: an instance that will not dial out says
//! what it lacks.
//!
//! The three things this file pins, all stated in §4.4:
//!
//! * **a miss is a `503`, not a `404`.** `404` asserts the artifact does not
//!   exist, which is false — it exists, it is simply not here — and on a
//!   hybrid registry `404` is the signal that means *ask upstream*.
//! * **a miss is recorded once per `(registry, key)`**, with a counter. mise
//!   retries; the record must not grow with the retries.
//! * **a coordinate a rule refused is not a gap in the mirror.** A blocked
//!   package must never be proposed for the next bundle.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;
use std::time::Duration;

use actix_web::test::{call_service, TestRequest};
use batlehub_adapters::in_memory::InMemoryMissRecorder;
use batlehub_adapters::registry::offline::OfflineRegistryClient;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{AirGapPolicy, MissFilter, MissKind, PackageId, PackageStatus},
    ports::{MissRecorder, RegistryClient, VerdictRepository},
    rules::BlockListRule,
    services::RegistryPolicy,
};

const REG: &str = "npm-mirror";

struct Lab {
    misses: Arc<InMemoryMissRecorder>,
    repo: Arc<dyn batlehub_core::ports::PackageRepository>,
}

/// The app: one npm registry whose client refuses every call, the miss
/// recorder behind it, and the block list in the chain so a refusal can be
/// told apart from a gap.
async fn lab(record: bool) -> (impl TestService, Lab) {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let misses = InMemoryMissRecorder::new();
    let repo = Arc::clone(&parts.proxy_svc.repo);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        let real = Arc::clone(hot.registries.get(REG).expect("the fixture built one"));
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(OfflineRegistryClient::new(real, REG)) as Arc<dyn RegistryClient>,
        );
        hot.air_gap = AirGapPolicy {
            enabled: true,
            record_misses: record,
            miss_retention_days: 90,
            bundle_trusted_keys: vec!["a".repeat(64)],
        };
        hot.miss_recorder = Some(Arc::clone(&misses) as Arc<dyn MissRecorder>);
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: Some(Duration::ZERO),
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![Box::new(BlockListRule::new(Arc::clone(&repo)))],
            }),
        );
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, Lab { misses, repo })
}

async fn get<S: TestService>(app: &S, uri: &str) -> actix_web::dev::ServiceResponse {
    call_service(app, admin_get(uri)).await
}

async fn body_of(resp: actix_web::dev::ServiceResponse) -> serde_json::Value {
    actix_web::test::read_body_json(resp).await
}

fn tarball(version: &str) -> String {
    format!("/proxy/{REG}/lodash/{version}/tarball")
}

// ── the refusal ──────────────────────────────────────────────────────────────

#[actix_web::test]
async fn a_miss_is_a_503_that_names_the_coordinate_and_what_to_do() {
    let (app, _) = lab(true).await;
    let resp = get(&app, &tarball("1.1.0")).await;
    assert_eq!(
        resp.status(),
        503,
        "404 would assert the artifact does not exist, and is what a hybrid fall-through acts on"
    );
    let body = body_of(resp).await;
    assert_eq!(body["code"], "content_unavailable");
    assert_eq!(body["registry"], REG);
    assert!(
        body["coordinate"].as_str().unwrap().contains("lodash"),
        "{body}"
    );
    assert!(body["bundle_hint"].as_str().unwrap().contains("air-gap"));
}

#[actix_web::test]
async fn a_listing_is_refused_the_same_way() {
    let (app, lab) = lab(true).await;
    let resp = get(&app, &format!("/proxy/{REG}/lodash")).await;
    assert_eq!(resp.status(), 503);
    let recorded = lab.misses.list(&MissFilter::default()).await.unwrap();
    assert!(
        recorded.iter().any(|m| m.kind == MissKind::Document),
        "a listing the bundle lacks is recorded as a document: {recorded:?}"
    );
}

// ── the record ───────────────────────────────────────────────────────────────

#[actix_web::test]
async fn retries_bump_one_row_rather_than_writing_many() {
    let (app, lab) = lab(true).await;
    for _ in 0..3 {
        assert_eq!(get(&app, &tarball("1.1.0")).await.status(), 503);
    }
    let rows = lab
        .misses
        .list(&MissFilter {
            kind: Some(MissKind::Artifact),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "mise retries; the log must not grow with it");
    assert_eq!(rows[0].count, 3);
    assert_eq!(rows[0].registry, REG);
    assert!(rows[0].last_seen >= rows[0].first_seen);
}

#[actix_web::test]
async fn recording_can_be_turned_off_without_changing_the_refusal() {
    let (app, lab) = lab(false).await;
    assert_eq!(get(&app, &tarball("1.1.0")).await.status(), 503);
    assert!(lab
        .misses
        .list(&MissFilter::default())
        .await
        .unwrap()
        .is_empty());
}

/// RFC 0008 §5.3 and decision 7: the record is written after the rule chain,
/// so a coordinate an administrator blocked is never proposed for the next
/// bundle. A blocked package is not a gap in the mirror.
#[actix_web::test]
async fn a_blocked_coordinate_is_refused_and_not_recorded_as_missing() {
    let (app, lab) = lab(true).await;
    lab.repo
        .set_status(
            &PackageId::new(REG, "lodash", "1.1.0"),
            PackageStatus::Blocked {
                reason: "known bad".into(),
                blocked_by: "admin".into(),
                blocked_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
    let resp = get(&app, &tarball("1.1.0")).await;
    assert_eq!(resp.status(), 403, "a block is a refusal, not a gap");
    let rows = lab
        .misses
        .list(&MissFilter {
            kind: Some(MissKind::Artifact),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "a blocked coordinate must not reach the next bundle's list: {rows:?}"
    );
}

// ── the admin surface ────────────────────────────────────────────────────────

#[actix_web::test]
async fn the_record_is_readable_and_purgeable_by_an_administrator() {
    let (app, _) = lab(true).await;
    get(&app, &tarball("1.1.0")).await;
    get(&app, &tarball("1.0.0")).await;

    let v = get_json(&app, "/api/v1/admin/air-gap/missing").await;
    assert_eq!(v["total"], 2, "{v}");
    assert_eq!(v["air_gapped"], true);
    assert_eq!(v["items"][0]["registry"], REG);

    let v = get_json(&app, "/api/v1/admin/air-gap/missing?kind=document").await;
    assert_eq!(v["total"], 0, "the filter narrows by kind: {v}");
    let resp = get(&app, "/api/v1/admin/air-gap/missing?kind=nope").await;
    assert_eq!(resp.status(), 400);

    // A purge with an explicit cutoff in the future forgets everything.
    let resp = call_service(
        &app,
        TestRequest::delete()
            .uri("/api/v1/admin/air-gap/missing?before=2099-01-01T00:00:00Z")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body = body_of(resp).await;
    assert_eq!(body["deleted"], 2);
    let v = get_json(&app, "/api/v1/admin/air-gap/missing").await;
    assert_eq!(v["total"], 0);
}

#[actix_web::test]
async fn the_record_needs_a_system_verb() {
    let (app, _) = lab(true).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/air-gap/missing")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

/// RFC 0008 §4.5's warning, on the wire: a hybrid registry keeps working —
/// publishing to a disconnected instance is legitimate — but its fall-through
/// to upstream can never happen. What it must not do is turn the refusal into
/// a `404`, which is the signal that means *ask upstream* and which RFC 0006
/// spent a document separating from a refusal.
#[actix_web::test]
async fn a_hybrid_registry_does_not_fall_through_to_an_upstream_it_cannot_reach() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Hybrid, None);
    let misses = InMemoryMissRecorder::new();
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        let real = Arc::clone(hot.registries.get(REG).expect("the fixture built one"));
        hot.registries.insert(
            REG.to_owned(),
            Arc::new(OfflineRegistryClient::new(real, REG)) as Arc<dyn RegistryClient>,
        );
        hot.air_gap = AirGapPolicy {
            enabled: true,
            record_misses: true,
            miss_retention_days: 90,
            bundle_trusted_keys: vec!["a".repeat(64)],
        };
        hot.miss_recorder = Some(Arc::clone(&misses) as Arc<dyn MissRecorder>);
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let resp = get(&app, &tarball("1.1.0")).await;
    assert_eq!(
        resp.status(),
        503,
        "the fall-through has nowhere to go, and a 404 would say the version does not exist"
    );
    assert!(!misses
        .list(&MissFilter::default())
        .await
        .unwrap()
        .is_empty());
}

/// An air-gapped registry with nothing cached answers `503` to *everything*.
/// An operator reading an empty miss log should not have to work out whether
/// that means "complete" or "empty".
#[actix_web::test]
async fn a_registry_with_nothing_in_it_is_named_rather_than_left_to_be_deduced() {
    let (app, _) = lab(true).await;
    let v = get_json(&app, "/api/v1/admin/air-gap/missing").await;
    assert_eq!(v["empty_registries"][0], REG, "{v}");
    assert_eq!(v["total"], 0, "nothing has been asked for yet: {v}");
}

// ── the sink ─────────────────────────────────────────────────────────────────

/// The catch-all rewrite rule's target: it records the host and fetches
/// nothing, which is what makes "a host nobody predicted" diagnosable
/// instead of a connect timeout on a disconnected workstation.
#[actix_web::test]
async fn an_unmirrored_host_is_a_501_that_names_it_and_fetches_nothing() {
    let (app, lab) = lab(true).await;
    let resp = get(
        &app,
        "/_air-gap/unmirrored/binaries.sonarsource.com/Distribution/sonar-scanner.zip",
    )
    .await;
    assert_eq!(resp.status(), 501);
    let body = body_of(resp).await;
    assert_eq!(body["host"], "binaries.sonarsource.com");
    assert_eq!(body["code"], "unmirrored_host");

    let rows = lab
        .misses
        .list(&MissFilter {
            kind: Some(MissKind::UnmirroredHost),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].storage_key, "binaries.sonarsource.com");
    assert!(rows[0]
        .coordinate
        .as_deref()
        .unwrap()
        .contains("sonar-scanner.zip"));
}

// ── the key a bundle has to name ─────────────────────────────────────────────

/// `X-BatleHub-Storage-Key` is what a bundle export writes into the manifest,
/// so it has to be the key the proxy really used — not a plausible one.
///
/// This is the drift guard for the whole mechanism. A storage key is a
/// function of the *route*: npm's tarball is `…/{name}/{version}/tarball`,
/// which is not the file name the download URL ends in, and a manifest built
/// from the URL imports cleanly and serves nothing. Asserting the header
/// against the store means the export can never be wrong about a route this
/// test covers, and a new route that reports the wrong key fails here rather
/// than on a disconnected estate.
#[actix_web::test]
async fn the_reported_storage_key_is_where_the_bytes_actually_landed() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let storage = Arc::clone(&parts.proxy_svc.storage);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let resp = call_service(&app, admin_get(&tarball("1.1.0"))).await;
    assert_eq!(resp.status(), 200, "the connected instance serves it");
    let key = resp
        .headers()
        .get("X-BatleHub-Storage-Key")
        .expect("every artifact response says where it is kept")
        .to_str()
        .unwrap()
        .to_owned();
    // Drain the body: the cache write happens as the stream is consumed.
    let _ = actix_web::test::read_body(resp).await;

    assert_eq!(key, format!("artifact:{REG}/lodash/1.1.0/tarball"));
    assert!(
        storage.exists(&key).await.unwrap(),
        "the header named a key the store does not hold: {key}"
    );
}

/// The coordinate rides beside the key, because the key cannot be read back
/// into one: a name may contain slashes (`cli/cli`, `@scope/pkg`), so
/// `gh/cli/cli/v2.60.0/filename/gh.tar.gz` splits four plausible ways. An
/// import that guesses wrong files the *verdict* against a package nobody
/// asks about, which looks exactly like the verdict having been lost.
#[actix_web::test]
async fn the_response_names_the_coordinate_it_filed_the_bytes_under() {
    let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let resp = call_service(&app, admin_get(&tarball("1.1.0"))).await;
    assert_eq!(resp.status(), 200);
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .map(|v| v.to_str().unwrap().to_owned())
    };
    assert_eq!(header("X-BatleHub-Package").as_deref(), Some("lodash"));
    assert_eq!(header("X-BatleHub-Version").as_deref(), Some("1.1.0"));
}

// ── the bundle ───────────────────────────────────────────────────────────────

/// RFC 0008 §4.3: `import` verifies `manifest.sig` against
/// `air_gap.bundle_trusted_keys` **before reading a single blob**, then
/// writes each blob and its metadata row. This walks a bundle through that
/// door — signed, unsigned, tampered, and carried twice.
mod bundle {
    use super::*;
    use batlehub_core::services::bundle::{
        write_bundle, BundleEntry, BundleManifest, BUNDLE_VERSION,
    };
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> (SigningKey, String) {
        let signing = SigningKey::from_bytes(&[9u8; 32]);
        let public = hex::encode(signing.verifying_key().to_bytes());
        (signing, public)
    }

    fn manifest(entries: Vec<BundleEntry>) -> BundleManifest {
        BundleManifest {
            bundle_version: BUNDLE_VERSION,
            bundle_id: "bundle-1".into(),
            created_at: chrono::Utc::now(),
            source_plan: Some("mise-plan.json".into()),
            entries,
        }
    }

    fn entry(key: &str, bytes: &[u8]) -> BundleEntry {
        BundleEntry {
            registry: REG.into(),
            key: key.into(),
            digest: batlehub_core::services::integrity::sha256_hex(bytes),
            size: bytes.len() as u64,
            package_name: Some("lodash".into()),
            version: Some("1.1.0".into()),
            verdict: Some("allowed".into()),
            reason_codes: vec![],
            verified_at: Some(chrono::Utc::now()),
            git_ref: None,
        }
    }

    /// A bundle, signed by `signer`, carrying `bytes` under one key.
    fn build(signer: &SigningKey, entries: Vec<BundleEntry>, blobs: Vec<Vec<u8>>) -> Vec<u8> {
        let m = manifest(entries);
        let bytes = m.to_signed_bytes().unwrap();
        let sig = signer.sign(&bytes).to_bytes();
        let mut out = Vec::new();
        write_bundle(&mut out, &m, &sig, |digest| {
            blobs
                .iter()
                .find(|b| batlehub_core::services::integrity::sha256_hex(b) == digest)
                .cloned()
        })
        .unwrap();
        out
    }

    async fn post<S: TestService>(app: &S, body: Vec<u8>) -> actix_web::dev::ServiceResponse {
        call_service(
            app,
            TestRequest::post()
                .uri("/api/v1/admin/bundle/import")
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .insert_header(("Content-Type", "application/octet-stream"))
                .set_payload(body)
                .to_request(),
        )
        .await
    }

    /// The app's trusted key is the one the test signs with — and, like every
    /// other lab in this file, its registry client refuses to dial. Without
    /// that the fixture registry answers every coordinate, and "the import
    /// worked" and "the fixture served it anyway" are indistinguishable.
    async fn signed_lab(public: &str) -> impl TestService {
        signed_lab_with(public, None).await
    }

    async fn signed_lab_with(
        public: &str,
        verdicts: Option<Arc<dyn VerdictRepository>>,
    ) -> impl TestService {
        let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
        {
            let mut hot = parts.proxy_svc.hot.write().await;
            let real = Arc::clone(hot.registries.get(REG).expect("the fixture built one"));
            hot.registries.insert(
                REG.to_owned(),
                Arc::new(OfflineRegistryClient::new(real, REG)) as Arc<dyn RegistryClient>,
            );
            hot.air_gap = AirGapPolicy {
                enabled: true,
                record_misses: true,
                miss_retention_days: 90,
                bundle_trusted_keys: vec![public.to_owned()],
            };
            hot.verdicts = verdicts;
        }
        build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
    }

    #[actix_web::test]
    async fn a_signed_bundle_is_imported_and_carrying_it_twice_writes_once() {
        let (signing, public) = key();
        let app = signed_lab(&public).await;
        let bytes = b"the artifact".to_vec();
        let body = build(
            &signing,
            vec![entry("npm-mirror/lodash/1.1.0/lodash.tgz", &bytes)],
            vec![bytes.clone()],
        );

        let resp = post(&app, body.clone()).await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert_eq!(v["imported"], 1, "{v}");
        assert_eq!(v["rejected"], 0);
        assert_eq!(v["signer_key"], public);
        assert_eq!(v["already_imported"], false);

        // The same bundle again: an import is idempotent by id.
        let resp = post(&app, body).await;
        let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert_eq!(v["already_imported"], true);
        assert_eq!(v["imported"], 0);

        // And the history says so.
        let v = get_json(&app, "/api/v1/admin/bundle").await;
        assert_eq!(v["items"][0]["bundle_id"], "bundle-1");
        assert_eq!(v["items"][0]["blobs"], 1);
    }

    #[actix_web::test]
    async fn a_bundle_signed_by_an_untrusted_key_is_refused_before_any_blob() {
        let (signing, _) = key();
        // The instance trusts a different key.
        let app = signed_lab(&"b".repeat(64)).await;
        let bytes = b"payload".to_vec();
        let body = build(
            &signing,
            vec![entry("npm-mirror/lodash/1.1.0/lodash.tgz", &bytes)],
            vec![bytes],
        );
        let resp = post(&app, body).await;
        assert_eq!(resp.status(), 403);
        let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert!(v["message"].as_str().unwrap().contains("signature"), "{v}");
        // Nothing was recorded, because nothing was read.
        let v = get_json(&app, "/api/v1/admin/bundle").await;
        assert_eq!(v["items"].as_array().unwrap().len(), 0);
    }

    #[actix_web::test]
    async fn a_tampered_manifest_no_longer_verifies() {
        let (signing, public) = key();
        let app = signed_lab(&public).await;
        let bytes = b"payload".to_vec();
        let mut body = build(
            &signing,
            vec![entry("npm-mirror/lodash/1.1.0/lodash.tgz", &bytes)],
            vec![bytes],
        );
        // Flip a byte inside the manifest's JSON. The tar's own checksum is
        // not a signature, so the container still parses — which is exactly
        // the case the signature exists for.
        let needle = b"bundle-1";
        let at = body
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("the id is in the manifest");
        body[at] = b'X';
        let resp = post(&app, body).await;
        assert_eq!(resp.status(), 403);
    }

    /// A bundle names storage keys, and a bundle that could name *any* key
    /// would be a way to plant content anywhere. The manifest's own
    /// validation refuses it before the importer sees it.
    #[actix_web::test]
    async fn a_key_that_escapes_the_store_is_refused() {
        let (signing, public) = key();
        let app = signed_lab(&public).await;
        let bytes = b"payload".to_vec();
        let body = build(
            &signing,
            vec![entry("../../etc/cron.d/root", &bytes)],
            vec![bytes],
        );
        let resp = post(&app, body).await;
        assert_eq!(resp.status(), 400);
        let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert!(
            v["message"].as_str().unwrap().contains("storage key"),
            "{v}"
        );
    }

    /// An entry naming a registry this instance does not have is rejected
    /// per entry, and the rest of the bundle still lands.
    #[actix_web::test]
    async fn an_entry_for_an_unknown_registry_is_rejected_without_failing_the_import() {
        let (signing, public) = key();
        let app = signed_lab(&public).await;
        let good = b"good".to_vec();
        let orphan = b"orphan".to_vec();
        let mut stray = entry("elsewhere/pkg/1.0/x.tgz", &orphan);
        stray.registry = "no-such-registry".into();
        let body = build(
            &signing,
            vec![entry("npm-mirror/lodash/1.1.0/lodash.tgz", &good), stray],
            vec![good, orphan],
        );
        let resp = post(&app, body).await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert_eq!(v["imported"], 1);
        assert_eq!(v["rejected"], 1);
        assert!(
            v["rejections"][0]
                .as_str()
                .unwrap()
                .contains("no registry named"),
            "{v}"
        );
    }

    #[actix_web::test]
    async fn an_entry_whose_blob_is_missing_is_rejected_rather_than_written_empty() {
        let (signing, public) = key();
        let app = signed_lab(&public).await;
        let bytes = b"never carried".to_vec();
        // The manifest names it; no blob is written for it.
        let body = build(
            &signing,
            vec![entry("npm-mirror/lodash/1.1.0/lodash.tgz", &bytes)],
            vec![],
        );
        let resp = post(&app, body).await;
        let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert_eq!(v["imported"], 0);
        assert_eq!(v["rejected"], 1);
        assert!(
            v["rejections"][0].as_str().unwrap().contains("no blob"),
            "{v}"
        );
    }

    /// The round trip the whole design is for: bytes go in through the
    /// import door and come back out of the read path.
    ///
    /// It is the only test that can catch a wrong key, and a wrong key is
    /// easy to write: a storage key is a function of the **route**, not of
    /// the URL a client started from. npm's tarball lives under
    /// `…/{name}/{version}/tarball`, and a bundle naming
    /// `…/{name}/{version}/lodash.tgz` — the file name, which is what a
    /// download URL ends in — imports cleanly, reports one blob written, and
    /// serves `503` forever.
    #[actix_web::test]
    async fn an_imported_artifact_is_served_from_the_key_the_read_path_reads() {
        let (signing, public) = key();
        let app = signed_lab(&public).await;
        let bytes = b"the tarball, carried across".to_vec();
        let body = build(
            &signing,
            vec![entry("npm-mirror/lodash/1.1.0/tarball", &bytes)],
            vec![bytes.clone()],
        );
        assert_eq!(post(&app, body).await.status(), 200);

        let resp = call_service(&app, admin_get(&tarball("1.1.0"))).await;
        assert_eq!(
            resp.status(),
            200,
            "an instance that will not dial serves what the bundle gave it"
        );
        assert_eq!(
            actix_web::test::read_body(resp).await.as_ref(),
            bytes.as_slice()
        );

        // And a coordinate the bundle did not carry is still a miss.
        let resp = call_service(&app, admin_get(&tarball("9.9.9"))).await;
        assert_eq!(resp.status(), 503);

        // The registry is no longer reported as holding nothing — the pair of
        // `a_registry_with_nothing_in_it_is_named_rather_than_left_to_be_deduced`,
        // and the reason that report has to be recomputed rather than taken
        // once at boot.
        let v = get_json(&app, "/api/v1/admin/air-gap/missing").await;
        assert!(
            v["empty_registries"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(true),
            "an imported registry is not empty: {v}"
        );
    }

    /// RFC 0008 §13 decision 4: without the verdict the bundle carries, an
    /// imported artifact is `SCAN_PENDING` on a registry with `[security]` —
    /// fail-closed, so the instance refuses everything it just imported.
    #[actix_web::test]
    async fn the_verdict_travels_with_the_bytes() {
        let (signing, public) = key();
        let verdicts = batlehub_adapters::in_memory::InMemoryVerdictRepository::new();
        let app = signed_lab_with(
            &public,
            Some(Arc::clone(&verdicts) as Arc<dyn VerdictRepository>),
        )
        .await;

        let bytes = b"judged on the connected side".to_vec();
        let mut e = entry("npm-mirror/lodash/1.1.0/tarball", &bytes);
        e.package_name = Some("lodash".into());
        e.verdict = Some("allowed".into());
        e.reason_codes = vec![];
        let body = build(&signing, vec![e], vec![bytes]);
        assert_eq!(post(&app, body).await.status(), 200);

        let stored = verdicts
            .get(&PackageId::new(REG, "lodash", "1.1.0"))
            .await
            .unwrap()
            .expect("the verdict crossed with the bytes");
        assert_eq!(stored.state.as_str(), "allowed");
        assert!(
            stored.policy_ref.starts_with("bundle:"),
            "it says where the judgement came from, and never claims to be a local check: {}",
            stored.policy_ref
        );
    }

    /// A bundle is signed by whoever holds the key, and a signer who could
    /// write `denied` into this instance's verdict table could refuse any
    /// package on it. Only a *served* judgement crosses.
    #[actix_web::test]
    async fn a_bundle_cannot_plant_a_refusal() {
        let (signing, public) = key();
        let verdicts = batlehub_adapters::in_memory::InMemoryVerdictRepository::new();
        let app = signed_lab_with(
            &public,
            Some(Arc::clone(&verdicts) as Arc<dyn VerdictRepository>),
        )
        .await;

        let bytes = b"payload".to_vec();
        let mut e = entry("npm-mirror/lodash/1.1.0/tarball", &bytes);
        e.package_name = Some("lodash".into());
        e.verdict = Some("denied".into());
        let body = build(&signing, vec![e], vec![bytes]);
        assert_eq!(post(&app, body).await.status(), 200);
        assert!(
            verdicts
                .get(&PackageId::new(REG, "lodash", "1.1.0"))
                .await
                .unwrap()
                .is_none(),
            "a bundle may carry evidence that something was allowed, never an order to refuse"
        );
    }

    /// RFC 0008 §13.3. A forge resolves a ref *before* it fetches anything,
    /// and a disconnected instance has no forge to ask — so without the
    /// `ref → commit` row the bundle carries, a forge registry answers `503`
    /// to every coordinate in the bundle it just accepted.
    #[actix_web::test]
    async fn a_ref_resolution_crosses_the_gap_with_the_bytes() {
        let (signing, public) = key();
        let parts = local_registry_app_parts(REG, "npm", RegistryMode::Proxy, None);
        let refs = batlehub_adapters::in_memory::InMemoryRefResolutionRepository::new();
        {
            let mut hot = parts.proxy_svc.hot.write().await;
            hot.air_gap = AirGapPolicy {
                enabled: true,
                record_misses: true,
                miss_retention_days: 90,
                bundle_trusted_keys: vec![public.clone()],
            };
            hot.ref_resolutions =
                Some(Arc::clone(&refs) as Arc<dyn batlehub_core::ports::RefResolutionRepository>);
        }
        let app =
            build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

        let bytes = b"the tag's bytes".to_vec();
        let mut e = entry("npm-mirror/cli/cli/v2.60.0/filename/gh.tar.gz", &bytes);
        e.git_ref = Some(batlehub_core::services::bundle::BundleRef {
            owner_repo: "cli/cli".into(),
            git_ref: "v2.60.0".into(),
            kind: "tag".into(),
            sha: "a".repeat(40),
        });
        let body = build(&signing, vec![e], vec![bytes]);
        assert_eq!(post(&app, body).await.status(), 200);

        let stored = batlehub_core::ports::RefResolutionRepository::get(
            refs.as_ref(),
            REG,
            "cli/cli",
            "v2.60.0",
        )
        .await
        .unwrap()
        .expect("the resolution crossed with the bytes");
        assert_eq!(stored.sha, "a".repeat(40));
        assert_eq!(stored.kind.as_str(), "tag");
    }

    #[actix_web::test]
    async fn importing_needs_a_system_verb() {
        let (signing, public) = key();
        let app = signed_lab(&public).await;
        let bytes = b"payload".to_vec();
        let body = build(
            &signing,
            vec![entry("npm-mirror/lodash/1.1.0/lodash.tgz", &bytes)],
            vec![bytes],
        );
        let resp = call_service(
            &app,
            TestRequest::post()
                .uri("/api/v1/admin/bundle/import")
                .insert_header(("Authorization", bearer(USER_TOKEN)))
                .set_payload(body)
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 403);
    }
}
