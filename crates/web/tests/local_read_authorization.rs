//! Per-package **visibility** on the artifact routes that used to read storage
//! directly.
//!
//! The 2026-08-26 survey's findings 6 and 7 are the half of the local-read gap
//! that fails the other way round from findings 4, 5, 9 and 10: these three
//! routes ran the registry rule chain but never called `check_visibility`,
//! because they built a storage key and read `local_svc.storage` themselves
//! rather than going through `get_artifact`. The listing beside each one *did*
//! check, so the symptom was an asymmetry — `maven-metadata.xml` refused a
//! team-visibility coordinate while the jar next to it was served to anyone.
//!
//! [`crates/web/tests/authz_matrix.rs`] covers the same three routes on its
//! visibility axis, but deliberately with `Visibility::Internal`, which refuses
//! anything below `User` and so is answered by the role alone. `Team` is the
//! case that needs a *group*: an authenticated `User` who is not a member is
//! exactly the caller an Internal check waves through, and is the one an
//! operator sets team visibility to stop. That is what these tests hold.
//!
//! Each has its public-visibility control immediately before the assertion, so
//! a test that starts passing because the fixture stopped publishing is visible
//! as a failure rather than as a pass.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, TestRequest};

use batlehub_adapters::in_memory::InMemoryTeamNamespaceStore;
use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{TeamNamespace, Visibility};
use batlehub_core::ports::TeamNamespacePort;

/// A local-mode registry of `kind` with a team-namespace port wired in.
///
/// The port is the fixture: `check_visibility` returns `Ok(())` outright when
/// there is none, so without it every assertion here would pass vacuously.
async fn app_with_namespaces(
    name: &'static str,
    kind: &'static str,
) -> (
    impl TestService,
    Arc<InMemoryTeamNamespaceStore>,
    Arc<batlehub_core::services::LocalRegistryService>,
) {
    app_with_namespaces_and_sbom(name, kind, None, None).await
}

/// [`app_with_namespaces`] with an SBOM service mounted.
async fn app_with_namespaces_and_sbom(
    name: &'static str,
    kind: &'static str,
    sbom_svc: Option<Arc<batlehub_core::services::SbomService>>,
    ownership: Option<Arc<dyn batlehub_core::ports::OwnershipPort>>,
) -> (
    impl TestService,
    Arc<InMemoryTeamNamespaceStore>,
    Arc<batlehub_core::services::LocalRegistryService>,
) {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let mut parts = local_only_app_parts(name, kind, RegistryMode::Local, false);

    let cur = parts.local_svc.clone();
    parts.local_svc = Arc::new(batlehub_core::services::LocalRegistryService {
        backend: cur.backend.clone(),
        storage: cur.storage.clone(),
        hot: cur.hot.clone(),
        quota: cur.quota.clone(),
        ownership: ownership.or_else(|| cur.ownership.clone()),
        team_namespace: Some(ns_store.clone() as Arc<dyn TeamNamespacePort>),
        sbom: cur.sbom.clone(),
        explore_cache: cur.explore_cache.clone(),
        package_repo: cur.package_repo.clone(),
        readme: cur.readme.clone(),
    });

    let local_svc = parts.local_svc.clone();
    let app =
        build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), sbom_svc).await;
    (app, ns_store, local_svc)
}

/// Claim `prefix` for a group `USER_TOKEN` is not in, and mark `package` as
/// team-visible.
///
/// Both halves are needed: `check_team_visibility` refuses when it cannot find
/// an owning namespace *and* when the caller is not in the one it finds, and a
/// test that only set the visibility would be exercising the first.
async fn make_team_only(ns_store: &InMemoryTeamNamespaceStore, registry: &str, package: &str) {
    ns_store
        .claim_namespace(TeamNamespace {
            registry: registry.to_owned(),
            prefix: package.to_owned(),
            group_id: "team-nobody".to_owned(),
            claimed_by: Some("admin".to_owned()),
            separator: '/',
        })
        .await
        .expect("claim namespace");
    ns_store
        .set_visibility(registry, package, Visibility::Team)
        .await
        .expect("set visibility");
}

