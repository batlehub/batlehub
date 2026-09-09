//! Importing a forge release into the registry that serves it (RFC 0021).
//!
//! The assertion that matters is the last one: after the import, the **gallery**
//! lists the extension and its `.vsix` downloads. Everything before it is
//! scaffolding for that, because an import that publishes a row nothing renders
//! has done nothing an editor can see — which is the whole failure this RFC
//! exists to fix.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use async_trait::async_trait;
use bytes::Bytes;
use serde_json::{json, Value};

use batlehub_config::schema::RegistryMode;
use batlehub_core::entities::{PackageId, PackageMetadata};
use batlehub_core::error::CoreError;
use batlehub_core::ports::{
    FetchedArtifact, ForgeAsset, ForgeRelease, ForgeReleaseSource, RegistryClient,
};
use batlehub_core::services::{ImportPrincipal, ReleaseImportService, ReleaseSelector};
use batlehub_web::handlers::proxy::vsx::import::{SignAfterPublish, VsixCoordinates};

const REG: &str = "local-vsx";
const SOURCE: &str = "gh";

/// A real VSIX: a zip whose `extension/package.json` is what the import reads
/// the coordinate from, exactly as `POST /api/-/publish` does.
fn vsix(publisher: &str, name: &str, version: &str) -> Vec<u8> {
    use std::io::Write as _;
    let manifest = json!({
        "publisher": publisher,
        "name": name,
        "version": version,
        "displayName": "Imported Extension",
        "description": "arrived by release import",
    });
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        writer
            .start_file("extension/package.json", opts)
            .expect("manifest entry");
        writer
            .write_all(&serde_json::to_vec(&manifest).expect("manifest bytes"))
            .expect("manifest write");
        writer.finish().expect("vsix");
    }
    buf.into_inner()
}

/// A forge holding one release, reachable through the client an import fetches
/// with — the arrangement `RegistryClient::releases()` exists for.
struct FakeForge {
    releases: Vec<ForgeRelease>,
    bodies: HashMap<String, Vec<u8>>,
    fetched: Mutex<Vec<String>>,
}

#[async_trait]
impl ForgeReleaseSource for FakeForge {
    async fn list_releases(&self, _repo: &str) -> Result<Vec<ForgeRelease>, CoreError> {
        Ok(self.releases.clone())
    }
    async fn release_by_tag(&self, _repo: &str, tag: &str) -> Result<ForgeRelease, CoreError> {
        self.releases
            .iter()
            .find(|r| r.tag == tag)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(tag.to_owned()))
    }
}

#[async_trait]
impl RegistryClient for FakeForge {
    fn registry_type(&self) -> &str {
        "github"
    }
    fn releases(&self) -> Option<&dyn ForgeReleaseSource> {
        Some(self)
    }
    async fn resolve_metadata(&self, _pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Err(CoreError::NotSupported("not used".into()))
    }
    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let artifact = pkg.artifact.clone().unwrap_or_default();
        self.fetched.lock().unwrap().push(artifact.clone());
        let body = self
            .bodies
            .get(&artifact)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(artifact))?;
        Ok(FetchedArtifact {
            stream: Box::pin(futures::stream::once(async move {
                Ok::<Bytes, CoreError>(Bytes::from(body))
            })),
            cache_control: None,
        })
    }
}

/// An app with one local `openvsx` registry and one import configured into it.
async fn app_with_import() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    app_with_import_history(None).await
}

/// The same, with somewhere for a run's history to go — what the console
/// endpoints read (RFC 0021 §6.5). `None` is the app every other test here
/// builds: an import that records no history still imports.
async fn app_with_import_history(
    history: batlehub_web::handlers::back_office::ops::release_import::ImportHistoryHandle,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let parts = local_registry_app_parts(REG, "openvsx", RegistryMode::Local, None);
    let local_svc = Arc::clone(&parts.local_svc);

    let forge = Arc::new(FakeForge {
        releases: vec![ForgeRelease {
            tag: "v1.0.0".to_owned(),
            draft: false,
            prerelease: false,
            assets: vec![
                ForgeAsset {
                    name: "batlehub-vsx-1.0.0.vsix".to_owned(),
                    artifact: "filename/batlehub-vsx-1.0.0.vsix".to_owned(),
                    size: None,
                },
                ForgeAsset {
                    name: "checksums.txt".to_owned(),
                    artifact: "filename/checksums.txt".to_owned(),
                    size: None,
                },
            ],
        }],
        bodies: HashMap::from([
            (
                "filename/batlehub-vsx-1.0.0.vsix".to_owned(),
                vsix("batlehub", "batlehub-vsx", "1.0.0"),
            ),
            (
                "filename/checksums.txt".to_owned(),
                b"not a package".to_vec(),
            ),
        ]),
        fetched: Mutex::new(vec![]),
    });

    let svc = Arc::new(ReleaseImportService {
        local: Arc::clone(&local_svc),
        client: forge as Arc<dyn RegistryClient>,
        coordinates: Arc::new(VsixCoordinates),
        into: REG.to_owned(),
        from: SOURCE.to_owned(),
        repo: "batleforc/batlehub-vsx".to_owned(),
        assets: vec!["*.vsix".to_owned()],
        select: ReleaseSelector::Latest,
        principal: ImportPrincipal::new("svc-release-import", vec![]).expect("principal"),
        after_publish: Some(Arc::new(SignAfterPublish { local: local_svc })),
    });

    let defaults = ConfigureAppDefaults {
        release_imports: HashMap::from([(REG.to_owned(), vec![svc])]),
        import_history: history,
        ..Default::default()
    };
    build_local_registry_app_with_defaults(parts, batlehub_web::CargoIndexMap::default(), defaults)
        .await
}

