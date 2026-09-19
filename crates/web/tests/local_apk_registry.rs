//! Alpine `apk`: the local/hybrid half — publish, the signed index, the served
//! key, and the one thing that makes this index worth generating at all, which
//! is that a blocked version is *absent* from it (RFC 0026 §4.4, §10).
//!
//! The proxy half — the relayed index, the coordinate on a `.apk`, a blocked
//! version refused with a `403` — is in `apk_proxy.rs`; the two are separate
//! files because they answer different questions about the same kind.

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body, TestRequest};

use batlehub_config::schema::RegistryMode;
use batlehub_web::ApkSignerMap;

/// A 2048-bit RSA key, the same one `crates/adapters` signs its fixtures with.
const SIGNING_KEY: &str =
    include_str!("../../adapters/src/repo/testdata/apk_signing_2048.pkcs8.pem");
const KEY_NAME: &str = "internal@example.com-5f3a1c2e.rsa.pub";

const PKGINFO: &str = "pkgname = hello\npkgver = 1.0-r0\narch = x86_64\nsize = 1024\n\
                       builddate = 1700000000\npkgdesc = a greeting\nlicense = MIT\n";

/// A `.apk` as `apk mkpkg` writes one: no signature member, control then data.
///
/// Deliberately unsigned. A package with no `.SIGN.*` member still installs from
/// a signed index, because the install path checks the index's `C:` and never
/// the package's own signature — and that is exactly what makes local
/// publishing tractable (RFC 0026 §2.5).
fn make_test_apk(pkginfo: &str) -> Vec<u8> {
    use std::io::Write;

    let tar_of = |name: &str, content: &[u8]| {
        let mut tb = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tb.append(&header, content).unwrap();
        tb.into_inner().unwrap()
    };
    let gz = |bytes: &[u8]| {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    };

    [
        gz(&tar_of(".PKGINFO", pkginfo.as_bytes())),
        gz(&tar_of("usr/bin/hello", b"#!/bin/sh\necho hi\n")),
    ]
    .concat()
}

/// Read the `APKINDEX` text back out of a served `APKINDEX.tar.gz`.
///
/// Walks the concatenated gzip members: the index lives behind the signature,
/// and a single-member reader finds only the signature.
fn index_text(file: &[u8]) -> String {
    use std::io::Read;

    let mut offset = 0usize;
    while offset < file.len() {
        let mut cursor = std::io::Cursor::new(&file[offset..]);
        let mut plain = Vec::new();
        {
            let mut dec = flate2::bufread::GzDecoder::new(&mut cursor);
            if dec.read_to_end(&mut plain).is_err() {
                break;
            }
        }
        let consumed = cursor.position() as usize;
        if consumed == 0 {
            break;
        }
        let mut archive = tar::Archive::new(std::io::Cursor::new(plain));
        if let Ok(entries) = archive.entries() {
            for entry in entries.flatten() {
                let is_index = entry
                    .path()
                    .ok()
                    .is_some_and(|p| p.to_string_lossy() == "APKINDEX");
                if is_index {
                    let mut text = String::new();
                    let mut entry = entry;
                    entry.read_to_string(&mut text).unwrap();
                    return text;
                }
            }
        }
        offset += consumed;
    }
    panic!("no APKINDEX entry in the served file");
}

/// The tar entry names of the file's first gzip member — the signature member.
fn first_member_entries(file: &[u8]) -> Vec<String> {
    use std::io::Read;

    let mut cursor = std::io::Cursor::new(file);
    let mut plain = Vec::new();
    {
        let mut dec = flate2::bufread::GzDecoder::new(&mut cursor);
        dec.read_to_end(&mut plain).unwrap();
    }
    tar::Archive::new(std::io::Cursor::new(plain))
        .entries()
        .unwrap()
        .flatten()
        .filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
        .collect()
}

