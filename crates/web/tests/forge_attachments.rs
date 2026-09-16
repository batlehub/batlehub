//! Forgejo's attachment endpoint: `…/attachments/{uuid}` (RFC 0019 §4.2).
//!
//! Forgejo addresses a release asset by uuid on a path that names no
//! repository, and a client builds that URL itself from its own configured
//! forge root rather than following the `browser_download_url` in the release
//! document. `mise`'s `forgejo:` backend does only that — which is how the
//! closed-world suite found this: the checksum sibling, fetched by browser URL,
//! came through the proxy while the binary beside it went straight to
//! codeberg.org.
//!
//! What this file pins is the mapping that makes the route answerable: the uuid
//! is remembered from the release document this instance rewrote, and it
//! resolves to the *same coordinate* the by-name download route builds — one
//! artifact, one storage key, one rule chain, one audit row. A uuid nothing has
//! been remembered for is a `404`, so the route is not an opaque relay for the
//! forge's whole attachment space.
//!
//! See `tests/common/mod.rs` for the shared app-factory infrastructure.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::call_service;
use async_trait::async_trait;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{DocumentKind, FetchedArtifact, RegistryClient, VersionDocument},
};

const REG: &str = "local-fj";
const REPO: &str = "acme/widget";
const TAG: &str = "v1.0.0";
const FILE: &str = "app.bin";
const UUID: &str = "26305e39-a8d3-43ae-b846-f1958634ada6";
const ASSET: &[u8] = b"the asset bytes";

/// A forge that answers the two release documents and the one asset in them.
struct Fj;

impl Fj {
    fn release() -> serde_json::Value {
        serde_json::json!({
            "id": 1,
            "tag_name": TAG,
            "assets": [{
                "id": 9,
                "name": FILE,
                "uuid": UUID,
                "size": ASSET.len(),
                // The forge's own URL, which the proxy rewrites — and which
                // the client this exists for never reads.
                "browser_download_url":
                    format!("https://forge.invalid/{REPO}/releases/download/{TAG}/{FILE}"),
            }],
        })
    }
}

#[async_trait]
impl RegistryClient for Fj {
    fn registry_type(&self) -> &str {
        "forgejo"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata::minimal(
            pkg.clone(),
            serde_json::Value::Null,
        ))
    }

    /// The release listing is a *version document*, not an artifact: it goes
    /// through the listing pipeline, and the handler rewrites what comes back.
    async fn fetch_version_document(
        &self,
        _package: &str,
        kind: DocumentKind,
    ) -> Result<VersionDocument, CoreError> {
        if kind == DocumentKind::Versions {
            Ok(VersionDocument::json(serde_json::json!([Fj::release()])))
        } else {
            Err(CoreError::NotFound(format!("no {kind:?} document")))
        }
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let body: Vec<u8> = match (pkg.artifact.as_deref(), pkg.version.as_str()) {
            // The release listing and the release by tag: documents, not assets.
            (None, "releases") => serde_json::to_vec(&serde_json::json!([Fj::release()])).unwrap(),
            (None, TAG) => serde_json::to_vec(&Fj::release()).unwrap(),
            (Some(sel), TAG) if sel == format!("filename/{FILE}") => ASSET.to_vec(),
            _ => {
                return Err(CoreError::NotFound(format!(
                    "no {:?} at {}@{}",
                    pkg.artifact, pkg.name, pkg.version
                )))
            }
        };
        Ok(FetchedArtifact {
            stream: Box::pin(futures::stream::once(async move {
                Ok(bytes::Bytes::from(body))
            })),
            cache_control: None,
        })
    }
}

async fn app() -> impl TestService {
    let parts = local_registry_app_parts(REG, "forgejo", RegistryMode::Proxy, None);
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        hot.registries
            .insert(REG.to_owned(), Arc::new(Fj) as Arc<dyn RegistryClient>);
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

async fn get<S: TestService>(app: &S, uri: &str) -> actix_web::dev::ServiceResponse {
    call_service(app, admin_get(uri)).await
}

fn attachment_uri(uuid: &str) -> String {
    format!("/proxy/{REG}/attachments/{uuid}")
}

#[actix_web::test]
async fn an_asset_read_by_uuid_is_the_asset_the_release_document_named() {
    let app = app().await;

    // Nothing is known about the uuid until the document that carries it has
    // been served.
    assert_eq!(get(&app, &attachment_uri(UUID)).await.status(), 404);

    assert_eq!(
        get(&app, &format!("/proxy/{REG}/{REPO}/releases/tags/{TAG}"))
            .await
            .status(),
        200
    );

    let resp = get(&app, &attachment_uri(UUID)).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(actix_web::test::read_body(resp).await, ASSET);
}

/// The listing teaches it too: a client that resolves a version from
/// `/releases` and installs from it never asks for the release by tag.
#[actix_web::test]
async fn the_release_listing_teaches_the_mapping_as_well() {
    let app = app().await;

    assert_eq!(
        get(&app, &format!("/proxy/{REG}/{REPO}/releases"))
            .await
            .status(),
        200
    );

    assert_eq!(get(&app, &attachment_uri(UUID)).await.status(), 200);
}

/// The whole point of resolving the uuid to a coordinate rather than relaying
/// it: by uuid and by name are one artifact, under one key.
#[actix_web::test]
async fn by_uuid_and_by_name_are_the_same_stored_artifact() {
    let app = app().await;
    get(&app, &format!("/proxy/{REG}/{REPO}/releases/tags/{TAG}")).await;

    let by_name = get(
        &app,
        &format!("/proxy/{REG}/{REPO}/releases/download/{TAG}/{FILE}"),
    )
    .await;
    assert_eq!(by_name.status(), 200);
    let by_uuid = get(&app, &attachment_uri(UUID)).await;
    assert_eq!(by_uuid.status(), 200);

    let key = |resp: &actix_web::dev::ServiceResponse| {
        resp.headers()
            .get("X-BatleHub-Storage-Key")
            .map(|v| v.to_str().unwrap().to_owned())
    };
    assert_eq!(
        key(&by_uuid),
        key(&by_name),
        "the uuid resolved to a different coordinate than the name did"
    );
    assert!(key(&by_uuid).is_some(), "no storage key was reported");
}

/// An unknown uuid is refused, and the refusal says what would make it
/// answerable — a relay would have gone upstream with it instead.
#[actix_web::test]
async fn an_unremembered_uuid_is_refused_and_says_why() {
    let app = app().await;

    let resp = get(
        &app,
        &attachment_uri("00000000-0000-0000-0000-000000000000"),
    )
    .await;
    assert_eq!(resp.status(), 404);
    let body = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("release document"), "{body}");
}

/// The path is Forgejo's, and only a Forgejo registry answers it.
#[actix_web::test]
async fn a_github_registry_does_not_serve_the_forgejo_attachment_path() {
    let app = proxy_registry_app("local-gh", "github").await;

    let resp = get(&app, &format!("/proxy/local-gh/attachments/{UUID}")).await;
    assert_eq!(resp.status(), 404);
    let body = String::from_utf8(actix_web::test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("not a forgejo registry"), "{body}");
}