async fn get_as<S: TestService>(app: &S, uri: &str, token: &str) -> u16 {
    let req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(token)))
        .to_request();
    call_service(app, req).await.status().as_u16()
}

// ── Maven (survey finding 6) ─────────────────────────────────────────────────

const SAMPLE_POM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<project>
  <groupId>com.example</groupId>
  <artifactId>mylib</artifactId>
  <version>1.0.0</version>
  <packaging>jar</packaging>
</project>"#;

/// The jar is stored under `maven_artifact_storage_key`, not the flat artifact
/// key, which is why this route computed its own key and why it ended up
/// bypassing the visibility check that `maven-metadata.xml` runs.
#[actix_web::test]
async fn maven_jar_is_refused_to_a_non_member_of_a_team_visible_coordinate() {
    let (app, ns_store, _local_svc) = app_with_namespaces("local-maven", "maven").await;

    for (uri, payload) in [
        (
            "/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom",
            SAMPLE_POM.as_bytes(),
        ),
        (
            "/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.jar",
            b"fake-jar-bytes".as_slice(),
        ),
    ] {
        let req = TestRequest::put()
            .uri(uri)
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(payload)
            .to_request();
        assert_eq!(call_service(&app, req).await.status(), 201, "{uri}");
    }

    let jar = "/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.jar";
    let metadata = "/proxy/local-maven/maven2/com/example/mylib/maven-metadata.xml";

    // Control: public, so both are served.
    assert_eq!(get_as(&app, jar, USER_TOKEN).await, 200);
    assert_eq!(get_as(&app, metadata, USER_TOKEN).await, 200);

    make_team_only(&ns_store, "local-maven", "com.example:mylib").await;

    // The listing always refused. The jar is the half that did not.
    assert_eq!(get_as(&app, metadata, USER_TOKEN).await, 403);
    assert_eq!(
        get_as(&app, jar, USER_TOKEN).await,
        403,
        "the jar must not outlive the metadata's refusal"
    );
}

// ── NuGet (survey finding 6) ─────────────────────────────────────────────────

fn make_sample_nupkg(id: &str, version: &str) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    let nuspec = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{id}</id>
    <version>{version}</version>
    <description>a package</description>
    <authors>TestAuthor</authors>
  </metadata>
</package>"#
    );
    let mut buf = Vec::new();
    let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
    let opts = SimpleFileOptions::default();
    zip.start_file(format!("{id}.nuspec"), opts).unwrap();
    zip.write_all(nuspec.as_bytes()).unwrap();
    zip.finish().unwrap();
    buf
}

#[actix_web::test]
async fn nupkg_is_refused_to_a_non_member_of_a_team_visible_package() {
    let (app, ns_store, _local_svc) = app_with_namespaces("local-nuget", "nuget").await;

    let boundary = "nugetboundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"package\"; filename=\"package.nupkg\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    ).into_bytes();
    body.extend_from_slice(&make_sample_nupkg("mylib", "1.0.0"));
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let req = TestRequest::put()
        .uri("/proxy/local-nuget/nuget/api/v2/package")
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header((
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        ))
        .set_payload(body)
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 201);

    let nupkg = "/proxy/local-nuget/nuget/v3/flat/mylib/1.0.0/mylib.1.0.0.nupkg";
    let index = "/proxy/local-nuget/nuget/v3/flat/mylib/index.json";

    assert_eq!(get_as(&app, nupkg, USER_TOKEN).await, 200);
    assert_eq!(get_as(&app, index, USER_TOKEN).await, 200);

    make_team_only(&ns_store, "local-nuget", "mylib").await;

    assert_eq!(get_as(&app, index, USER_TOKEN).await, 403);
    assert_eq!(
        get_as(&app, nupkg, USER_TOKEN).await,
        403,
        "the flat index refused this caller; the bytes it points at must too"
    );
}

// ── Terraform provider binary (survey finding 7) ─────────────────────────────