/// An app for an `apk` registry, optionally with a signing key.
async fn apk_app(
    registry: &str,
    mode: RegistryMode,
    signed: bool,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    let parts = local_registry_app_parts(registry, "apk", mode, None);
    let LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
    } = parts;

    let mut signers = HashMap::new();
    if signed {
        signers.insert(
            registry.to_owned(),
            Arc::new(batlehub_adapters::repo::ApkSigner::from_pem(SIGNING_KEY, KEY_NAME).unwrap()),
        );
    }

    finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        ApkSignerMap::from(signers),
        test_auth_providers(),
    )
    .await
}

async fn publish(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    registry: &str,
    pkginfo: &str,
) -> u16 {
    call_service(
        app,
        TestRequest::put()
            .uri(&format!("/proxy/{registry}/apk/upload"))
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_test_apk(pkginfo))
            .to_request(),
    )
    .await
    .status()
    .as_u16()
}

async fn get_bytes(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    uri: &str,
) -> (u16, Vec<u8>) {
    let resp = call_service(
        app,
        TestRequest::get()
            .uri(uri)
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    let status = resp.status().as_u16();
    (status, read_body(resp).await.to_vec())
}

#[actix_web::test]
async fn apk_publish_then_read_index_and_package() {
    let app = apk_app("alpine", RegistryMode::Local, false).await;
    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);

    let (status, index) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    assert_eq!(status, 200);
    let text = index_text(&index);
    assert!(
        text.contains("P:hello\n"),
        "index lists the package: {text}"
    );
    assert!(text.contains("V:1.0-r0\n"));
    assert!(text.contains("A:x86_64\n"));
    // The build date the age gate reads.
    assert!(text.contains("t:1700000000\n"));
    // The identity apk checks at install, computed from the upload itself.
    assert!(
        text.contains("C:Q1") && text.contains('='),
        "the index carries a Q1 identity: {text}"
    );

    // The package is downloadable under the name its own `.PKGINFO` gave it,
    // never the one the client sent (the request above sent no name at all).
    let (status, bytes) = get_bytes(&app, "/proxy/alpine/apk/x86_64/hello-1.0-r0.apk").await;
    assert_eq!(status, 200);
    assert_eq!(bytes, make_test_apk(PKGINFO), "stored byte-exact");
}

