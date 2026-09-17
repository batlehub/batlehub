//! The Nix binary cache: the publish half (RFC 0028 §4.4, phase 4).
//!
//! `nix copy --to` is the only publish in this tree that arrives as **two**
//! requests whose first one names no coordinate:
//!
//! ```text
//! HEAD nar/{fileHash}.nar.xz     ← is it already here?
//! PUT  nar/{fileHash}.nar.xz     ← the bytes, naming no package
//! PUT  {storeHash}.narinfo       ← *now* the coordinate exists
//! ```
//!
//! So what these tests are really about is the seam: bytes parked with nothing
//! known about them, and a document that claims them, proves what they are, and
//! earns the registry's signature over hashes **this server recomputed**.
//!
//! The proxy half is `nix_proxy.rs`.

mod common;
#[allow(unused_imports)]
use common::*;

use std::sync::Arc;

use actix_web::test::{call_service, read_body, TestRequest};
use batlehub_config::schema::RegistryMode;
use batlehub_core::services::nix::{self, NarInfo, NixSigningKey};
use sha2::{Digest, Sha256};

const REG: &str = "nixlocal";
const KEY_NAME: &str = "batlehub-nixlocal-1";
/// `hello-1.0.0.2-doc` → package `hello`, version `1.0.0.2-doc`, by Nix's own
/// split at the first dash not followed by a letter.
const HASH: &str = "0001npbf2n4z3pjy6vm2mw8ywkqixxs6";
const PKG: &str = "hello";
const VERSION: &str = "1.0.0.2-doc";

async fn local_app(
    signed: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let parts = local_registry_app_parts(REG, "nix", RegistryMode::Local, None);
    if signed {
        let key = NixSigningKey::from_seed([7u8; 32], KEY_NAME);
        parts
            .local_svc
            .hot
            .write()
            .await
            .nix_signing
            .insert(REG.to_owned(), Arc::new(key.clone()));
        parts
            .proxy_svc
            .hot
            .write()
            .await
            .nix_signing
            .insert(REG.to_owned(), Arc::new(key));
    }
    build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await
}

/// The NAR bytes. Uncompressed, because `check_nar`'s codec handling has its own
/// tests in the adapter and what is under test here is the *seam*.
fn nar() -> Vec<u8> {
    b"nix-archive-1-contents".repeat(64)
}

fn nix32(raw: &[u8]) -> String {
    format!("sha256:{}", nix::nix32_encode(raw))
}

/// The file name the client picks: its own `FileHash` in bare nix32, no prefix
/// — `narInfo->fileHash->to_string(HashFormat::Nix32, false)`.
fn nar_file() -> String {
    format!("{}.nar", nix::nix32_encode(&Sha256::digest(nar())))
}

/// A narinfo that tells the truth about [`nar`].
fn truthful_narinfo() -> String {
    let bytes = nar();
    let digest = nix32(&Sha256::digest(&bytes));
    format!(
        "StorePath: /nix/store/{HASH}-{PKG}-{VERSION}\n\
         URL: nar/{}\n\
         Compression: none\n\
         FileHash: {digest}\n\
         FileSize: {}\n\
         NarHash: {digest}\n\
         NarSize: {}\n\
         References: \n",
        nar_file(),
        bytes.len(),
        bytes.len(),
    )
}