/// An app whose runs are remembered.
async fn with_history() -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    app_with_import_history(Some(Arc::new(
        batlehub_adapters::in_memory::release_import::InMemoryImportHistory::new(),
    )))
    .await
}

fn import_uri() -> String {
    format!("/api/v1/admin/registries/{REG}/import")
}

/// The distinction the console exists to draw, and the reason there is a table
/// at all: `server/src/watcher.rs` says "'the import ran and found nothing new'
/// and 'the import has not run' are the two states an operator needs to tell
/// apart", and before this both were an absent row.
#[actix_web::test]
async fn the_last_run_is_absent_until_one_runs_and_named_after() {
    let app = with_history().await;

    // Configured, never run.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/imports")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: Value = read_body_json(resp).await;
    let import = &body["registries"][0]["imports"][0];
    assert_eq!(import["repo"], "batleforc/batlehub-vsx", "{body}");
    assert!(
        import["last_run"].is_null(),
        "a registry that has never imported must say so: {body}"
    );

    // Run it.
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&import_uri())
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);

    // Now it is named, with who asked.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/imports")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    let run = &body["registries"][0]["imports"][0]["last_run"];
    assert!(!run.is_null(), "the run was not recorded: {body}");
    assert_eq!(run["imported"], 1, "{body}");
    assert_eq!(run["repo"], "batleforc/batlehub-vsx", "{body}");
    assert!(
        run["triggered_by"].is_string(),
        "an operator asked, so the row must name them: {body}"
    );
}

/// A run that found nothing is still a run. This is the half a page cannot show
/// if the history only records imports that did something.
#[actix_web::test]
async fn a_run_that_imported_nothing_is_still_recorded() {
    let app = with_history().await;

    for _ in 0..2 {
        let resp = call_service(
            &app,
            TestRequest::post()
                .uri(&import_uri())
                .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
                .set_json(json!({}))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), 200);
    }

    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/api/v1/admin/imports")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    let body: Value = read_body_json(resp).await;
    let run = &body["registries"][0]["imports"][0]["last_run"];
    // The second run is the newest, and it skipped what the first published.
    assert_eq!(run["imported"], 0, "{body}");
    assert_eq!(run["skipped"], 1, "{body}");
}

/// An app with no history configured still imports. A history that is not
/// recorded must never turn a working import into a failed request.
#[actix_web::test]
async fn an_import_works_with_no_history_configured() {
    let app = app_with_import().await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&import_uri())
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let report: Value = read_body_json(resp).await;
    assert_eq!(report["imported"], 1, "{report}");
}

/// The whole point, end to end: a release asset becomes an entry an editor can
/// see and install.
#[actix_web::test]
async fn an_imported_release_is_a_gallery_entry_that_downloads() {
    let app = app_with_import().await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&import_uri())
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let report: Value = read_body_json(resp).await;
    assert_eq!(report["imported"], 1, "{report}");
    assert_eq!(report["errors"], 0, "{report}");

    // The OpenVSX API answers about it — the document an editor resolves
    // through, built from the manifest inside the archive rather than from
    // anything the import was told.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!("/proxy/{REG}/api/batlehub/batlehub-vsx"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "the gallery must know the extension");
    let entry: Value = read_body_json(resp).await;
    assert_eq!(entry["version"], "1.0.0", "{entry}");

    // And the bytes come back through the download route a client uses.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri(&format!(
                "/proxy/{REG}/api/batlehub/batlehub-vsx/1.0.0/file/batlehub.batlehub-vsx-1.0.0.vsix"
            ))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200, "the VSIX must download");
    assert!(
        read_body(resp).await.starts_with(b"PK"),
        "a zip, not an error page"
    );
}

/// Re-running is free, which is what makes an interval safe to set.
#[actix_web::test]
async fn a_second_import_skips_what_the_registry_already_holds() {
    let app = app_with_import().await;
    call_service(
        &app,
        TestRequest::post()
            .uri(&import_uri())
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({}))
            .to_request(),
    )
    .await;

    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&import_uri())
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({}))
            .to_request(),
    )
    .await;
    let report: Value = read_body_json(resp).await;
    assert_eq!(report["imported"], 0, "{report}");
    assert_eq!(report["skipped"], 1, "{report}");
    assert_eq!(report["errors"], 0, "{report}");
}

/// A registry nothing is configured into is a `404`, not an empty `200`: an
/// operator asking has a configuration in mind, and "nothing happened" would
/// look like a working import of nothing.
#[actix_web::test]
async fn a_registry_with_no_import_configured_is_not_found() {
    let app = app_with_import().await;
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/registries/some-other/import")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);

    // Same for a repository this registry does not import.
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&import_uri())
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(json!({ "repo": "someone/else" }))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

/// `cache:warm` is what it takes to *ask*. A reader has no business triggering
/// a publish into a registry, however narrow the effect looks.
#[actix_web::test]
async fn a_plain_reader_may_not_ask_for_an_import() {
    let app = app_with_import().await;
    let resp = call_service(
        &app,
        TestRequest::post()
            .uri(&import_uri())
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_json(json!({}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}