#[actix_web::test]
async fn apk_publish_requires_authentication() {
    let app = apk_app("alpine", RegistryMode::Local, false).await;
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/alpine/apk/upload")
            .set_payload(make_test_apk(PKGINFO))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

/// The traversal test every registry kind owes (CLAUDE.md step 8): a version
/// that would escape the storage key is a `400` at the edge.
#[actix_web::test]
async fn apk_publish_traversal_version_returns_400() {
    let app = apk_app("alpine", RegistryMode::Local, false).await;
    let evil = "pkgname = hello\npkgver = ../../etc/x\narch = x86_64\n";
    assert_eq!(publish(&app, "alpine", evil).await, 400);
}

/// **The stored file name has to parse back to the coordinate it was stored
/// for.** `apk_coordinate` splits from the right and needs a `-r<digits>`
/// release suffix, and `{name}-{version}.apk` does not automatically survive
/// that:
///
///   * `1.0` has no release suffix at all, so the publish used to return 201
///     and every download of those bytes a 400 — unreachable for good.
///   * `1.0-beta-r0` parses back as `("hello-1.0", "beta-r0")`, so the index
///     entry and the download gate name different coordinates and a block
///     removes the listing without refusing the direct fetch.
///
/// Both are refused at the edge now, where the publisher can still act on it.
#[actix_web::test]
async fn apk_publish_refuses_a_version_that_does_not_round_trip() {
    let app = apk_app("alpine", RegistryMode::Local, false).await;

    for bad in ["1.0", "1.0-beta-r0", "1.0-rX", "1.0-r"] {
        let pkginfo = format!("pkgname = hello\npkgver = {bad}\narch = x86_64\n");
        assert_eq!(
            publish(&app, "alpine", &pkginfo).await,
            400,
            "pkgver {bad:?} does not round-trip through apk_coordinate and must be refused"
        );
    }

    // The spelling `abuild` actually produces still publishes.
    let good = "pkgname = hello\npkgver = 1.0-r0\narch = x86_64\n";
    assert_eq!(publish(&app, "alpine", good).await, 201);
}

#[actix_web::test]
async fn apk_publish_traversal_arch_returns_400() {
    let app = apk_app("alpine", RegistryMode::Local, false).await;
    let evil = "pkgname = hello\npkgver = 1.0-r0\narch = ../../etc\n";
    assert_eq!(publish(&app, "alpine", evil).await, 400);
}

/// A `.PKGINFO` that tries to smuggle an index line does not get one.
///
/// The file format makes the direct attack impossible — `PkgInfo::parse` splits
/// on lines and then on `=`, so a *value* can never hold a newline — and a
/// stray `P:evil` line in the control file has no `=` and is skipped rather
/// than becoming a field. This pins that: the published index describes one
/// package, named by `pkgname`, and not the attacker's.
#[actix_web::test]
async fn apk_publish_cannot_smuggle_an_index_line_through_pkginfo() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    let sneaky = "pkgname = hello\npkgver = 1.0-r0\narch = x86_64\nP:evil\nV:9.9-r9\n\
                  pkgdesc = greeting\n";
    assert_eq!(publish(&app, "alpine", sneaky).await, 201);

    let (_, index) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    let text = index_text(&index);
    assert!(text.contains("P:hello\n"), "the real package: {text}");
    assert!(!text.contains("P:evil"), "no smuggled entry: {text}");
    assert!(!text.contains("V:9.9-r9"), "no smuggled version: {text}");
    assert_eq!(
        text.matches("P:").count(),
        1,
        "exactly one package in the index: {text}"
    );
}

#[actix_web::test]
async fn apk_signed_publish_emits_an_rsa256_entry_and_serves_the_key() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;

    // The key is served *before* anything is published, so a client can be set
    // up first.
    let (status, key) = get_bytes(&app, &format!("/proxy/alpine/apk/keys/{KEY_NAME}")).await;
    assert_eq!(status, 200, "the key is live before the first publish");
    let key = String::from_utf8(key).unwrap();
    assert!(key.starts_with("-----BEGIN PUBLIC KEY-----"), "got: {key}");

    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);

    let (status, index) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    assert_eq!(status, 200);

    // The signature member comes first and is named for the key, because apk
    // looks the key up by exactly that name in /etc/apk/keys.
    let entries = first_member_entries(&index);
    assert_eq!(
        entries,
        vec![format!(".SIGN.RSA256.{KEY_NAME}")],
        "the first member is the signature, named for the key"
    );
    // And the index is still readable behind it.
    assert!(index_text(&index).contains("P:hello\n"));
}

/// A name that is not this registry's key is a `404`, so a client that typos
/// the file name learns it at `curl` rather than at `apk update` — where the
/// symptom would be "untrusted index" and not "no such key".
#[actix_web::test]
async fn apk_key_route_answers_only_the_configured_name() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    let (status, _) = get_bytes(&app, "/proxy/alpine/apk/keys/typo.rsa.pub").await;
    assert_eq!(status, 404);
}

#[actix_web::test]
async fn apk_key_route_is_404_on_an_unsigned_registry() {
    let app = apk_app("alpine", RegistryMode::Local, false).await;
    let (status, _) = get_bytes(&app, &format!("/proxy/alpine/apk/keys/{KEY_NAME}")).await;
    assert_eq!(status, 404);
}