const PROVIDER_MANIFEST: &str = r#"{
  "version": "5.0.0",
  "protocols": ["5.0"],
  "platforms": [
    {"os": "linux", "arch": "amd64", "filename": "terraform-provider-aws_5.0.0_linux_amd64.zip", "shasum": "deadbeef"}
  ]
}"#;

/// Terraform is local-only, so this route has no proxy fall-through to fall back
/// on: the gate it applies is the only gate the provider zip ever gets.
#[actix_web::test]
async fn provider_binary_is_refused_to_a_non_member_of_a_team_visible_provider() {
    let (app, ns_store, _local_svc) = app_with_namespaces("local-tf", "terraform").await;

    let req = TestRequest::post()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/versions")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/json"))
        .set_payload(PROVIDER_MANIFEST)
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 201);

    let req = TestRequest::put()
        .uri("/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(b"fake-zip-bytes".as_slice())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let binary = "/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/artifact/linux/amd64";
    let versions = "/proxy/local-tf/v1/providers/hashicorp/aws/versions";
    let download = "/proxy/local-tf/v1/providers/hashicorp/aws/5.0.0/download/linux/amd64";

    assert_eq!(get_as(&app, binary, USER_TOKEN).await, 200);
    assert_eq!(get_as(&app, versions, USER_TOKEN).await, 200);

    make_team_only(&ns_store, "local-tf", "providers/hashicorp/aws").await;

    // The two documents that *describe* the download always refused. The
    // download itself is the one a caller could reach by constructing its URL.
    assert_eq!(get_as(&app, versions, USER_TOKEN).await, 403);
    assert_eq!(get_as(&app, download, USER_TOKEN).await, 403);
    assert_eq!(
        get_as(&app, binary, USER_TOKEN).await,
        403,
        "refused the listing and the download document, but handed over the zip"
    );
}

// ── Search (survey finding 11) ───────────────────────────────────────────────

/// Publish `name` straight through the service, as `authz_matrix` seeds.
async fn seed(
    local_svc: &batlehub_core::services::LocalRegistryService,
    registry: &str,
    name: &str,
) {
    use batlehub_core::entities::{Identity, Role};
    use bytes::Bytes;
    use sha2::{Digest, Sha256};

    let artifact = Bytes::from_static(b"search-fixture-bytes");
    let checksum = hex::encode(Sha256::digest(&artifact));
    local_svc
        .publish(batlehub_core::services::PublishRequest {
            registry: registry.to_owned(),
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            artifact,
            checksum,
            index_metadata: serde_json::json!({}),
            unlisted: false,
            publisher: Identity {
                user_id: Some("user-1".to_owned()),
                role: Role::User,
                auth_provider: None,
                groups: vec![],
            },
            signature_bytes: None,
            signature_type: None,
        })
        .await
        .expect("fixture must publish");
}

