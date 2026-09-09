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
            synthesise_listings: false,
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
            synthesise_listings: false,
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
            facts: None,
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
                synthesise_listings: false,
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

    /// [`signed_lab_with`] for another registry kind, with listings
    /// synthesised — the disconnected instance of RFC 0008-bis.
    async fn signed_lab_of(public: &str, registry_type: &str) -> impl TestService {
        let parts = local_registry_app_parts_with_artifact_meta(
            REG,
            registry_type,
            RegistryMode::Proxy,
            None,
            None,
            InMemoryArtifactMetaRepository::arc(),
        );
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
                synthesise_listings: true,
            };
            hot.policies.insert(
                REG.to_owned(),
                Arc::new(RegistryPolicy {
                    metadata_ttl: None,
                    firewall_only: false,
                    serve_stale_metadata: false,
                    artifact_ttl: None,
                    rules: vec![],
                }),
            );
        }
        build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
    }

    /// RFC 0008-bis §13.7: a provider crosses the gap as three artifacts —
    /// the archive, its checksum list and the list's signature — plus the
    /// facts the export read off the connected side's download document.
    /// The disconnected instance then composes the listing and the download
    /// document Terraform installs through, with the publisher's keys and
    /// the archive's file name from the list, and signs nothing itself.
    #[actix_web::test]
    async fn an_imported_provider_gets_its_listing_and_a_download_document_it_can_verify() {
        let (signing, public) = key();
        let app = signed_lab_of(&public, "terraform").await;
        let archive = b"PK the provider".to_vec();
        let digest = batlehub_core::services::integrity::sha256_hex(&archive);
        let list = format!(
            "{digest}  terraform-provider-null_3.2.2_linux_amd64.zip\n{}  terraform-provider-null_3.2.2_darwin_arm64.zip\n",
            "0".repeat(64)
        )
        .into_bytes();
        let sig = b"-----BEGIN PGP SIGNATURE-----".to_vec();
        let provider = |key: &str, bytes: &[u8]| BundleEntry {
            package_name: Some("providers/hashicorp/null".into()),
            version: Some("3.2.2".into()),
            ..entry(key, bytes)
        };
        let mut with_facts = provider(
            &format!("{REG}/providers/hashicorp/null/3.2.2/linux/amd64"),
            &archive,
        );
        with_facts.facts = Some(serde_json::json!({ "terraform": {
            "protocols": ["5.0"],
            "signing_keys": { "gpg_public_keys": [{ "key_id": "34365D9472D7468F", "ascii_armor": "-----BEGIN PGP PUBLIC KEY BLOCK-----" }] },
        } }));
        let body = build(
            &signing,
            vec![
                with_facts,
                provider(
                    &format!("{REG}/providers/hashicorp/null/3.2.2/shasums"),
                    &list,
                ),
                provider(
                    &format!("{REG}/providers/hashicorp/null/3.2.2/shasums.sig"),
                    &sig,
                ),
            ],
            vec![archive.clone(), list.clone(), sig.clone()],
        );
        let resp = post(&app, body).await;
        assert_eq!(resp.status(), 200);
        let v: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert_eq!(v["imported"], 3, "{v}");
        assert_eq!(v["rejected"], 0, "{v}");

        // The listing: one version, its platform, its protocols.
        let resp = get(
            &app,
            &format!("/proxy/{REG}/v1/providers/hashicorp/null/versions"),
        )
        .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
        let listing = body_of(resp).await;
        assert_eq!(listing["versions"][0]["version"], "3.2.2");
        assert_eq!(
            listing["versions"][0]["protocols"],
            serde_json::json!(["5.0"])
        );
        assert_eq!(listing["versions"][0]["platforms"][0]["os"], "linux");

        // The download document: the archive's digest and its name in the
        // list, the three URLs on this host, the publisher's keys.
        let resp = get(
            &app,
            &format!("/proxy/{REG}/v1/providers/hashicorp/null/3.2.2/download/linux/amd64"),
        )
        .await;
        assert_eq!(resp.status(), 200);
        assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
        let doc = body_of(resp).await;
        assert_eq!(doc["shasum"], digest);
        assert_eq!(
            doc["filename"],
            "terraform-provider-null_3.2.2_linux_amd64.zip"
        );
        assert_eq!(doc["os"], "linux");
        assert_eq!(doc["arch"], "amd64");
        assert_eq!(doc["protocols"], serde_json::json!(["5.0"]));
        assert_eq!(
            doc["signing_keys"]["gpg_public_keys"][0]["key_id"],
            "34365D9472D7468F"
        );
        for (field, tail) in [
            ("download_url", "/3.2.2/artifact/linux/amd64"),
            ("shasums_url", "/3.2.2/shasums"),
            ("shasums_signature_url", "/3.2.2/shasums.sig"),
        ] {
            let url = doc[field].as_str().unwrap_or_default();
            assert!(
                url.contains(&format!("/proxy/{REG}/v1/providers/hashicorp/null{tail}")),
                "{field} = {url}"
            );
        }

        // And the three the document names are served, bytes intact.
        for (tail, bytes) in [
            ("artifact/linux/amd64", &archive),
            ("shasums", &list),
            ("shasums.sig", &sig),
        ] {
            let resp = get(
                &app,
                &format!("/proxy/{REG}/v1/providers/hashicorp/null/3.2.2/{tail}"),
            )
            .await;
            assert_eq!(resp.status(), 200, "{tail}");
            let served = actix_web::test::read_body(resp).await;
            assert_eq!(&served[..], &bytes[..], "{tail}");
        }

        // A platform the bundle did not carry is the `503` of RFC 0008.
        let resp = get(
            &app,
            &format!("/proxy/{REG}/v1/providers/hashicorp/null/3.2.2/download/darwin/arm64"),
        )
        .await;
        assert_eq!(resp.status(), 503);
    }

    /// A provider archive without its checksum list and signature is
    /// listed by nothing: Terraform would fetch the document, then the
    /// list, and refuse — so the document is not composed.
    #[actix_web::test]
    async fn a_provider_archive_alone_gets_no_download_document() {
        let (signing, public) = key();
        let app = signed_lab_of(&public, "terraform").await;
        let archive = b"PK the provider".to_vec();
        let mut alone = entry(
            &format!("{REG}/providers/hashicorp/null/3.2.2/linux/amd64"),
            &archive,
        );
        alone.package_name = Some("providers/hashicorp/null".into());
        alone.version = Some("3.2.2".into());
        let resp = post(&app, build(&signing, vec![alone], vec![archive])).await;
        assert_eq!(resp.status(), 200);

        let resp = get(
            &app,
            &format!("/proxy/{REG}/v1/providers/hashicorp/null/versions"),
        )
        .await;
        assert_eq!(resp.status(), 200, "the version is held, so it is listed");
        let resp = get(
            &app,
            &format!("/proxy/{REG}/v1/providers/hashicorp/null/3.2.2/download/linux/amd64"),
        )
        .await;
        assert_eq!(resp.status(), 503);
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
                synthesise_listings: false,
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

// ── RFC 0008-bis: listings synthesised from the held set ─────────────────────
//
// The disconnected instance holds versions a bundle brought and no document
// for any package. Phase 0 measured what npm, pip and mise do with the `503`
// that follows (retry it; never ask for the artifact they could have had).
// These are the answer: a listing composed from the held set, in the
// registry's own shape, through the same filters a fetched one goes through,
// and marked as what it is.

use batlehub_adapters::in_memory::InMemoryArtifactMetaRepository;
use batlehub_core::entities::PackageMetadata;
use batlehub_core::ports::{ArtifactCacheMeta, ArtifactMetaRecord, CacheEntry};

/// A disconnected app of one registry holding what `held` names, each
/// `(package, version, artifact suffix)` as a bundle import leaves it: the
/// bytes, the artifact-meta row and the `meta:` entry with the digest. The
/// suffix is the one the route files the bytes under — npm's `tarball`, a
/// PyPI filename, a forge `filename/<asset>` — because a key is a function
/// of the route (RFC 0008 §14.1) and the join reads it back off the key.
async fn holding_lab(
    registry_type: &str,
    synthesise: bool,
    held: &[(&str, &str, Option<&str>)],
) -> (impl TestService, Lab) {
    let with_extra: Vec<(&str, &str, Option<&str>, serde_json::Value)> = held
        .iter()
        .map(|(n, v, a)| (*n, *v, *a, serde_json::Value::Null))
        .collect();
    holding_lab_extra(registry_type, synthesise, &with_extra).await
}

/// [`holding_lab`] with the `meta:` entry's `extra` per row — what the
/// import files there from the bytes (a crate's index facts, §13.4).
async fn holding_lab_extra(
    registry_type: &str,
    synthesise: bool,
    held: &[(&str, &str, Option<&str>, serde_json::Value)],
) -> (impl TestService, Lab) {
    let meta = InMemoryArtifactMetaRepository::arc();
    let parts = local_registry_app_parts_with_artifact_meta(
        REG,
        registry_type,
        RegistryMode::Proxy,
        None,
        None,
        meta.clone(),
    );
    let misses = InMemoryMissRecorder::new();
    let repo = Arc::clone(&parts.proxy_svc.repo);
    for (i, (name, version, artifact, extra)) in held.iter().enumerate() {
        let key = match artifact {
            Some(a) => format!("{REG}/{name}/{version}/{a}"),
            None => format!("{REG}/{name}/{version}"),
        };
        let digest = format!("{:02x}", i).repeat(32);
        // The bytes too, so a listed version is served by the next request —
        // the invariant the whole design rests on.
        parts
            .proxy_svc
            .storage
            .store(
                &format!("artifact:{key}"),
                bytes::Bytes::from_static(b"held bytes"),
                Default::default(),
            )
            .await
            .unwrap();
        meta.record_artifact(ArtifactMetaRecord {
            key: &format!("artifact:{key}"),
            registry: REG,
            package_name: name,
            version,
            size: Some(1000 + i as u64),
            checksum: Some(&digest),
        })
        .await
        .unwrap();
        parts
            .proxy_svc
            .cache
            .set(
                &format!("meta:{key}"),
                CacheEntry {
                    metadata: PackageMetadata {
                        id: PackageId::new(REG, *name, *version),
                        published_at: None,
                        download_url: None,
                        checksum: Some(digest),
                        is_signed: None,
                        extra: extra.clone(),
                        cache_control: None,
                    },
                    cached_at: chrono::Utc::now(),
                    expires_at: None,
                },
                None,
            )
            .await
            .unwrap();
    }
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
            synthesise_listings: synthesise,
        };
        hot.miss_recorder = Some(Arc::clone(&misses) as Arc<dyn MissRecorder>);
        hot.policies.insert(
            REG.to_owned(),
            Arc::new(RegistryPolicy {
                // The imported entries never expire on a disconnected instance (RFC
                // 0008 §14.2); a zero TTL here would send every resolve to the
                // offline client and refuse the very bytes the listing named.
                metadata_ttl: None,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![Box::new(BlockListRule::new(Arc::clone(&repo)))],
            }),
        );
    }
    // The cargo index route refuses a registry with no sparse index to
    // proxy before it reaches the document path; the disconnected instance
    // has one configured and never dials it.
    let indexes = if registry_type == "cargo" {
        batlehub_web::CargoIndexMap::new(std::collections::HashMap::from([(
            REG.to_owned(),
            batlehub_web::CargoIndexProxy {
                http: reqwest::Client::new(),
                index_url: "http://index.invalid".to_owned(),
            },
        )]))
    } else {
        batlehub_web::CargoIndexMap::default()
    };
    let app = build_local_registry_app(parts, indexes, None).await;
    (app, Lab { misses, repo })
}