/// The index this instance signs is the one it may edit, and RFC 0006's rule
/// applies to it in full: a blocked version is **absent**, so apk's solver
/// reports its own "unable to select" rather than downloading anything.
#[actix_web::test]
async fn apk_blocked_version_is_absent_from_the_regenerated_index() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);

    let second = "pkgname = other\npkgver = 2.0-r0\narch = x86_64\nbuilddate = 1700000001\n";
    assert_eq!(publish(&app, "alpine", second).await, 201);

    let (_, index) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    let text = index_text(&index);
    assert!(
        text.contains("P:hello\n"),
        "both are listed before the block"
    );
    assert!(text.contains("P:other\n"));

    // Block `hello` 1.0-r0 through the admin API, then republish `other` to
    // force a regeneration — the block-change hook is the *next* test.
    let block = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/packages/block")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({
                "registry": "alpine",
                "name": "hello",
                "version": "1.0-r0",
                "reason": "test",
            }))
            .to_request(),
    )
    .await;
    assert_eq!(block.status(), 200, "the block was accepted");

    assert_eq!(publish(&app, "alpine", second).await, 201);

    let (_, index) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    let text = index_text(&index);
    assert!(
        !text.contains("P:hello\n"),
        "the blocked version is gone from the index: {text}"
    );
    assert!(
        text.contains("P:other\n"),
        "and the rest of the repository is intact: {text}"
    );
}

/// **The block-change hook**, and the reason it exists: the index must lose the
/// blocked version *without* anyone republishing.
///
/// The test above proves the filter runs at generation; this proves a block
/// triggers a generation. Without it a block would be enforced at the download
/// and invisible in the listing until the next upload — correct behaviour
/// resting on a stale document (RFC 0026 §6.4).
#[actix_web::test]
async fn apk_block_regenerates_the_index_with_no_republish() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);

    let (_, before) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    assert!(index_text(&before).contains("P:hello\n"));

    let block = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/packages/block")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({
                "registry": "alpine",
                "name": "hello",
                "version": "1.0-r0",
                "reason": "test",
            }))
            .to_request(),
    )
    .await;
    assert_eq!(block.status(), 200);

    // No publish between the block and this read.
    let (_, after) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    let text = index_text(&after);
    assert!(
        !text.contains("P:hello"),
        "the block reached the index on its own: {text}"
    );

    // And unblocking puts it back, re-signed.
    let unblock = call_service(
        &app,
        TestRequest::post()
            .uri("/api/v1/admin/packages/unblock")
            .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
            .set_json(serde_json::json!({
                "registry": "alpine",
                "name": "hello",
                "version": "1.0-r0",
            }))
            .to_request(),
    )
    .await;
    assert_eq!(unblock.status(), 200);

    let (_, restored) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    assert!(
        index_text(&restored).contains("P:hello\n"),
        "unblocking restores the entry"
    );
    assert_eq!(
        index_text(&before),
        index_text(&restored),
        "and restores exactly the document that was there before"
    );
}

/// Publishing twice must produce the same bytes for the same set of packages —
/// otherwise every regeneration re-signs a different document and clients
/// re-download half a megabyte for nothing.
#[actix_web::test]
async fn apk_index_generation_is_deterministic() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);
    let (_, first) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;

    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);
    let (_, second) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;

    assert_eq!(
        index_text(&first),
        index_text(&second),
        "republishing the same package leaves the index text unchanged"
    );
}

/// Each architecture has its own index, and a publish to one must not disturb
/// the other — the `{arch}` in the sidecar key is what keeps them apart.
#[actix_web::test]
async fn apk_architectures_get_separate_indexes() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);
    let arm = "pkgname = hello\npkgver = 1.0-r0\narch = aarch64\nbuilddate = 1700000000\n";
    assert_eq!(publish(&app, "alpine", arm).await, 201);

    for (arch, other) in [("x86_64", "aarch64"), ("aarch64", "x86_64")] {
        let (status, index) =
            get_bytes(&app, &format!("/proxy/alpine/apk/{arch}/APKINDEX.tar.gz")).await;
        assert_eq!(status, 200);
        let text = index_text(&index);
        assert!(text.contains(&format!("A:{arch}\n")), "{arch}: {text}");
        assert!(!text.contains(&format!("A:{other}\n")), "{arch}: {text}");
    }
}