async fn npm_search_names<S: TestService>(app: &S, token: &str) -> Vec<String> {
    let req = TestRequest::get()
        .uri("/proxy/local-npm/-/v1/search?text=acme&size=50")
        .insert_header(("Authorization", bearer(token)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value =
        serde_json::from_slice(&actix_web::test::read_body(resp).await).expect("JSON");
    body["objects"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| o["package"]["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Survey finding 11. `local_hits` read `list_package_names` — a bare
/// `SELECT DISTINCT name FROM local_packages` with no visibility, `unlisted` or
/// identity filter — so a search named every private package in the registry to
/// anyone who asked, including callers the package itself answers `403` to.
///
/// The control is the same request as an admin: if the search returned nothing
/// to anybody the assertion below would pass for the wrong reason.
#[actix_web::test]
async fn search_does_not_name_a_team_visible_package_to_a_non_member() {
    let (app, ns_store, local_svc) = app_with_namespaces("local-npm", "npm").await;
    seed(&local_svc, "local-npm", "acme-public").await;
    seed(&local_svc, "local-npm", "acme-secret").await;

    // Control: both are public, so both are found.
    let before = npm_search_names(&app, USER_TOKEN).await;
    assert!(before.contains(&"acme-secret".to_owned()), "{before:?}");

    make_team_only(&ns_store, "local-npm", "acme-secret").await;

    let names = npm_search_names(&app, USER_TOKEN).await;
    assert!(
        names.contains(&"acme-public".to_owned()),
        "the public package must still be found: {names:?}"
    );
    assert!(
        !names.contains(&"acme-secret".to_owned()),
        "search named a team-visible package to a non-member: {names:?}"
    );

    // …and an admin, who is above visibility, still sees it. Without this the
    // test would pass against a search that returns nothing at all.
    let as_admin = npm_search_names(&app, ADMIN_TOKEN).await;
    assert!(as_admin.contains(&"acme-secret".to_owned()), "{as_admin:?}");
}

/// The other half of finding 11, and the one filtering alone does not close.
///
/// The search cache is keyed `search:{registry}:{limit}:{query}` — no identity
/// in it — and the merged result set used to be what got stored. So the *first*
/// authorised searcher wrote their private hits into a shared entry, and every
/// later caller of the same query was served them from rung 1, filter or no
/// filter. The cache now holds the upstream answer only; the local half is
/// merged per request.
#[actix_web::test]
async fn a_warmed_search_cache_does_not_replay_one_callers_private_hits_to_another() {
    let ns_store = InMemoryTeamNamespaceStore::new();
    let mut parts = local_only_app_parts("hybrid-npm", "npm", RegistryMode::Hybrid, true);
    let cur = parts.local_svc.clone();
    parts.local_svc = Arc::new(batlehub_core::services::LocalRegistryService {
        backend: cur.backend.clone(),
        storage: cur.storage.clone(),
        hot: cur.hot.clone(),
        quota: cur.quota.clone(),
        ownership: cur.ownership.clone(),
        team_namespace: Some(ns_store.clone() as Arc<dyn TeamNamespacePort>),
        sbom: cur.sbom.clone(),
        explore_cache: cur.explore_cache.clone(),
        package_repo: cur.package_repo.clone(),
        readme: cur.readme.clone(),
    });
    let local_svc = parts.local_svc.clone();
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    seed(&local_svc, "hybrid-npm", "acme-secret").await;
    make_team_only(&ns_store, "hybrid-npm", "acme-secret").await;

    let names_for = |token: &'static str| {
        let app = &app;
        async move {
            let req = TestRequest::get()
                .uri("/proxy/hybrid-npm/-/v1/search?text=acme&size=50")
                .insert_header(("Authorization", bearer(token)))
                .to_request();
            let resp = call_service(app, req).await;
            assert_eq!(resp.status(), 200);
            let body: serde_json::Value =
                serde_json::from_slice(&actix_web::test::read_body(resp).await).expect("JSON");
            body["objects"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|o| o["package"]["name"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }
    };

    // The admin may see it, and warms the cache doing so.
    let as_admin = names_for(ADMIN_TOKEN).await;
    assert!(as_admin.contains(&"acme-secret".to_owned()), "{as_admin:?}");

    // The same query, from a caller who may not.
    let as_user = names_for(USER_TOKEN).await;
    assert!(
        !as_user.contains(&"acme-secret".to_owned()),
        "the cache replayed the admin's private hit to a non-member: {as_user:?}"
    );
}

// ── Whole-registry documents (RFC 0015 §4.4) ─────────────────────────────────
//
// The tests above are per-coordinate: a caller names a package and is refused.
// These are the other shape, and the one §4.4 exists for. A whole-registry
// document names *every* package in the registry, so a coordinate the caller
// would be refused must not appear in it — being refused the artifact afterwards
// is not a remedy, because the name was the secret.
//
// Each was built from `list_package_names` and, in conda's case, straight from
// `backend.get_versions` with no visibility check at all. That is survey finding
// 11's shape — a listing assembled from a bare name query — surviving on the
// ecosystems whose listings nobody had revisited.
//
// The public control comes first in each, so a test that starts passing because
// the fixture stopped publishing fails instead.

/// Minimal conda `.tar.bz2`: a bzip2-compressed tar holding `info/index.json`.
fn make_conda_tar_bz2(name: &str, version: &str) -> Vec<u8> {
    use bzip2::write::BzEncoder;
    use bzip2::Compression;
    use std::io::Write as _;

    let index_bytes = serde_json::to_vec(&serde_json::json!({
        "name": name,
        "version": version,
        "build": "0",
        "build_number": 0,
        "depends": [],
        "subdir": "linux-64",
    }))
    .unwrap();

    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(index_bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "info/index.json", index_bytes.as_slice())
            .unwrap();
        builder.finish().unwrap();
    }

    let mut encoder = BzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

/// The names a caller's `repodata.json` describes, across both generations.
async fn repodata_names<S: TestService>(app: &S, token: &str) -> Vec<String> {
    let req = TestRequest::get()
        .uri("/proxy/local-conda/linux-64/repodata.json")
        .insert_header(("Authorization", bearer(token)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200);
    let doc: serde_json::Value =
        serde_json::from_slice(&actix_web::test::read_body(resp).await).expect("JSON");

    let mut names: Vec<String> = ["packages", "packages.conda"]
        .iter()
        .filter_map(|k| doc[*k].as_object())
        .flat_map(|m| m.values())
        .filter_map(|entry| entry["name"].as_str().map(str::to_owned))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `repodata.json` is the channel's inventory, and it named private packages.
///
/// This document was built from `backend.get_versions` directly — no
/// `check_visibility`, no grant filter — so a team-visible conda package was
/// listed to every caller who fetched the channel, including the ones the same
/// registry answers `403` to on the package itself. conda fetches this on every
/// `conda install`, so the disclosure was not a corner of the API: it was the
/// first request every client makes.
#[actix_web::test]
async fn conda_repodata_does_not_name_a_team_visible_package_to_a_non_member() {
    let (app, ns_store, _local_svc) = app_with_namespaces("local-conda", "conda").await;

    for name in ["openpkg", "secretpkg"] {
        let req = TestRequest::post()
            .uri("/proxy/local-conda/linux-64/")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_payload(make_conda_tar_bz2(name, "1.0.0"))
            .to_request();
        assert!(
            call_service(&app, req).await.status().is_success(),
            "publishing {name} must succeed, or the assertions below pass vacuously"
        );
    }

    // Control: both public, both named.
    let before = repodata_names(&app, USER_TOKEN).await;
    assert!(
        before.contains(&"secretpkg".to_owned()),
        "fixture published nothing: {before:?}"
    );

    make_team_only(&ns_store, "local-conda", "secretpkg").await;

    let after = repodata_names(&app, USER_TOKEN).await;
    assert!(
        !after.contains(&"secretpkg".to_owned()),
        "the channel index named a package this caller is refused: {after:?}"
    );
    assert!(
        after.contains(&"openpkg".to_owned()),
        "filtering must remove one package, not blank the channel: {after:?}"
    );

    // And a member still sees it — otherwise the assertion above would pass on a
    // channel that had simply stopped listing anything.
    let as_admin = repodata_names(&app, ADMIN_TOKEN).await;
    assert!(as_admin.contains(&"secretpkg".to_owned()), "{as_admin:?}");
}

/// `available-packages` asserts it is the *complete* contents of the repository.
///
/// Which is what makes an unfiltered one worse than an ordinary listing leak:
/// Composer treats the list as authoritative and will not request a package
/// absent from it, so every name in it is both a disclosure and a promise. The
/// handler already gated *whether* the caller gets the document; nothing decided
/// what went in it.
#[actix_web::test]
async fn composer_available_packages_omits_a_team_visible_package() {
    let (app, ns_store, _local_svc) = app_with_namespaces("local-composer", "composer").await;

    for name in ["acme/open", "acme/secret"] {
        let req = TestRequest::post()
            .uri("/proxy/local-composer/api/upload")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_payload(make_composer_zip(name, "1.0.0"))
            .to_request();
        assert!(
            call_service(&app, req).await.status().is_success(),
            "publishing {name} must succeed, or the assertions below pass vacuously"
        );
    }

    let available = |token: &'static str| {
        let app = &app;
        async move {
            let req = TestRequest::get()
                .uri("/proxy/local-composer/packages.json")
                .insert_header(("Authorization", bearer(token)))
                .to_request();
            let resp = call_service(app, req).await;
            assert_eq!(resp.status(), 200);
            let doc: serde_json::Value =
                serde_json::from_slice(&actix_web::test::read_body(resp).await).expect("JSON");
            doc["available-packages"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        }
    };

    let before = available(USER_TOKEN).await;
    assert!(
        before.contains(&"acme/secret".to_owned()),
        "fixture published nothing: {before:?}"
    );

    make_team_only(&ns_store, "local-composer", "acme/secret").await;

    let after = available(USER_TOKEN).await;
    assert!(
        !after.contains(&"acme/secret".to_owned()),
        "`available-packages` named a package this caller is refused: {after:?}"
    );
    assert!(
        after.contains(&"acme/open".to_owned()),
        "filtering must remove one package, not empty the repository: {after:?}"
    );
}

// ── RFC 0015 §4.4 — the namespace tier reaches a whole-registry document ──────
//
// `available-packages` is the sharpest of the six wired documents, and §13.5 says
// why: it *asserts* it is the complete contents of the repository, and Composer
// will not request a package absent from it. So every name in it is
// simultaneously a disclosure and a promise, and the filter has to be right in
// both directions — a missing name breaks resolution, an extra one enumerates a
// private inventory.
//
// It is also the only one of the six with no per-package grant check behind it:
// the other five call `load_visible_versions`, which authorizes, so a filter that
// listed too much was caught downstream there and not here.
//
// The filter used to resolve the registry node **alone**, which made
// `[[registries.namespaces]]` invisible to all six documents in both directions.

/// A local Composer app with `packages` published, and `grants` installed
/// *afterwards*.
///
/// The publishes run under the fixture's own permissive hierarchy because
/// publishing needs `releases:publish`; the hierarchy under test is then swapped
/// into the shared `HotConfig`, which is also what proves the document reflects
/// the live config rather than one captured at construction.
async fn composer_app_with(
    packages: &[&str],
    grants: batlehub_core::entities::RegistryGrants,
) -> impl TestService {
    let parts = local_registry_app_parts("local-composer", "composer", RegistryMode::Local, None);
    let hot = parts.proxy_svc.hot.clone();
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    for name in packages {
        let req = TestRequest::post()
            .uri("/proxy/local-composer/api/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_composer_zip(name, "1.0.0"))
            .to_request();
        assert_eq!(
            call_service(&app, req).await.status(),
            200,
            "{name} must publish, or the assertions below are about an empty registry"
        );
    }

    hot.write().await.grants = [("local-composer".to_owned(), Arc::new(grants))].into();
    app
}

/// The names in `available-packages`, as an ordinary user.
async fn available_packages<S: TestService>(app: &S) -> Vec<String> {
    let req = TestRequest::get()
        .uri("/proxy/local-composer/packages.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "the document itself must be served");
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    body.get("available-packages")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A registry node granting `registry` to `role:user`, over `namespaces` in
/// config order. `None` grants is the seal (`grants = {}`).
fn hierarchy(
    registry: &[batlehub_core::entities::Action],
    namespaces: &[(&str, Option<&[batlehub_core::entities::Action]>)],
) -> batlehub_core::entities::RegistryGrants {
    use batlehub_core::entities::{
        GrantMap, Node, RegistryGrants, RegistryKind, Role, SubjectMatcher, Tier,
    };
    RegistryGrants {
        kind: RegistryKind::Composer,
        registry: Node::new(
            Tier::Registry,
            "registry:local-composer",
            Some(GrantMap::new().grant(SubjectMatcher::Role(Role::User), registry.to_vec())),
        ),
        namespaces: namespaces
            .iter()
            .map(|(prefix, grants)| {
                (
                    (*prefix).to_owned(),
                    Node::new(
                        Tier::Namespace,
                        format!("namespace:{prefix}"),
                        Some(match grants {
                            None => GrantMap::sealed(),
                            Some(actions) => GrantMap::new()
                                .grant(SubjectMatcher::Role(Role::User), actions.to_vec()),
                        }),
                    ),
                )
            })
            .collect(),
    }
}

/// A namespace seal withholds from the document what the registry granted.
///
/// The direction that is a **disclosure** rather than a breakage: the registry
/// grants the read, so a filter that resolved only the registry node answered
/// "everything" and named a sealed namespace's packages to a caller the seal
/// excludes. §6.3 requires the listing and the download gate to agree, so the
/// control asserts the same coordinate is refused on the per-package route —
/// which is what makes the document the half that was wrong.
#[actix_web::test]
async fn a_namespace_seal_is_honoured_by_available_packages() {
    use batlehub_core::entities::Action;

    let app = composer_app_with(
        &["acme/lib", "other/lib"],
        hierarchy(
            &[Action::ReleasesRead, Action::ReleasesList],
            &[("acme", None)],
        ),
    )
    .await;

    let names = available_packages(&app).await;
    assert!(
        !names.contains(&"acme/lib".to_owned()),
        "a sealed namespace must not be enumerated to a caller it excludes; got {names:?}"
    );
    assert!(
        names.contains(&"other/lib".to_owned()),
        "the control: a seal is a namespace seal, not a refusal of everything: {names:?}"
    );

    let req = TestRequest::get()
        .uri("/proxy/local-composer/p2/acme/lib.json")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let status = call_service(&app, req).await.status();
    assert!(
        status.is_client_error(),
        "the per-package route already refused this coordinate; the document was \
         the half that disagreed, and got {status}"
    );
}

/// A namespace grant **below** a seal reaches the document, and only the
/// packages under it.
///
/// The widening direction, end to end. §4.3: *"A seal stops inheritance, it does
/// not disable the nodes beneath it"* — so re-opening one package's namespace
/// inside a sealed vendor is a supported configuration, and it is the shape that
/// proves the filter consults the namespace tier per package rather than
/// answering once from the registry node. Before that, `acme/lib` was absent
/// from the inventory while `/p2/acme/lib.json` served it — and Composer, which
/// treats this list as complete, would have reported a package it was entitled
/// to as not existing.
#[actix_web::test]
async fn a_namespace_grant_below_a_seal_reaches_available_packages() {
    use batlehub_core::entities::Action;

    let app = composer_app_with(
        &["acme/lib", "acme/other", "other/lib"],
        hierarchy(
            &[Action::ReleasesRead, Action::ReleasesList],
            // Config order is path order, so the seal comes first and the
            // narrower block below it re-opens exactly what it names.
            // **Both verbs.** The seal removes everything the registry granted,
            // `releases:list` included, and a listing's gate is `releases:list`
            // now (§4.2) — so re-opening only the read verb serves the name in
            // the inventory and then refuses the document it points at. §10
            // rule 4 gives translated configs both together for exactly this
            // reason; a hand-written grants block has to say so.
            &[
                ("acme", None),
                (
                    "acme/lib",
                    Some(&[Action::ReleasesRead, Action::ReleasesList]),
                ),
            ],
        ),
    )
    .await;

    let names = available_packages(&app).await;
    assert!(
        names.contains(&"acme/lib".to_owned()),
        "a namespace grant below the seal must reach its own packages; got {names:?}"
    );
    assert!(
        !names.contains(&"acme/other".to_owned()),
        "and only its own — the seal still covers the rest of the vendor: {names:?}"
    );
    assert!(
        names.contains(&"other/lib".to_owned()),
        "the control: the registry grant still reaches everything outside the seal: {names:?}"
    );

    // The listing and the gate agree, in both directions, on the two coordinates
    // the seal separates.
    for (package, served) in [("acme/lib", true), ("acme/other", false)] {
        let req = TestRequest::get()
            .uri(&format!("/proxy/local-composer/p2/{package}.json"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request();
        let status = call_service(&app, req).await.status();
        assert_eq!(
            status.is_success(),
            served,
            "{package}: the document and the per-package route must agree, got {status}"
        );
    }
}

// ── SBOM ─────────────────────────────────────────────────────────────────────

/// The per-artifact SBOM checked only that the caller was not anonymous, so a
/// team-visible package's inventory went to any authenticated user — and its
/// `200` confirmed a name the download path answers `403` for.
#[actix_web::test]
async fn sbom_is_hidden_from_a_non_member_of_a_team_visible_package() {
    use batlehub_core::{
        entities::{ArtifactSbom, SbomFormat, SbomSource},
        ports::SbomRepository,
    };

    let repo = batlehub_adapters::in_memory::InMemorySbomRepository::new();
    repo.upsert_sbom(ArtifactSbom {
        id: uuid::Uuid::new_v4(),
        artifact_key: "local-maven/com.example:mylib/1.0.0".to_owned(),
        registry: "local-maven".to_owned(),
        package_name: "com.example:mylib".to_owned(),
        version: "1.0.0".to_owned(),
        format: SbomFormat::Spdx,
        spec_version: "SPDX-2.3".to_owned(),
        document: serde_json::json!({"spdxVersion": "SPDX-2.3"}),
        source: SbomSource::Generated,
        created_at: chrono::Utc::now(),
        license: None,
    })
    .await
    .unwrap();
    let sbom_svc = Arc::new(batlehub_core::services::SbomService::new(repo, None, None));
    let (app, ns_store, _local_svc) =
        app_with_namespaces_and_sbom("local-maven", "maven", Some(sbom_svc), None).await;

    let req = TestRequest::put()
        .uri("/proxy/local-maven/maven2/com/example/mylib/1.0.0/mylib-1.0.0.pom")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(SAMPLE_POM.as_bytes())
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 201);

    let sbom = "/api/v1/sbom/local-maven/com.example:mylib/1.0.0";
    // Control: public, so it is served.
    assert_eq!(get_as(&app, sbom, USER_TOKEN).await, 200);

    make_team_only(&ns_store, "local-maven", "com.example:mylib").await;

    assert_eq!(
        get_as(&app, sbom, USER_TOKEN).await,
        404,
        "refused as absent, so the status does not confirm the package"
    );
    assert_eq!(get_as(&app, sbom, ADMIN_TOKEN).await, 200);
}

// ── Cargo owners ─────────────────────────────────────────────────────────────

/// `cargo owner --list` took no identity, so it named a team-visible crate's
/// owners — and confirmed the crate exists — to a caller its index refuses.
#[actix_web::test]
async fn cargo_owners_are_hidden_from_a_non_member_of_a_team_visible_crate() {
    let ownership = batlehub_adapters::in_memory::InMemoryOwnershipStore::new();
    let (app, ns_store, _local_svc) = app_with_namespaces_and_sbom(
        "local-cargo",
        "cargo",
        None,
        Some(ownership as Arc<dyn batlehub_core::ports::OwnershipPort>),
    )
    .await;

    let req = TestRequest::put()
        .uri("/proxy/local-cargo/api/v1/crates/new")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_publish_payload("secret-crate", "0.1.0"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);

    let owners = "/proxy/local-cargo/api/v1/crates/secret-crate/owners";
    // Control: public, so it is served.
    assert_eq!(get_as(&app, owners, USER_TOKEN).await, 200);

    make_team_only(&ns_store, "local-cargo", "secret-crate").await;

    assert_eq!(
        get_as(&app, owners, USER_TOKEN).await,
        404,
        "answered as the missing crate's 404"
    );
    // The write route tells the hidden crate apart no better: an owned crate
    // and an unknown one must not answer differently.
    let req = TestRequest::put()
        .uri(owners)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(serde_json::json!({ "users": ["someone"] }))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}