fn header<'a>(resp: &'a actix_web::dev::ServiceResponse, name: &str) -> Option<&'a str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

async fn document_misses(lab: &Lab) -> Vec<String> {
    lab.misses
        .list(&MissFilter {
            kind: Some(MissKind::Document),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.storage_key)
        .collect()
}

#[actix_web::test]
async fn a_held_npm_package_gets_a_packument_composed_from_its_held_versions() {
    let (app, lab) = holding_lab(
        "npm",
        true,
        &[
            ("lodash", "1.0.0", Some("tarball")),
            ("lodash", "1.2.0", Some("tarball")),
        ],
    )
    .await;
    let resp = get(&app, &format!("/proxy/{REG}/lodash")).await;
    assert_eq!(resp.status(), 200, "a held package's listing is answered");
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    assert_eq!(header(&resp, "X-BatleHub-Listing-Held"), Some("2"));
    let body = body_of(resp).await;
    assert_eq!(body["name"], "lodash");
    assert_eq!(
        body["dist-tags"]["latest"], "1.2.0",
        "the pointer is the highest held"
    );
    let versions = body["versions"].as_object().unwrap();
    assert_eq!(versions.len(), 2, "{body}");
    let tarball_url = versions["1.2.0"]["dist"]["tarball"].as_str().unwrap();
    assert!(
        tarball_url.ends_with(&format!("/proxy/{REG}/lodash/1.2.0/tarball")),
        "the tarball URL points at this proxy: {tarball_url}"
    );
    assert!(versions["1.2.0"]["dist"]["integrity"]
        .as_str()
        .unwrap()
        .starts_with("sha256-"));
    assert!(
        document_misses(&lab).await.is_empty(),
        "an answered listing is not a miss"
    );
    // Every version the listing named is served by the next request (§5.1).
    let resp = get(&app, &tarball("1.2.0")).await;
    let status = resp.status();
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(
        status,
        200,
        "a listed version is a served version: {}",
        String::from_utf8_lossy(&body)
    );
}

#[actix_web::test]
async fn a_synthesised_listing_goes_through_the_block_list_like_a_fetched_one() {
    let (app, lab) = holding_lab(
        "npm",
        true,
        &[
            ("lodash", "1.0.0", Some("tarball")),
            ("lodash", "1.2.0", Some("tarball")),
        ],
    )
    .await;
    lab.repo
        .set_status(
            &PackageId::new(REG, "lodash", "1.2.0"),
            PackageStatus::Blocked {
                reason: "known bad".into(),
                blocked_by: "admin".into(),
                blocked_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
    let resp = get(&app, &format!("/proxy/{REG}/lodash")).await;
    assert_eq!(resp.status(), 200);
    let body = body_of(resp).await;
    let versions = body["versions"].as_object().unwrap();
    assert!(
        !versions.contains_key("1.2.0"),
        "a blocked held version is not listed: {body}"
    );
    assert_eq!(
        body["dist-tags"]["latest"], "1.0.0",
        "and the pointer moves with it"
    );
}

#[actix_web::test]
async fn an_unheld_package_is_still_the_503_and_the_recorded_miss() {
    let (app, lab) = holding_lab("npm", true, &[("lodash", "1.0.0", Some("tarball"))]).await;
    let resp = get(&app, &format!("/proxy/{REG}/left-pad")).await;
    assert_eq!(resp.status(), 503, "nothing held, nothing composed");
    assert!(header(&resp, "X-BatleHub-Listing").is_none());
    let misses = document_misses(&lab).await;
    assert!(
        misses.iter().any(|k| k.contains("left-pad")),
        "the miss is recorded for the next bundle: {misses:?}"
    );
}

#[actix_web::test]
async fn synthesis_turned_off_is_rfc_0008s_refusal() {
    let (app, lab) = holding_lab("npm", false, &[("lodash", "1.0.0", Some("tarball"))]).await;
    let resp = get(&app, &format!("/proxy/{REG}/lodash")).await;
    assert_eq!(resp.status(), 503);
    assert!(!document_misses(&lab).await.is_empty());
    // The artifact itself is unaffected either way: held is held.
    let resp = get(&app, &tarball("1.0.0")).await;
    assert_eq!(
        resp.status(),
        200,
        "the held tarball is served whatever the listing does"
    );
}

#[actix_web::test]
async fn a_held_pypi_package_gets_the_json_simple_page_and_the_html_one() {
    let (app, _lab) = holding_lab(
        "pypi",
        true,
        &[
            ("six", "1.17.0", Some("six-1.17.0-py2.py3-none-any.whl")),
            ("six", "1.17.0", Some("six-1.17.0.tar.gz")),
        ],
    )
    .await;
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/simple/six/"))
        .insert_header(("Authorization", "Bearer admin-token"))
        .insert_header((
            "Accept",
            "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.1",
        ))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    assert!(header(&resp, "content-type")
        .unwrap()
        .starts_with("application/vnd.pypi.simple.v1+json"));
    let body = body_of(resp).await;
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{body}");
    let wheel = files
        .iter()
        .find(|f| f["filename"] == "six-1.17.0-py2.py3-none-any.whl")
        .unwrap();
    let url = wheel["url"].as_str().unwrap();
    assert!(
        url.contains(&format!(
            "/proxy/{REG}/packages/six-1.17.0-py2.py3-none-any.whl#sha256="
        )),
        "{url}"
    );
    assert_eq!(wheel["hashes"]["sha256"], "00".repeat(32));

    // The PEP 503 HTML page too (phase 3), for a pip that does not negotiate.
    let resp = get(&app, &format!("/proxy/{REG}/simple/six/")).await;
    assert_eq!(resp.status(), 200);
    assert!(header(&resp, "content-type")
        .unwrap()
        .starts_with("text/html"));
}

#[actix_web::test]
async fn a_forge_answers_the_release_listing_and_the_release_by_tag_from_held_assets() {
    let (app, lab) = holding_lab(
        "github",
        true,
        &[
            (
                "cli/cli",
                "v2.60.0",
                Some("filename/gh_2.60.0_linux_amd64.tar.gz"),
            ),
            (
                "cli/cli",
                "v2.60.0",
                Some("filename/gh_2.60.0_macOS_arm64.zip"),
            ),
            (
                "cli/cli",
                "v2.59.0",
                Some("filename/gh_2.59.0_linux_amd64.tar.gz"),
            ),
        ],
    )
    .await;

    // The listing: one release per held tag, newest first.
    let resp = get(&app, &format!("/proxy/{REG}/cli/cli/releases")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let body = body_of(resp).await;
    let releases = body.as_array().unwrap();
    assert_eq!(releases.len(), 2, "{body}");
    assert_eq!(releases[0]["tag_name"], "v2.60.0");
    assert_eq!(releases[0]["assets"].as_array().unwrap().len(), 2);

    // The release by tag — what a pinned `mise install` asks for first —
    // with every download link pointing home.
    let resp = get(&app, &format!("/proxy/{REG}/cli/cli/releases/tags/v2.60.0")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    assert_eq!(header(&resp, "X-BatleHub-Listing-Held"), Some("2"));
    let body = body_of(resp).await;
    assert_eq!(body["tag_name"], "v2.60.0");
    let url = body["assets"][0]["browser_download_url"].as_str().unwrap();
    assert!(
        url.contains(&format!(
            "/proxy/{REG}/cli/cli/releases/download/v2.60.0/gh_2.60.0_"
        )),
        "{url}"
    );
    assert_eq!(
        body["assets"][0]["url"], body["assets"][0]["browser_download_url"],
        "the API link mise reads points at the same held download, not at an asset id the instance cannot answer"
    );

    // A tag nothing is held for is the 503 of before, and a recorded miss —
    // filed as a *document* under the package, naming the tag it asked for
    // and the tags that were held (RFC 0008-bis §4.4).
    let resp = get(&app, &format!("/proxy/{REG}/cli/cli/releases/tags/v9.9.9")).await;
    assert_eq!(resp.status(), 503);
    let rows = lab.misses.list(&MissFilter::default()).await.unwrap();
    let row = rows
        .iter()
        .find(|m| m.storage_key == "cli/cli (release)")
        .unwrap_or_else(|| panic!("no document miss for the release by tag: {rows:?}"));
    assert_eq!(row.kind, MissKind::Document);
    assert_eq!(row.requested_version.as_deref(), Some("v9.9.9"));
    assert_eq!(row.held_versions, vec!["v2.60.0", "v2.59.0"]);
    assert!(
        !rows.iter().any(|m| m.kind == MissKind::Artifact),
        "the release by tag is not an artifact miss: {rows:?}"
    );

    // A blocked tag is a refusal, not a listing.
    lab.repo
        .set_status(
            &PackageId::new(REG, "cli/cli", "v2.59.0"),
            PackageStatus::Blocked {
                reason: "pulled".into(),
                blocked_by: "admin".into(),
                blocked_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();
    let resp = get(&app, &format!("/proxy/{REG}/cli/cli/releases/tags/v2.59.0")).await;
    assert_eq!(
        resp.status(),
        403,
        "a block on the tag refuses its composed release"
    );
    let resp = get(&app, &format!("/proxy/{REG}/cli/cli/releases")).await;
    let body = body_of(resp).await;
    assert_eq!(
        body.as_array().unwrap().len(),
        1,
        "and drops it from the listing: {body}"
    );
}

#[actix_web::test]
async fn an_artifact_miss_under_synthesis_names_the_version_and_what_was_held() {
    let (app, lab) = holding_lab(
        "npm",
        true,
        &[
            ("lodash", "1.0.0", Some("tarball")),
            ("lodash", "1.2.0", Some("tarball")),
        ],
    )
    .await;
    // A client with a lock naming a version this instance does not hold.
    let resp = get(&app, &tarball("1.1.0")).await;
    assert_eq!(resp.status(), 503);
    let rows = lab
        .misses
        .list(&MissFilter {
            kind: Some(MissKind::Artifact),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].requested_version.as_deref(), Some("1.1.0"));
    assert_eq!(
        rows[0].held_versions,
        vec!["1.2.0", "1.0.0"],
        "what the synthesised packument had named, newest first"
    );
    // The admin listing carries both, so the CLI and the console can print
    // them without a second call.
    let resp = get(&app, "/api/v1/admin/air-gap/missing?kind=artifact").await;
    assert_eq!(resp.status(), 200);
    let body = body_of(resp).await;
    assert_eq!(body["items"][0]["requested_version"], "1.1.0");
    assert_eq!(
        body["items"][0]["held_versions"],
        serde_json::json!(["1.2.0", "1.0.0"])
    );
}

#[actix_web::test]
async fn without_synthesis_a_miss_names_its_version_but_no_held_set() {
    let (app, lab) = holding_lab("npm", false, &[("lodash", "1.0.0", Some("tarball"))]).await;
    let resp = get(&app, &tarball("1.1.0")).await;
    assert_eq!(resp.status(), 503);
    let rows = lab.misses.list(&MissFilter::default()).await.unwrap();
    let row = rows.iter().find(|m| m.kind == MissKind::Artifact).unwrap();
    assert_eq!(row.requested_version.as_deref(), Some("1.1.0"));
    assert!(
        row.held_versions.is_empty(),
        "the client saw no listing to compare with: {row:?}"
    );
}

// ── RFC 0008-bis phase 3: the remaining renderers, one route each ─────────────

#[actix_web::test]
async fn a_held_crate_gets_a_sparse_index_line_from_the_facts_the_import_filed() {
    let facts = serde_json::json!({ "cargo": { "deps": [], "features": {}, "links": null } });
    let (app, _lab) = holding_lab_extra(
        "cargo",
        true,
        &[("unicode-xid", "0.2.5", Some("dl"), facts)],
    )
    .await;
    let resp = get(&app, &format!("/proxy/{REG}/registry/un/ic/unicode-xid")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let body = actix_web::test::read_body(resp).await;
    let line: serde_json::Value = serde_json::from_slice(body.trim_ascii_end()).unwrap();
    assert_eq!(line["name"], "unicode-xid");
    assert_eq!(line["vers"], "0.2.5");
    assert_eq!(line["yanked"], false);
    assert_eq!(line["cksum"], "00".repeat(32));
}

#[actix_web::test]
async fn a_held_crate_without_facts_is_not_listed() {
    let (app, lab) = holding_lab("cargo", true, &[("unicode-xid", "0.2.5", Some("dl"))]).await;
    let resp = get(&app, &format!("/proxy/{REG}/registry/un/ic/unicode-xid")).await;
    assert_eq!(
        resp.status(),
        503,
        "a line without its deps would be a lie cargo builds against"
    );
    assert!(!document_misses(&lab).await.is_empty());
}

#[actix_web::test]
async fn a_held_go_module_gets_its_list_latest_and_info() {
    let module = "github.com/google/uuid";
    let (app, lab) = holding_lab(
        "goproxy",
        true,
        &[
            (module, "v1.5.0", Some("zip")),
            (module, "v1.5.0", Some("mod")),
            (module, "v1.6.0", Some("zip")),
        ],
    )
    .await;
    let resp = get(&app, &format!("/proxy/{REG}/{module}/@v/list")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let body = actix_web::test::read_body(resp).await;
    assert_eq!(std::str::from_utf8(&body).unwrap(), "v1.5.0\nv1.6.0\n");

    let resp = get(&app, &format!("/proxy/{REG}/{module}/@latest")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(body_of(resp).await["Version"], "v1.6.0");

    let resp = get(&app, &format!("/proxy/{REG}/{module}/@v/v1.5.0.info")).await;
    assert_eq!(resp.status(), 200, "the .info is composed for a held zip");
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    assert_eq!(body_of(resp).await["Version"], "v1.5.0");

    let resp = get(&app, &format!("/proxy/{REG}/{module}/@v/v1.4.0.info")).await;
    assert_eq!(resp.status(), 503);
    let rows = lab.misses.list(&MissFilter::default()).await.unwrap();
    let row = rows
        .iter()
        .find(|m| m.storage_key == format!("{module} (info)"))
        .unwrap_or_else(|| {
            panic!("the unheld .info is a document miss naming its version: {rows:?}")
        });
    assert_eq!(row.kind, MissKind::Document);
    assert_eq!(row.requested_version.as_deref(), Some("v1.4.0"));
    assert_eq!(row.held_versions, vec!["v1.6.0", "v1.5.0"]);
}

#[actix_web::test]
async fn a_held_maven_artifact_gets_its_metadata_xml() {
    let (app, _lab) = holding_lab(
        "maven",
        true,
        &[
            (
                "org.apache.commons:commons-lang3",
                "3.12.0",
                Some("commons-lang3-3.12.0.jar"),
            ),
            (
                "org.apache.commons:commons-lang3",
                "3.12.0",
                Some("commons-lang3-3.12.0.pom"),
            ),
        ],
    )
    .await;
    let resp = get(
        &app,
        &format!("/proxy/{REG}/maven2/org/apache/commons/commons-lang3/maven-metadata.xml"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let body = actix_web::test::read_body(resp).await;
    let xml = std::str::from_utf8(&body).unwrap();
    assert!(xml.contains("<release>3.12.0</release>"), "{xml}");
    assert!(xml.contains("<version>3.12.0</version>"), "{xml}");

    // Maven asks for a checksum beside the document; a composed document
    // answers its own (RFC 0008-bis §13.5).
    let resp = get(
        &app,
        &format!("/proxy/{REG}/maven2/org/apache/commons/commons-lang3/maven-metadata.xml.sha1"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let sha1 = actix_web::test::read_body(resp).await;
    let expected = {
        use sha1::Digest as _;
        hex::encode(sha1::Sha1::digest(xml.as_bytes()))
    };
    assert_eq!(std::str::from_utf8(&sha1).unwrap(), expected);
    let resp = get(
        &app,
        &format!("/proxy/{REG}/maven2/org/apache/commons/commons-lang3/maven-metadata.xml.md5"),
    )
    .await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn a_held_nuget_package_gets_its_flat_index() {
    let (app, _lab) = holding_lab(
        "nuget",
        true,
        &[(
            "newtonsoft.json",
            "13.0.3",
            Some("newtonsoft.json.13.0.3.nupkg"),
        )],
    )
    .await;
    let resp = get(
        &app,
        &format!("/proxy/{REG}/nuget/v3/flat/newtonsoft.json/index.json"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    assert_eq!(
        body_of(resp).await["versions"],
        serde_json::json!(["13.0.3"])
    );
}

#[actix_web::test]
async fn a_held_gitlab_release_link_gets_the_listing_and_the_release_by_tag() {
    let (app, _lab) = holding_lab(
        "gitlab",
        true,
        &[("group/tool", "v2.0.0", Some("link/tool-linux-amd64"))],
    )
    .await;
    let resp = get(&app, &format!("/proxy/{REG}/group/tool/-/releases")).await;
    assert_eq!(resp.status(), 200);
    let body = body_of(resp).await;
    assert_eq!(body[0]["tag_name"], "v2.0.0");
    let url = body[0]["assets"]["links"][0]["url"].as_str().unwrap();
    assert!(
        url.ends_with("/group/tool/-/releases/v2.0.0/downloads/tool-linux-amd64"),
        "{url}"
    );
    let resp = get(&app, &format!("/proxy/{REG}/group/tool/-/releases/v2.0.0")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    assert_eq!(body_of(resp).await["tag_name"], "v2.0.0");
}

// ── RFC 0008-bis §13.6: the renderers that open the artifact at import ───────

#[actix_web::test]
async fn a_held_gem_gets_its_compact_index_from_the_gemspec_facts() {
    let facts = serde_json::json!({ "rubygems": { "platform": "ruby", "dependencies": [] } });
    let (app, lab) =
        holding_lab_extra("rubygems", true, &[("rake", "13.2.1", Some("gem"), facts)]).await;
    let resp = get(&app, &format!("/proxy/{REG}/info/rake")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let info = actix_web::test::read_body(resp).await;
    let info = std::str::from_utf8(&info).unwrap().to_owned();
    assert!(info.starts_with("---\n13.2.1 |checksum:"), "{info}");

    let resp = get(&app, &format!("/proxy/{REG}/versions")).await;
    assert_eq!(
        resp.status(),
        200,
        "the registry-wide document is composed from every held gem"
    );
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let versions = actix_web::test::read_body(resp).await;
    let versions = std::str::from_utf8(&versions).unwrap();
    let md5 = {
        use md5::Digest as _;
        hex::encode(md5::Md5::digest(info.as_bytes()))
    };
    assert!(
        versions.contains(&format!("rake 13.2.1 {md5}")),
        "{versions}"
    );

    let resp = get(&app, &format!("/proxy/{REG}/api/v1/versions/rake.json")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(body_of(resp).await[0]["number"], "13.2.1");

    // A gem imported without its gemspec facts is not listed: nothing composed.
    let (app2, _) = holding_lab("rubygems", true, &[("rake", "13.2.1", Some("gem"))]).await;
    let resp = get(&app2, &format!("/proxy/{REG}/info/rake")).await;
    assert_eq!(resp.status(), 503);
    let _ = lab;
}

#[actix_web::test]
async fn a_held_conda_package_gets_its_subdirs_repodata_and_an_empty_one_elsewhere() {
    let facts = serde_json::json!({ "conda": { "name": "_libgcc_mutex", "version": "0.1", "build": "conda_forge", "build_number": 0, "depends": [], "subdir": "linux-64" } });
    let (app, _lab) = holding_lab_extra(
        "conda",
        true,
        &[(
            "_libgcc_mutex",
            "0.1",
            Some("linux-64/_libgcc_mutex-0.1-conda_forge.tar.bz2"),
            facts,
        )],
    )
    .await;
    let resp = get(&app, &format!("/proxy/{REG}/linux-64/repodata.json")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let body = body_of(resp).await;
    assert_eq!(
        body["packages"]["_libgcc_mutex-0.1-conda_forge.tar.bz2"]["build"],
        "conda_forge"
    );
    let resp = get(&app, &format!("/proxy/{REG}/noarch/repodata.json")).await;
    assert_eq!(
        resp.status(),
        200,
        "a client reads every subdir; an empty one is a true answer"
    );
    let body = body_of(resp).await;
    assert!(body["packages"].as_object().unwrap().is_empty(), "{body}");
}

#[actix_web::test]
async fn a_held_nuget_package_gets_its_registration_page_from_the_nuspec_facts() {
    let facts = serde_json::json!({ "nuget": { "id": "Newtonsoft.Json", "version": "13.0.3", "description": "Json.NET", "authors": "James", "tags": null } });
    let (app, _lab) = holding_lab_extra(
        "nuget",
        true,
        &[(
            "newtonsoft.json",
            "13.0.3",
            Some("newtonsoft.json.13.0.3.nupkg"),
            facts,
        )],
    )
    .await;
    let resp = get(
        &app,
        &format!("/proxy/{REG}/nuget/v3/registration5/newtonsoft.json/index.json"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let body = body_of(resp).await;
    assert_eq!(
        body["items"][0]["items"][0]["catalogEntry"]["version"],
        "13.0.3"
    );
    assert_eq!(
        body["items"][0]["items"][0]["catalogEntry"]["id"],
        "Newtonsoft.Json"
    );
}

#[actix_web::test]
async fn a_held_composer_dist_gets_its_p2_document_from_composer_json() {
    let facts =
        serde_json::json!({ "composer": { "name": "vendor/pkg", "require": { "php": ">=8.1" } } });
    let (app, _lab) = holding_lab_extra(
        "composer",
        true,
        &[("vendor/pkg", "1.2.0", Some("dist"), facts)],
    )
    .await;
    let resp = get(&app, &format!("/proxy/{REG}/p2/vendor/pkg.json")).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(header(&resp, "X-BatleHub-Listing"), Some("synthesised"));
    let body = body_of(resp).await;
    let entry = &body["packages"]["vendor/pkg"][0];
    assert_eq!(entry["version"], "1.2.0");
    assert!(
        entry["dist"]["url"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/proxy/{REG}/dist/vendor/pkg/1.2.0")),
        "{entry}"
    );
    let resp = get(&app, &format!("/proxy/{REG}/p2/vendor/pkg~dev.json")).await;
    assert_eq!(
        resp.status(),
        200,
        "Composer asks for ~dev whether or not anything dev is held"
    );
}