// ── What a real apk refuses ──────────────────────────────────────────────────
//
// Five defects reached this file only when `tests/heavy/apk.sh` was first run
// against `apk.static` (RFC 0026 §13). Every test above passed through all of
// them, because our own reader is more forgiving than the format: it walks the
// gzip members independently, so it cannot see a tar stream that ends in the
// middle, and it never checks a header field it does not read. These pin the
// three that live in the bytes this server writes.

/// An `APKINDEX.tar.gz` is **one tar stream split across two gzip members**,
/// not two tar files. A writer that finalises the signature member puts the
/// end-of-archive marker in the middle of the stream, and apk — handed the
/// data member after it — answers `BAD archive` and resolves nothing.
#[actix_web::test]
async fn apk_index_is_one_tar_stream_with_no_interior_end_of_archive() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);

    let (status, index) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    assert_eq!(status, 200);

    let members = gzip_member_plaintexts(&index);
    assert_eq!(members.len(), 2, "signature member, then the data member");
    assert!(
        !members[0].ends_with(&[0u8; 1024][..]),
        "the signature member ends the tar stream: apk answers BAD archive"
    );
    assert!(
        members[1].ends_with(&[0u8; 1024][..]),
        "the last member must end the stream, or the archive never terminates"
    );
}

/// apk's tar reader takes the POSIX `ustar` spelling and rejects the GNU one
/// *for the signature entry only* — the index still parses, and `apk update`
/// reports `BAD signature` on a signature that verifies with `openssl`. The
/// failure points at the key and the digest, neither of which is wrong, which
/// is why the shape is pinned here.
#[actix_web::test]
async fn apk_index_entries_carry_posix_ustar_headers() {
    let app = apk_app("alpine", RegistryMode::Local, true).await;
    assert_eq!(publish(&app, "alpine", PKGINFO).await, 201);

    let (status, index) = get_bytes(&app, "/proxy/alpine/apk/x86_64/APKINDEX.tar.gz").await;
    assert_eq!(status, 200);

    for (i, member) in gzip_member_plaintexts(&index).iter().enumerate() {
        let header = &member[..512];
        assert_eq!(&header[257..263], b"ustar\0", "member {i}: POSIX magic");
        assert_eq!(&header[263..265], b"00", "member {i}: POSIX version");
        assert_eq!(header[156], b'0', "member {i}: a regular-file typeflag");
    }
}

/// `apk mkpkg` — apk-tools 3's own builder — writes the v3 (ADB) container.
/// A v2 `APKINDEX` cannot describe one, so it is refused; the point of the
/// test is the **status and the sentence**, because the first run of the heavy
/// suite met a `502 Bad Gateway` whose message did not name the format.
#[actix_web::test]
async fn apk_publish_refuses_a_v3_package_with_a_400_that_names_the_format() {
    let app = apk_app("alpine", RegistryMode::Local, false).await;

    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/alpine/apk/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(b"ADBd\x6d\x51\x3d\x4b not really an adb container".to_vec())
            .to_request(),
    )
    .await;
    assert_eq!(
        resp.status().as_u16(),
        400,
        "bytes the client sent are a bad request, never a bad gateway"
    );
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(
        body.contains("v3") && body.contains("abuild"),
        "the refusal names the format and what to build with instead: {body}"
    );
}

/// The plaintext of each gzip member of a concatenated-member file.
fn gzip_member_plaintexts(file: &[u8]) -> Vec<Vec<u8>> {
    use std::io::Read;

    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < file.len() {
        let mut cursor = std::io::Cursor::new(&file[offset..]);
        let mut plain = Vec::new();
        {
            let mut dec = flate2::bufread::GzDecoder::new(&mut cursor);
            if dec.read_to_end(&mut plain).is_err() {
                break;
            }
        }
        let consumed = cursor.position() as usize;
        if consumed == 0 {
            break;
        }
        out.push(plain);
        offset += consumed;
    }
    out
}