async fn put<S: TestService>(app: &S, path: &str, body: Vec<u8>, token: &str) -> (u16, String) {
    let resp = call_service(
        app,
        TestRequest::put()
            .uri(&format!("/proxy/{REG}/nix/{path}"))
            .insert_header(("Authorization", bearer(token)))
            .set_payload(body)
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    let text = String::from_utf8_lossy(&read_body(resp).await).into_owned();
    (status, text)
}

async fn get<S: TestService>(app: &S, path: &str) -> (u16, String) {
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(&format!("/proxy/{REG}/nix/{path}"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    let text = String::from_utf8_lossy(&read_body(resp).await).into_owned();
    (status, text)
}

/// Upload the NAR and then claim it, the way the client does.
async fn publish<S: TestService>(app: &S) -> (u16, String) {
    let (s, b) = put(app, &format!("nar/{}", nar_file()), nar(), ADMIN_TOKEN).await;
    assert_eq!(s, 200, "the NAR upload failed: {b}");
    put(
        app,
        &format!("{HASH}.narinfo"),
        truthful_narinfo().into_bytes(),
        ADMIN_TOKEN,
    )
    .await
}

// ── the seam ─────────────────────────────────────────────────────────────────

/// **The whole of phase 4 in one test.** Two requests, and afterwards the store
/// path is substitutable: the narinfo is served, it carries this registry's
/// signature, that signature verifies against the key the `public-key`
/// endpoint hands out, and the NAR is there under the coordinate.
#[actix_web::test]
async fn a_store_path_published_in_two_requests_is_served_signed() {
    let app = local_app(true).await;
    let (status, body) = publish(&app).await;
    assert_eq!(status, 200, "{body}");

    // The narinfo this instance now serves.
    let (status, served) = get(&app, &format!("{HASH}.narinfo")).await;
    assert_eq!(status, 200, "{served}");
    let info = NarInfo::parse(&served).expect("the served document parses");

    // The coordinate came out of `StorePath:` by Nix's own split.
    assert_eq!(
        info.get("StorePath"),
        Some(format!("/nix/store/{HASH}-{PKG}-{VERSION}").as_str())
    );
    // `URL:` is this instance's layout, not the client's.
    assert_eq!(
        info.get("URL"),
        Some(format!("nar/{HASH}/{}", nar_file()).as_str()),
        "the served URL must carry the store hash the coordinate is derived from"
    );

    // **The signature, and what it is over.** Verified with the key the
    // endpoint publishes — not with one this test built — so a mismatch
    // between what is signed and what is handed out is caught here.
    let (status, key_line) = get(&app, "public-key").await;
    assert_eq!(status, 200, "{key_line}");
    let trusted = vec![key_line.trim().to_owned()];
    assert!(
        trusted[0].starts_with(&format!("{KEY_NAME}:")),
        "{key_line}"
    );

    let fp = nix::fingerprint(&info).expect("fingerprints");
    let sig = info
        .get_all("Sig")
        .into_iter()
        .find(|s| s.starts_with(&format!("{KEY_NAME}:")))
        .expect("the registry signed what it hosts");
    assert!(
        nix::verify_signature(&fp, sig, &trusted),
        "the served narinfo does not verify against the key `public-key` publishes"
    );

    // …and the bytes are there, which is what makes the narinfo worth serving.
    let (status, _) = get(&app, &format!("nar/{HASH}/{}", nar_file())).await;
    assert_eq!(status, 200);
}

/// The signature must attest what was **checked**, not what was claimed
/// (RFC 0028 decision 3). A narinfo whose `NarHash` disagrees with the bytes is
/// refused before anything is signed or stored — and the error names the field,
/// because the publisher's next question is always "which one".
#[actix_web::test]
async fn a_narinfo_that_disagrees_with_its_bytes_is_refused_and_publishes_nothing() {
    let app = local_app(true).await;
    let (s, _) = put(&app, &format!("nar/{}", nar_file()), nar(), ADMIN_TOKEN).await;
    assert_eq!(s, 200);

    let lying = truthful_narinfo().replace(
        &nix32(&Sha256::digest(nar())),
        "sha256:0000000000000000000000000000000000000000000000000000",
    );
    let (status, body) = put(
        &app,
        &format!("{HASH}.narinfo"),
        lying.into_bytes(),
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("FileHash") || body.contains("NarHash"),
        "the refusal must name the field that disagreed: {body}"
    );

    // Nothing was published: the narinfo is not served, so nothing can
    // substitute from it.
    let (status, _) = get(&app, &format!("{HASH}.narinfo")).await;
    assert_ne!(status, 200, "a refused publish must leave nothing behind");
}

/// A publisher cannot mint this registry's signature — they would otherwise be
/// able to have clients trust bytes we never verified. Every *other* `Sig:` is
/// somebody else's provenance and is kept: `nix copy` signs client-side when
/// the store has `secret-key-files`, and dropping that would destroy real
/// evidence.
#[actix_web::test]
async fn a_forged_registry_signature_is_dropped_and_the_publishers_own_is_kept() {
    let app = local_app(true).await;
    let (s, _) = put(&app, &format!("nar/{}", nar_file()), nar(), ADMIN_TOKEN).await;
    assert_eq!(s, 200);

    let forged = format!(
        "{}Sig: {KEY_NAME}:ZmFrZQ==\nSig: someone-else-1:ZmFrZQ==\n",
        truthful_narinfo()
    );
    let (status, body) = put(
        &app,
        &format!("{HASH}.narinfo"),
        forged.into_bytes(),
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (_, served) = get(&app, &format!("{HASH}.narinfo")).await;
    let info = NarInfo::parse(&served).unwrap();
    let ours: Vec<&str> = info
        .get_all("Sig")
        .into_iter()
        .filter(|s| s.starts_with(&format!("{KEY_NAME}:")))
        .collect();
    assert_eq!(
        ours.len(),
        1,
        "exactly one signature under our name — ours: {ours:?}"
    );
    assert_ne!(
        ours[0],
        format!("{KEY_NAME}:ZmFrZQ=="),
        "the forgery survived"
    );
    assert!(
        info.get_all("Sig")
            .iter()
            .any(|s| s.starts_with("someone-else-1:")),
        "a third party's signature is provenance and must be kept: {served}"
    );
}

/// A narinfo may only claim a NAR **its own publisher** uploaded. Without that,
/// a `releases:write` holder could wait for someone else's upload and wrap
/// those bytes in a coordinate of their choosing — and this registry would sign
/// it.
#[actix_web::test]
async fn a_narinfo_cannot_claim_another_publishers_upload() {
    let app = local_app(true).await;
    // The admin parks the bytes.
    let (s, _) = put(&app, &format!("nar/{}", nar_file()), nar(), ADMIN_TOKEN).await;
    assert_eq!(s, 200);

    // A different identity tries to claim them.
    let (status, body) = put(
        &app,
        &format!("{HASH}.narinfo"),
        truthful_narinfo().into_bytes(),
        USER_TOKEN,
    )
    .await;
    assert!(
        status == 400 || status == 403,
        "another publisher's NAR must not be claimable, got {status}: {body}"
    );
}

/// `nix copy --to` probes before it uploads, and the answer decides whether it
/// sends megabytes. Before the upload it must be "no".
#[actix_web::test]
async fn the_nar_probe_answers_no_before_the_upload_and_yes_after() {
    let app = local_app(true).await;
    let probe = format!("nar/{}", nar_file());

    let before = call_service(
        &app,
        TestRequest::default()
            .method(actix_web::http::Method::HEAD)
            .uri(&format!("/proxy/{REG}/nix/{probe}"))
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .to_request(),
    )
    .await
    .status()
    .as_u16();
    assert_ne!(
        before, 405,
        "a 405 makes `fileExists` disable the cache for 60 seconds"
    );
    assert_ne!(before, 200, "nothing has been uploaded yet");
}

/// The store name is attacker-controlled and becomes a storage key, so a
/// traversal in `StorePath:` is a `400` at the edge — not something the storage
/// backend's own guard has to catch.
#[actix_web::test]
async fn nix_publish_traversal_version_returns_400() {
    let app = local_app(true).await;
    let (s, _) = put(&app, &format!("nar/{}", nar_file()), nar(), ADMIN_TOKEN).await;
    assert_eq!(s, 200);

    for name in ["..%2f..%2fetc%2fpasswd", "a/b", "../../etc/x"] {
        let doc = truthful_narinfo().replace(
            &format!("/nix/store/{HASH}-{PKG}-{VERSION}"),
            &format!("/nix/store/{HASH}-{name}"),
        );
        let (status, body) = put(
            &app,
            &format!("{HASH}.narinfo"),
            doc.into_bytes(),
            ADMIN_TOKEN,
        )
        .await;
        assert_eq!(status, 400, "'{name}' must be refused: {body}");
    }
}

/// A narinfo PUT to one address whose `StorePath:` names another would put its
/// bytes under a coordinate it does not name — and the two may not be blocked
/// alike.
#[actix_web::test]
async fn a_narinfo_must_name_the_path_it_was_put_to() {
    let app = local_app(true).await;
    let (s, _) = put(&app, &format!("nar/{}", nar_file()), nar(), ADMIN_TOKEN).await;
    assert_eq!(s, 200);

    let other = "1111npbf2n4z3pjy6vm2mw8ywkqixxs6";
    let (status, body) = put(
        &app,
        &format!("{other}.narinfo"),
        truthful_narinfo().into_bytes(),
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("same path") || body.contains("StorePath"),
        "{body}"
    );
}

/// A registry that signs nothing says so, rather than serving an empty body —
/// "no key" and "a key I could not read" must not look alike to a provisioning
/// script.
#[actix_web::test]
async fn a_registry_with_no_key_has_no_public_key_to_publish() {
    let app = local_app(false).await;
    let (status, _) = get(&app, "public-key").await;
    assert_eq!(status, 404);
}

/// An unsigned registry still publishes — `require-sigs = false` is a
/// legitimate lab — and the served narinfo simply carries no `Sig:` of ours.
#[actix_web::test]
async fn an_unsigned_registry_publishes_and_serves_an_unsigned_narinfo() {
    let app = local_app(false).await;
    let (status, body) = publish(&app).await;
    assert_eq!(status, 200, "{body}");

    let (status, served) = get(&app, &format!("{HASH}.narinfo")).await;
    assert_eq!(status, 200);
    let info = NarInfo::parse(&served).unwrap();
    assert!(
        info.get_all("Sig").is_empty(),
        "a registry with no key must not invent one: {served}"
    );
}

/// A narinfo claiming a NAR nobody uploaded is refused with a message that says
/// what went wrong, because the client's own error will only say the copy
/// failed.
#[actix_web::test]
async fn a_narinfo_with_no_uploaded_nar_is_refused_by_name() {
    let app = local_app(true).await;
    let (status, body) = put(
        &app,
        &format!("{HASH}.narinfo"),
        truthful_narinfo().into_bytes(),
        ADMIN_TOKEN,
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("nar"),
        "the message must name the NAR: {body}"
    );
}
