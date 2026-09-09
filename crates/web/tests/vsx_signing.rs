//! RFC 0020 — the VSIX signature asset: signed by the registry's key for what
//! it publishes, relayed from the upstream for what it proxies, advertised
//! only when servable.

mod common;
#[allow(unused_imports)]
use common::*;

use std::io::{Cursor, Write};
use std::sync::Arc;

use actix_web::test::{call_service, read_body, read_body_json, TestRequest};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Utc;
use futures::stream;
use serde_json::{json, Value};

use batlehub_config::schema::RegistryMode;
use batlehub_core::{
    entities::{PackageId, PackageMetadata},
    error::CoreError,
    ports::{FetchedArtifact, RegistryClient},
    services::{
        signature::{public_key_from_pem_or_hex, verify_ed25519, VsxSigningKey},
        vsx_signature::{
            manifest_matches, read_signature_archive, PUBLIC_KEY_ARTIFACT, SIGNATURE_ARTIFACT,
        },
    },
};

const REG: &str = "vsx";
const SIGNATURE: &str = "Microsoft.VisualStudio.Services.VsixSignature";
const PUBLIC_KEY: &str = "Microsoft.VisualStudio.Services.PublicKey";

/// A VSIX-shaped zip the publish path can read a manifest out of.
fn vsix(publisher: &str, name: &str, version: &str) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("extension.vsixmanifest", opts).unwrap();
    zip.write_all(b"<PackageManifest/>").unwrap();
    zip.start_file("extension/package.json", opts).unwrap();
    zip.write_all(
        json!({
            "name": name, "publisher": publisher, "version": version,
            "displayName": "Signed Thing", "engines": { "vscode": "^1.80.0" }
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap();
    out.into_inner()
}

async fn app_with_key(
    mode: RegistryMode,
    key: Option<VsxSigningKey>,
) -> (
    impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    LocalRegistryAppParts,
) {
    let parts = local_registry_app_parts(REG, "openvsx", mode, None);
    if let Some(key) = key {
        parts
            .local_svc
            .hot
            .write()
            .await
            .vsx_signing
            .insert(REG.to_owned(), Arc::new(key));
    }
    let keep = LocalRegistryAppParts {
        proxy_svc: parts.proxy_svc.clone(),
        admin_svc: parts.admin_svc.clone(),
        token_repo: parts.token_repo.clone(),
        access_config: parts.access_config.clone(),
        registry_map: parts.registry_map.clone(),
        local_svc: parts.local_svc.clone(),
        mode_map: parts.mode_map.clone(),
    };
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    (app, keep)
}

async fn publish(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    id: &str,
    version: &str,
    bytes: &[u8],
) {
    let req = TestRequest::put()
        .uri(&format!("/proxy/{REG}/{id}/{version}/vsix"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", "application/octet-stream"))
        .set_payload(bytes.to_vec())
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "publish");
}

async fn gallery_files(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    id: &str,
) -> Vec<String> {
    let req = TestRequest::post()
        .uri(&format!("/proxy/{REG}/vscode/gallery/extensionquery"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_json(json!({
            "filters": [{ "criteria": [{ "filterType": 7, "value": id }], "pageNumber": 1, "pageSize": 10 }],
            "flags": 950
        }))
        .to_request();
    let resp = call_service(app, req).await;
    assert_eq!(resp.status(), 200, "extensionquery");
    let body: Value = read_body_json(resp).await;
    let exts = body["results"][0]["extensions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(exts.len(), 1, "one entry for {id}: {body}");
    exts[0]["versions"][0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["assetType"].as_str().unwrap().to_owned())
        .collect()
}

async fn asset(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    id: &str,
    version: &str,
    asset_type: &str,
) -> (u16, Bytes) {
    let (publisher, name) = id.split_once('.').unwrap();
    let req = TestRequest::get()
        .uri(&format!(
            "/proxy/{REG}/vscode/asset/{publisher}/{name}/{version}/{asset_type}"
        ))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    let status = resp.status().as_u16();
    (status, read_body(resp).await)
}

// ── local: the registry's own signature ─────────────────────────────────────

#[actix_web::test]
async fn a_published_version_is_signed_advertised_and_verifiable() {
    let key = VsxSigningKey::from_seed([7u8; 32], Some("k1"));
    let public_hex = key.public_key_hex();
    let (app, _) = app_with_key(RegistryMode::Local, Some(key)).await;
    let bytes = vsix("acme", "tool", "1.0.0");
    publish(&app, "acme.tool", "1.0.0", &bytes).await;

    let files = gallery_files(&app, "acme.tool").await;
    assert!(files.contains(&SIGNATURE.to_owned()), "{files:?}");
    assert!(files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");

    let (status, archive) = asset(&app, "acme.tool", "1.0.0", SIGNATURE).await;
    assert_eq!(status, 200);
    let archive = read_signature_archive(&archive).expect("Open VSX's three entries");
    assert!(archive.has_p7s, "the empty .p7s the editor checks for");
    assert!(verify_ed25519(
        std::slice::from_ref(&public_hex),
        &archive.signature,
        &bytes
    ));
    assert!(manifest_matches(&archive.manifest, &bytes).unwrap());

    let (status, pem) = asset(&app, "acme.tool", "1.0.0", PUBLIC_KEY).await;
    assert_eq!(status, 200);
    let pem = String::from_utf8(pem.to_vec()).unwrap();
    assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----"), "{pem}");
    assert_eq!(
        hex::encode(public_key_from_pem_or_hex(&pem).unwrap()),
        public_hex
    );

    // The Open VSX document names both, and the key route serves the same key.
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/api/acme/tool/1.0.0"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert!(
        body["files"]["signature"]
            .as_str()
            .unwrap()
            .ends_with(SIGNATURE),
        "{body}"
    );
    assert!(
        body["files"]["publicKey"]
            .as_str()
            .unwrap()
            .ends_with(PUBLIC_KEY),
        "{body}"
    );
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/api/-/public-key/k1"))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200, "anonymous: a public key is public");
    assert_eq!(
        String::from_utf8(read_body(resp).await.to_vec()).unwrap(),
        pem
    );
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/api/-/public-key/other"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

#[actix_web::test]
async fn without_a_key_nothing_is_advertised_and_the_assets_are_404() {
    let (app, _) = app_with_key(RegistryMode::Local, None).await;
    publish(&app, "acme.tool", "1.0.0", &vsix("acme", "tool", "1.0.0")).await;
    let files = gallery_files(&app, "acme.tool").await;
    assert!(!files.contains(&SIGNATURE.to_owned()), "{files:?}");
    assert!(!files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");
    assert_eq!(asset(&app, "acme.tool", "1.0.0", SIGNATURE).await.0, 404);
    assert_eq!(asset(&app, "acme.tool", "1.0.0", PUBLIC_KEY).await.0, 404);
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/api/acme/tool/1.0.0"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert!(body["files"].get("signature").is_none(), "{body}");
}

/// A key configured after the publish signs on first request; a rotated key
/// re-signs, and the served key is always the one that verifies the archive.
#[actix_web::test]
async fn a_later_or_rotated_key_signs_on_first_request() {
    let (app, parts) = app_with_key(RegistryMode::Local, None).await;
    let bytes = vsix("acme", "tool", "2.0.0");
    publish(&app, "acme.tool", "2.0.0", &bytes).await;
    assert_eq!(asset(&app, "acme.tool", "2.0.0", SIGNATURE).await.0, 404);

    let first = VsxSigningKey::from_seed([1u8; 32], Some("first"));
    let first_hex = first.public_key_hex();
    parts
        .local_svc
        .hot
        .write()
        .await
        .vsx_signing
        .insert(REG.to_owned(), Arc::new(first));
    assert!(gallery_files(&app, "acme.tool")
        .await
        .contains(&SIGNATURE.to_owned()));
    let (status, a1) = asset(&app, "acme.tool", "2.0.0", SIGNATURE).await;
    assert_eq!(status, 200);
    let a1 = read_signature_archive(&a1).unwrap();
    assert!(verify_ed25519(
        std::slice::from_ref(&first_hex),
        &a1.signature,
        &bytes
    ));

    let second = VsxSigningKey::from_seed([2u8; 32], Some("second"));
    let second_hex = second.public_key_hex();
    parts
        .local_svc
        .hot
        .write()
        .await
        .vsx_signing
        .insert(REG.to_owned(), Arc::new(second));
    let (_, a2) = asset(&app, "acme.tool", "2.0.0", SIGNATURE).await;
    let a2 = read_signature_archive(&a2).unwrap();
    assert!(
        !verify_ed25519(&[first_hex], &a2.signature, &bytes),
        "re-signed"
    );
    assert!(verify_ed25519(
        std::slice::from_ref(&second_hex),
        &a2.signature,
        &bytes
    ));
    let (_, pem) = asset(&app, "acme.tool", "2.0.0", PUBLIC_KEY).await;
    let served = public_key_from_pem_or_hex(std::str::from_utf8(&pem).unwrap()).unwrap();
    assert_eq!(
        hex::encode(served),
        second_hex,
        "the served key verifies the served archive"
    );
    // The old id is gone with its key.
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/api/-/public-key/first"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 404);
}

// ── relay: the upstream's signature, never re-signed ────────────────────────

/// An upstream that signs: metadata says so, and the signature archive and
/// the public key are artifacts of the version addressed by selector.
struct SigningUpstream {
    public_key: bool,
}

#[async_trait]
impl RegistryClient for SigningUpstream {
    fn registry_type(&self) -> &str {
        "openvsx"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: PackageId {
                version: "9.9.9".to_owned(),
                ..pkg.clone()
            },
            published_at: Some(Utc::now()),
            download_url: None,
            checksum: None,
            is_signed: Some(true),
            extra: json!({
                "resolved_version": "9.9.9",
                "display_name": "Upstream Thing",
                "signature_url": "https://upstream.example/sig",
                "public_key_url": self.public_key.then_some("https://upstream.example/key"),
            }),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        let body = match pkg.artifact.as_deref() {
            Some(SIGNATURE_ARTIFACT) => "UPSTREAM-SIGZIP".to_owned(),
            Some(PUBLIC_KEY_ARTIFACT) if self.public_key => "UPSTREAM-PEM".to_owned(),
            Some(PUBLIC_KEY_ARTIFACT) => {
                return Err(CoreError::NotFound("no key upstream".into()));
            }
            _ => "PK\x03\x04upstream-vsix".to_owned(),
        };
        let bytes = Bytes::from(body);
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(bytes) })),
            cache_control: None,
        })
    }
}

async fn relay_app(
    public_key: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let parts = local_registry_app_parts(REG, "openvsx", RegistryMode::Hybrid, None);
    parts.local_svc.hot.write().await.registries.insert(
        REG.to_owned(),
        Arc::new(SigningUpstream { public_key }) as Arc<dyn RegistryClient>,
    );
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

#[actix_web::test]
async fn a_signed_upstream_version_is_relayed_with_its_key() {
    let app = relay_app(true).await;
    let files = gallery_files(&app, "up.stream").await;
    assert!(files.contains(&SIGNATURE.to_owned()), "{files:?}");
    assert!(files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");
    let (status, body) = asset(&app, "up.stream", "9.9.9", SIGNATURE).await;
    assert_eq!(status, 200);
    assert_eq!(
        &body[..],
        b"UPSTREAM-SIGZIP",
        "the upstream's bytes, untouched"
    );
    let (status, body) = asset(&app, "up.stream", "9.9.9", PUBLIC_KEY).await;
    assert_eq!(status, 200);
    assert_eq!(&body[..], b"UPSTREAM-PEM");
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/api/up/stream/9.9.9"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let body: Value = read_body_json(call_service(&app, req).await).await;
    assert!(body["files"]["signature"].is_string(), "{body}");
    assert!(body["files"]["publicKey"].is_string(), "{body}");
}

#[actix_web::test]
async fn an_upstream_without_a_key_relays_the_signature_alone() {
    let app = relay_app(false).await;
    let files = gallery_files(&app, "up.stream").await;
    assert!(files.contains(&SIGNATURE.to_owned()), "{files:?}");
    assert!(!files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");
    assert_eq!(asset(&app, "up.stream", "9.9.9", SIGNATURE).await.0, 200);
    assert_eq!(asset(&app, "up.stream", "9.9.9", PUBLIC_KEY).await.0, 404);
}

/// An upstream that does not sign: what both real adapters answer for a
/// signature selector on such a version (`NotFound`).
struct UnsignedUpstream;

#[async_trait]
impl RegistryClient for UnsignedUpstream {
    fn registry_type(&self) -> &str {
        "openvsx"
    }

    async fn resolve_metadata(&self, pkg: &PackageId) -> Result<PackageMetadata, CoreError> {
        Ok(PackageMetadata {
            id: pkg.clone(),
            published_at: Some(Utc::now()),
            download_url: None,
            checksum: None,
            is_signed: Some(false),
            extra: json!({ "resolved_version": pkg.version }),
            cache_control: None,
        })
    }

    async fn fetch_artifact(&self, pkg: &PackageId) -> Result<FetchedArtifact, CoreError> {
        if pkg.artifact.as_deref() == Some(SIGNATURE_ARTIFACT) {
            return Err(CoreError::NotFound("not signed upstream".into()));
        }
        let bytes = Bytes::from_static(b"PK\x03\x04upstream-vsix");
        Ok(FetchedArtifact {
            stream: Box::pin(stream::once(async move { Ok::<Bytes, CoreError>(bytes) })),
            cache_control: None,
        })
    }
}

#[actix_web::test]
async fn an_unsigned_upstream_version_advertises_no_signature() {
    let parts = local_registry_app_parts(REG, "openvsx", RegistryMode::Hybrid, None);
    parts.local_svc.hot.write().await.registries.insert(
        REG.to_owned(),
        Arc::new(UnsignedUpstream) as Arc<dyn RegistryClient>,
    );
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let files = gallery_files(&app, "up.stream").await;
    assert!(!files.contains(&SIGNATURE.to_owned()), "{files:?}");
    assert!(!files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");
    assert_eq!(asset(&app, "up.stream", "1.0.0", SIGNATURE).await.0, 404);
    assert_eq!(
        asset(
            &app,
            "up.stream",
            "1.0.0",
            "Microsoft.VisualStudio.Services.VSIXPackage"
        )
        .await
        .0,
        200
    );
}

// ── provided: an upstream's archive attached at publish time (§13.6) ────────

/// The marketplace's shape: the manifest over the stored bytes and a `.p7s`
/// with content, no `.sig`.
fn provided_archive(vsix_bytes: &[u8]) -> Vec<u8> {
    let manifest = batlehub_core::services::vsx_signature::signature_manifest(vsix_bytes).unwrap();
    let mut out = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file(".signature.manifest", opts).unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.start_file(".signature.p7s", opts).unwrap();
    zip.write_all(b"\x30\x82-pkcs7-from-the-marketplace")
        .unwrap();
    zip.finish().unwrap();
    out.into_inner()
}

async fn attach(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    id: &str,
    version: &str,
    archive: &[u8],
    token: Option<&str>,
) -> u16 {
    let mut req = TestRequest::put()
        .uri(&format!("/proxy/{REG}/{id}/{version}/vsix/signature"))
        .insert_header(("Content-Type", "application/zip"))
        .set_payload(archive.to_vec());
    if let Some(t) = token {
        req = req.insert_header(("Authorization", bearer(t)));
    }
    call_service(app, req.to_request()).await.status().as_u16()
}

#[actix_web::test]
async fn a_provided_archive_is_served_as_is_and_wins_over_the_registry_key() {
    let key = VsxSigningKey::from_seed([5u8; 32], Some("k5"));
    let (app, _) = app_with_key(RegistryMode::Local, Some(key)).await;
    let bytes = vsix("ms", "thing", "1.2.3");
    publish(&app, "ms.thing", "1.2.3", &bytes).await;
    // Before: the registry's own signature, with its key.
    let files = gallery_files(&app, "ms.thing").await;
    assert!(files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");

    let archive = provided_archive(&bytes);
    assert_eq!(
        attach(&app, "ms.thing", "1.2.3", &archive, Some(USER_TOKEN)).await,
        200
    );

    // After: the signature is advertised, the key is not — it is the signer's.
    let files = gallery_files(&app, "ms.thing").await;
    assert!(files.contains(&SIGNATURE.to_owned()), "{files:?}");
    assert!(!files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");
    let (status, body) = asset(&app, "ms.thing", "1.2.3", SIGNATURE).await;
    assert_eq!(status, 200);
    assert_eq!(&body[..], &archive[..], "the provided bytes, untouched");
    assert_eq!(asset(&app, "ms.thing", "1.2.3", PUBLIC_KEY).await.0, 404);
    let req = TestRequest::get()
        .uri(&format!("/proxy/{REG}/api/ms/thing/1.2.3"))
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let doc: Value = read_body_json(call_service(&app, req).await).await;
    assert!(doc["files"]["signature"].is_string(), "{doc}");
    assert!(doc["files"].get("publicKey").is_none(), "{doc}");

    // Re-attaching replaces; a second version stays registry-signed.
    assert_eq!(
        attach(&app, "ms.thing", "1.2.3", &archive, Some(USER_TOKEN)).await,
        200
    );
    let other = vsix("ms", "thing", "1.2.4");
    publish(&app, "ms.thing", "1.2.4", &other).await;
    let (status, a) = asset(&app, "ms.thing", "1.2.4", SIGNATURE).await;
    assert_eq!(status, 200);
    assert!(
        read_signature_archive(&a).is_ok(),
        "the registry's own archive for 1.2.4"
    );
}

#[actix_web::test]
async fn a_provided_archive_without_a_registry_key_is_still_advertised() {
    let (app, _) = app_with_key(RegistryMode::Local, None).await;
    let bytes = vsix("ms", "thing", "2.0.0");
    publish(&app, "ms.thing", "2.0.0", &bytes).await;
    assert!(!gallery_files(&app, "ms.thing")
        .await
        .contains(&SIGNATURE.to_owned()));
    assert_eq!(
        attach(
            &app,
            "ms.thing",
            "2.0.0",
            &provided_archive(&bytes),
            Some(USER_TOKEN)
        )
        .await,
        200
    );
    let files = gallery_files(&app, "ms.thing").await;
    assert!(files.contains(&SIGNATURE.to_owned()), "{files:?}");
    assert!(!files.contains(&PUBLIC_KEY.to_owned()), "{files:?}");
    assert_eq!(asset(&app, "ms.thing", "2.0.0", SIGNATURE).await.0, 200);
}

#[actix_web::test]
async fn a_provided_archive_is_checked_against_the_stored_bytes_and_gated_like_a_publish() {
    let (app, _) = app_with_key(RegistryMode::Local, None).await;
    let bytes = vsix("ms", "thing", "3.0.0");
    publish(&app, "ms.thing", "3.0.0", &bytes).await;
    let good = provided_archive(&bytes);
    // Anonymous: the publish gate.
    assert_eq!(attach(&app, "ms.thing", "3.0.0", &good, None).await, 403);
    // A version that is not here.
    assert_eq!(
        attach(&app, "ms.thing", "9.9.9", &good, Some(USER_TOKEN)).await,
        404
    );
    // A manifest over other bytes.
    let wrong = provided_archive(&vsix("ms", "thing", "3.0.1"));
    assert_eq!(
        attach(&app, "ms.thing", "3.0.0", &wrong, Some(USER_TOKEN)).await,
        400
    );
    // Not a signature archive at all.
    assert_eq!(
        attach(&app, "ms.thing", "3.0.0", b"not a zip", Some(USER_TOKEN)).await,
        400
    );
    // Nothing was advertised by the refusals.
    assert!(!gallery_files(&app, "ms.thing")
        .await
        .contains(&SIGNATURE.to_owned()));
    assert_eq!(asset(&app, "ms.thing", "3.0.0", SIGNATURE).await.0, 404);
}
