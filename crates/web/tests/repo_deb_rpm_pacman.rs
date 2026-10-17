//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::test::{call_service, read_body, TestRequest};

use batlehub_adapters::in_memory::InMemoryPackageRepository as InMemoryRepo;
use batlehub_config::schema::RegistryMode;
use batlehub_core::{entities::Role, rules::RbacRule, services::RegistryPolicy};
use batlehub_web::RepoSignerMap;

// ── Deb / RPM repository hosting ────────────────────────────────────────────

/// Build a minimal `.deb` (ar archive: debian-binary + control.tar.gz) for tests.
fn make_test_deb(control: &str) -> Vec<u8> {
    use std::io::Write;
    let mut tar_buf = Vec::new();
    {
        let mut tb = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_path("./control").unwrap();
        header.set_size(control.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tb.append(&header, control.as_bytes()).unwrap();
        tb.finish().unwrap();
    }
    let mut gz = Vec::new();
    {
        let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
        enc.write_all(&tar_buf).unwrap();
        enc.finish().unwrap();
    }
    let mut deb = Vec::new();
    {
        let mut builder = ar::Builder::new(&mut deb);
        let db = b"2.0\n";
        builder
            .append(
                &ar::Header::new(b"debian-binary".to_vec(), db.len() as u64),
                &db[..],
            )
            .unwrap();
        builder
            .append(
                &ar::Header::new(b"control.tar.gz".to_vec(), gz.len() as u64),
                &gz[..],
            )
            .unwrap();
    }
    deb
}

const HELLO_CONTROL: &str =
    "Package: hello\nVersion: 1.0\nArchitecture: amd64\nMaintainer: me\nDescription: hi\n";

#[actix_web::test]
async fn deb_publish_then_read_indexes_and_pool() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let deb = make_test_deb(HELLO_CONTROL);
    let publish = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/apt/deb/pool/stable/main/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(deb.clone())
            .to_request(),
    )
    .await;
    assert_eq!(publish.status(), 201);

    // Packages index lists the package with its pool Filename.
    let packages = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/dists/stable/main/binary-amd64/Packages")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(packages.status(), 200);
    let body = String::from_utf8(read_body(packages).await.to_vec()).unwrap();
    assert!(body.contains("Package: hello"));
    assert!(body.contains("Filename: pool/main/h/hello/hello_1.0_amd64.deb"));

    // Release references the Packages file.
    let release = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/dists/stable/Release")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(release.status(), 200);
    let release_body = String::from_utf8(read_body(release).await.to_vec()).unwrap();
    assert!(release_body.contains("Suite: stable"));
    assert!(release_body.contains("main/binary-amd64/Packages"));

    // The .deb is downloadable from the pool, byte-for-byte.
    let pool = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/pool/main/h/hello/hello_1.0_amd64.deb")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(pool.status(), 200);
    assert_eq!(read_body(pool).await.to_vec(), deb);

    // Unsigned repo: no InRelease.
    let inrelease = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/dists/stable/InRelease")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(inrelease.status(), 404);
}

#[actix_web::test]
async fn deb_publish_requires_authentication() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/apt/deb/pool/stable/main/upload")
            .set_payload(make_test_deb(HELLO_CONTROL))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn deb_publish_traversal_in_control_file_returns_400() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let control =
        "Package: ../../etc\nVersion: 1.0\nArchitecture: amd64\nMaintainer: me\nDescription: hi\n";
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/apt/deb/pool/stable/main/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_test_deb(control))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn deb_signed_publish_emits_inrelease_and_key() {
    // 32-byte Ed25519 seed (hex).
    let seed = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";
    let signer = Arc::new(
        batlehub_adapters::repo::OpenPgpSigner::from_seed_hex(seed, 1_700_000_000, "BatleHub")
            .unwrap(),
    );
    let mut map = HashMap::new();
    map.insert("apt".to_owned(), signer);
    let signer_map = RepoSignerMap::from(map);

    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
    } = parts;
    let app = finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        signer_map,
        test_auth_providers(),
    )
    .await;

    let publish = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/apt/deb/pool/stable/main/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_test_deb(HELLO_CONTROL))
            .to_request(),
    )
    .await;
    assert_eq!(publish.status(), 201);

    let inrelease = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/dists/stable/InRelease")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(inrelease.status(), 200);
    let body = String::from_utf8(read_body(inrelease).await.to_vec()).unwrap();
    assert!(body.contains("-----BEGIN PGP SIGNED MESSAGE-----"));
    assert!(body.contains("Suite: stable"));
    assert!(body.contains("-----BEGIN PGP SIGNATURE-----"));

    let key = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/key.gpg")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(key.status(), 200);
    let key_body = String::from_utf8(read_body(key).await.to_vec()).unwrap();
    assert!(key_body.contains("-----BEGIN PGP PUBLIC KEY BLOCK-----"));
}

/// Build a minimal real `.rpm` for tests.
fn make_test_rpm() -> Vec<u8> {
    let pkg = rpm::PackageBuilder::new("hello", "1.0", "MIT", "x86_64", "a greeting")
        .with_file_contents(
            b"#!/bin/sh\necho hi\n".to_vec(),
            rpm::FileOptions::new("/usr/bin/hello").mode(0o100755),
        )
        .unwrap()
        .build()
        .unwrap();
    let mut buf = Vec::new();
    pkg.write(&mut buf).unwrap();
    buf
}

#[actix_web::test]
async fn rpm_publish_then_read_repodata_and_package() {
    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let rpm_bytes = make_test_rpm();
    let publish = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/yum/rpm/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(rpm_bytes.clone())
            .to_request(),
    )
    .await;
    assert_eq!(publish.status(), 201);

    // repomd.xml references the primary metadata.
    let repomd = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/repodata/repomd.xml")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(repomd.status(), 200);
    let repomd_body = String::from_utf8(read_body(repomd).await.to_vec()).unwrap();
    assert!(repomd_body.contains(r#"<data type="primary">"#));
    assert!(repomd_body.contains("repodata/primary.xml.gz"));

    // primary.xml.gz exists and gunzips to metadata naming the package.
    let primary = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/repodata/primary.xml.gz")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(primary.status(), 200);
    let gz = read_body(primary).await.to_vec();
    let mut decoder = flate2::read::GzDecoder::new(&gz[..]);
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut xml).unwrap();
    assert!(xml.contains("<name>hello</name>"));
    assert!(xml.contains("packages/hello-1.0-1.x86_64.rpm"));

    // The .rpm is downloadable, byte-for-byte.
    let pkg = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/packages/hello-1.0-1.x86_64.rpm")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(pkg.status(), 200);
    assert_eq!(read_body(pkg).await.to_vec(), rpm_bytes);
}

/// rpm_publish doesn't call `validate_coordinate` like deb/pacman do — it derives
/// its storage id from `RpmPackage::storage_id()` (name-version-release[.epoch].arch)
/// and validates that with `validate_path_safe`, so the traversal payload must be
/// injected into the RPM header's name field rather than assumed to hit the same
/// validation function as the other two formats.
///
/// `rpm::PackageBuilder` itself rejects '/' in the name/version fields, so a
/// well-formed build can never carry a traversal payload — this simulates a
/// hand-crafted malicious upload by patching the built header bytes in place,
/// swapping the safely-built name for a same-length string containing a `..`
/// path segment. `rpm::Package::parse` (used by `parse_rpm`) does not re-validate
/// the character set on read, only the builder does on write.
#[actix_web::test]
async fn rpm_publish_traversal_in_header_returns_400() {
    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let pkg = rpm::PackageBuilder::new("abcde", "1.0", "MIT", "x86_64", "a greeting")
        .with_file_contents(
            b"#!/bin/sh\necho hi\n".to_vec(),
            rpm::FileOptions::new("/usr/bin/hello").mode(0o100755),
        )
        .unwrap()
        .build()
        .unwrap();
    let mut rpm_bytes = Vec::new();
    pkg.write(&mut rpm_bytes).unwrap();

    // Swap every occurrence of the 5-byte placeholder name for a same-length
    // traversal payload, so the header's tag-length bookkeeping stays valid
    // regardless of which of the legacy lead / main header / auto-provide
    // entries actually feeds `parse_rpm`'s extracted name.
    let needle = b"abcde";
    let replacement = b"a/../";
    let mut i = 0;
    while i + needle.len() <= rpm_bytes.len() {
        if &rpm_bytes[i..i + needle.len()] == needle {
            rpm_bytes[i..i + needle.len()].copy_from_slice(replacement);
        }
        i += 1;
    }

    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/yum/rpm/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(rpm_bytes)
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn rpm_signed_publish_emits_repomd_asc() {
    let seed = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";
    let signer = Arc::new(
        batlehub_adapters::repo::OpenPgpSigner::from_seed_hex(seed, 1_700_000_000, "BatleHub")
            .unwrap(),
    );
    let mut sm = HashMap::new();
    sm.insert("yum".to_owned(), signer);

    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Local, None);
    let LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
    } = parts;
    let app = finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        RepoSignerMap::from(sm),
        test_auth_providers(),
    )
    .await;

    let publish = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/yum/rpm/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_test_rpm())
            .to_request(),
    )
    .await;
    assert_eq!(publish.status(), 201);

    let asc = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/repodata/repomd.xml.asc")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(asc.status(), 200);
    let body = String::from_utf8(read_body(asc).await.to_vec()).unwrap();
    assert!(body.contains("-----BEGIN PGP SIGNATURE-----"));
}

#[actix_web::test]
async fn rpm_signing_key_is_served_before_any_publish() {
    // A client must be able to fetch the repo signing key to configure dnf BEFORE
    // the first package is published (the key is otherwise only written on publish).
    let seed = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";
    let signer = Arc::new(
        batlehub_adapters::repo::OpenPgpSigner::from_seed_hex(seed, 1_700_000_000, "BatleHub")
            .unwrap(),
    );
    let mut sm = HashMap::new();
    sm.insert("yum".to_owned(), signer);

    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Local, None);
    let LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
    } = parts;
    let app = finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        RepoSignerMap::from(sm),
        test_auth_providers(),
    )
    .await;

    // No publish has happened — the key still resolves from the live signer.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/repodata/repomd.xml.key")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("-----BEGIN PGP PUBLIC KEY BLOCK-----"));
}

#[actix_web::test]
async fn rpm_signing_key_is_404_when_unsigned() {
    // No signer configured → no key (an unsigned repo); must not imply one exists.
    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/repodata/repomd.xml.key")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

// ── Pacman repository hosting ────────────────────────────────────────────────

const PKGINFO: &str = "pkgname = hello\npkgbase = hello\npkgver = 1.0-1\npkgdesc = A greeting\nurl = https://example.com\nbuilddate = 1700000000\npackager = me\nsize = 4096\narch = x86_64\nlicense = MIT\ndepend = glibc\n";

/// Build a minimal pacman package (a gzip-compressed tar with a root `.PKGINFO`).
/// The parser detects the codec from the magic bytes, so `.pkg.tar.gz` is just as
/// valid as `.zst` and avoids pulling a zstd dev-dependency into the web tests.
fn make_test_pkg(pkginfo: &str) -> Vec<u8> {
    use std::io::Write;
    let mut tar_buf = Vec::new();
    {
        let mut tb = tar::Builder::new(&mut tar_buf);
        let mut header = tar::Header::new_gnu();
        header.set_path(".PKGINFO").unwrap();
        header.set_size(pkginfo.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tb.append(&header, pkginfo.as_bytes()).unwrap();
        tb.finish().unwrap();
    }
    let mut gz = Vec::new();
    {
        let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
        enc.write_all(&tar_buf).unwrap();
        enc.finish().unwrap();
    }
    gz
}

/// Gunzip + untar the repo DB and return the first `*/desc` entry's text.
fn db_desc_text(db: &[u8]) -> String {
    let mut decoder = flate2::read::GzDecoder::new(db);
    let mut tar_bytes = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut tar_bytes).unwrap();
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar_bytes));
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        if path.ends_with("/desc") {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut entry, &mut s).unwrap();
            return s;
        }
    }
    panic!("db tar has no */desc entry");
}

#[actix_web::test]
async fn pacman_publish_then_read_db_and_package() {
    let parts = local_registry_app_parts("arch", "pacman", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let pkg_bytes = make_test_pkg(PKGINFO);
    let publish = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/arch/pacman/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(pkg_bytes.clone())
            .to_request(),
    )
    .await;
    assert_eq!(publish.status(), 201);

    // The repo DB lists the package's desc with its filename.
    let db = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/arch/pacman/x86_64/arch.db")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(db.status(), 200);
    let desc = db_desc_text(&read_body(db).await);
    assert!(desc.contains("%NAME%\nhello\n"));
    assert!(desc.contains("%FILENAME%\nhello-1.0-1-x86_64.pkg.tar.gz\n"));
    // Unsigned repo: no PGPSIG embedded.
    assert!(!desc.contains("%PGPSIG%"));

    // The package is downloadable from its arch dir, byte-for-byte.
    let pkg = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/arch/pacman/x86_64/hello-1.0-1-x86_64.pkg.tar.gz")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(pkg.status(), 200);
    assert_eq!(read_body(pkg).await.to_vec(), pkg_bytes);

    // Unsigned repo: no detached DB signature.
    let sig = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/arch/pacman/x86_64/arch.db.sig")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(sig.status(), 404);
}

#[actix_web::test]
async fn pacman_publish_requires_authentication() {
    let parts = local_registry_app_parts("arch", "pacman", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/arch/pacman/upload")
            .set_payload(make_test_pkg(PKGINFO))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn pacman_publish_traversal_in_pkginfo_returns_400() {
    let parts = local_registry_app_parts("arch", "pacman", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let pkginfo = "pkgname = hello\npkgbase = hello\npkgver = ../../etc-1\npkgdesc = A greeting\nurl = https://example.com\nbuilddate = 1700000000\npackager = me\nsize = 4096\narch = x86_64\nlicense = MIT\ndepend = glibc\n";
    let resp = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/arch/pacman/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_test_pkg(pkginfo))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn pacman_signed_publish_emits_db_sig_pgpsig_and_key() {
    let seed = "9d61b19deffeba00aa3f3b6e3b0fe6a3f3a76b08e2c0a3f3b6e3b0fe6a3f3a76";
    let signer = Arc::new(
        batlehub_adapters::repo::OpenPgpSigner::from_seed_hex(seed, 1_700_000_000, "BatleHub")
            .unwrap(),
    );
    let mut sm = HashMap::new();
    sm.insert("arch".to_owned(), signer);

    let parts = local_registry_app_parts("arch", "pacman", RegistryMode::Local, None);
    let LocalRegistryAppParts {
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
    } = parts;
    let app = finish_test_app_with_extra(
        proxy_svc,
        admin_svc,
        token_repo,
        access_config,
        registry_map,
        local_svc,
        mode_map,
        batlehub_web::CargoIndexMap::default(),
        ConfigureAppDefaults::default(),
        RepoSignerMap::from(sm),
        test_auth_providers(),
    )
    .await;

    let publish = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/arch/pacman/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_test_pkg(PKGINFO))
            .to_request(),
    )
    .await;
    assert_eq!(publish.status(), 201);

    // Detached DB signature is present.
    let db_sig = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/arch/pacman/x86_64/arch.db.sig")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(db_sig.status(), 200);
    assert!(!read_body(db_sig).await.to_vec().is_empty());

    // The package has a sidecar detached signature.
    let pkg_sig = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/arch/pacman/x86_64/hello-1.0-1-x86_64.pkg.tar.gz.sig")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(pkg_sig.status(), 200);

    // The DB embeds the base64 %PGPSIG% field.
    let db = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/arch/pacman/x86_64/arch.db")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    let desc = db_desc_text(&read_body(db).await);
    assert!(desc.contains("%PGPSIG%"));

    // The public key is downloadable for pacman-key.
    let key = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/arch/pacman/key.gpg")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(key.status(), 200);
    let key_body = String::from_utf8(read_body(key).await.to_vec()).unwrap();
    assert!(key_body.contains("-----BEGIN PGP PUBLIC KEY BLOCK-----"));
}

#[actix_web::test]
async fn rpm_proxy_read_falls_through_to_upstream() {
    // Proxy mode: repodata is fetched from upstream (FixedRegistry returns bytes).
    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Proxy, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/repodata/repomd.xml")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn deb_local_missing_file_is_404() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/dists/stable/Release")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn deb_local_read_enforces_registry_rbac() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    // Replace the permissive default policy with one that denies anonymous
    // `releases:read` but allows users — proving local reads honor registry RBAC.
    {
        let mut hot = parts.proxy_svc.hot.write().await;
        let perms = HashMap::from([
            (Role::Anonymous, vec![]),
            (Role::User, vec!["releases:read".to_owned()]),
            (Role::Admin, vec!["*".to_owned()]),
        ]);
        hot.policies.insert(
            "apt".to_owned(),
            Arc::new(RegistryPolicy {
                metadata_ttl: None,
                firewall_only: false,
                serve_stale_metadata: false,
                artifact_ttl: None,
                rules: vec![Box::new(
                    RbacRule::from_patterns(perms).expect("fixture rbac patterns are valid"),
                )],
            }),
        );
    }
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    // Publish as a user (publish only requires authentication, not releases:read).
    let publish = call_service(
        &app,
        TestRequest::put()
            .uri("/proxy/apt/deb/pool/stable/main/upload")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .set_payload(make_test_deb(HELLO_CONTROL))
            .to_request(),
    )
    .await;
    assert_eq!(publish.status(), 201);

    // Anonymous read of a published index file is denied (403), not served.
    let anon = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/dists/stable/Release")
            .to_request(),
    )
    .await;
    assert_eq!(anon.status(), 403);

    // The same read succeeds for an authenticated user.
    let user = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/apt/deb/dists/stable/Release")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    assert_eq!(user.status(), 200);
}

#[actix_web::test]
async fn jetbrains_proxy_get_streams_upstream_by_path() {
    let app = make_app(InMemoryRepo::new()).await;
    // Anonymous GET of an IDE-archive path proxies to upstream (200). The whole
    // path after /jetbrains/ is carried in the synthetic `repo` coordinate's
    // artifact, so FixedRegistry echoes it back in the streamed body.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/jb/jetbrains/idea/idea-2026.1.3.tar.gz")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body = String::from_utf8(read_body(resp).await.to_vec()).unwrap();
    assert!(body.starts_with("artifact:jetbrains:"));
    assert!(body.contains("jb/repo/_/idea/idea-2026.1.3.tar.gz"));
}

#[actix_web::test]
async fn jetbrains_get_on_non_jetbrains_registry_is_404() {
    let app = make_app(InMemoryRepo::new()).await;
    // `npm` exists but is not a jetbrains registry → require_registry_type 404s.
    let resp = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/npm/jetbrains/idea/x.tar.gz")
            .to_request(),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn proxy_npm_packument_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_npm_version_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_npm_tarball_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/npm/lodash/4.17.21/tarball")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_cargo_crate_is_accessible_anonymously() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get().uri("/proxy/cargo/serde").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn proxy_cargo_download_blocked_for_anonymous() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get()
        .uri("/proxy/cargo/serde/1.0.0/download")
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 403);
}

#[actix_web::test]
async fn proxy_response_contains_artifact_bytes() {
    let app = make_app(InMemoryRepo::new()).await;
    let req = TestRequest::get().uri("/proxy/npm/lodash").to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
    let body = actix_web::test::read_body(resp).await;
    // FixedRegistry embeds the package key in the artifact
    assert!(std::str::from_utf8(&body).unwrap().contains("lodash"));
}

// ── Repository layout injectivity (found by `fuzz_repo_publish` /
//    `fuzz_repo_index_render`) ──────────────────────────────────────────────

async fn put_status<S, B>(app: &S, uri: &str, token: &str, body: Vec<u8>) -> u16
where
    S: actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse<B>,
        Error = actix_web::Error,
    >,
{
    call_service(
        app,
        TestRequest::put()
            .uri(uri)
            .insert_header(("Authorization", bearer(token)))
            .set_payload(body)
            .to_request(),
    )
    .await
    .status()
    .as_u16()
}

/// A component of `main/x` with package `x` and a component of `main` with
/// package `x/x` were the same pool file; a `/` in a component also re-cut the
/// sidecar key `regenerate_deb` groups on.
#[actix_web::test]
async fn deb_publish_refuses_a_slash_in_component_distribution_or_name() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;

    let x = make_test_deb("Package: x\nVersion: 1\nArchitecture: amd64\n");
    for uri in [
        "/proxy/apt/deb/pool/stable/main%2Fx/upload",
        "/proxy/apt/deb/pool/stable%2Fupdates/main/upload",
    ] {
        assert_eq!(
            put_status(&app, uri, USER_TOKEN, x.clone()).await,
            400,
            "{uri}"
        );
    }
    let nested = make_test_deb("Package: x/x\nVersion: 1\nArchitecture: amd64\n");
    let uri = "/proxy/apt/deb/pool/stable/main/upload";
    assert_eq!(put_status(&app, uri, USER_TOKEN, nested).await, 400);
    let underscore = make_test_deb("Package: a\nVersion: 1_2\nArchitecture: amd64\n");
    assert_eq!(put_status(&app, uri, USER_TOKEN, underscore).await, 400);
}

/// A repeated field let `name()` authorize one `Package` while the index
/// advertised the other; an empty field name rendered a line apt cannot read.
#[actix_web::test]
async fn deb_publish_refuses_a_control_file_apt_would_misread() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let uri = "/proxy/apt/deb/pool/stable/main/upload";
    for control in [
        "Package: mine\nVersion: 1\nPackage: theirs\n",
        "Package: a\nVersion: 1\n: x\n",
    ] {
        let status = put_status(&app, uri, USER_TOKEN, make_test_deb(control)).await;
        assert_eq!(status, 400, "{control:?}");
    }
}

/// Debian drops the epoch from pool file names, so `1:1.0` would land on
/// `1.0`'s file under a second index entry with a second checksum. The archive
/// refuses the second upload; so does this.
#[actix_web::test]
async fn deb_publish_refuses_a_second_version_on_the_same_pool_file() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let uri = "/proxy/apt/deb/pool/stable/main/upload";
    let plain = make_test_deb("Package: hello\nVersion: 1.0\nArchitecture: amd64\n");
    assert_eq!(put_status(&app, uri, ADMIN_TOKEN, plain).await, 201);
    let epoch = make_test_deb("Package: hello\nVersion: 1:1.0\nArchitecture: amd64\nX: y\n");
    assert_eq!(put_status(&app, uri, ADMIN_TOKEN, epoch).await, 409);
    // The control: another version is a different file and is accepted.
    let next = make_test_deb("Package: hello\nVersion: 1:1.1\nArchitecture: amd64\n");
    assert_eq!(put_status(&app, uri, ADMIN_TOKEN, next).await, 201);
}

/// The pool is shared by every suite, and each suite's stanza records the
/// file's checksum: a rebuilt `1.0` sent to a second suite would have replaced
/// the file under the first suite's index. Identical bytes (a promotion) pass.
#[actix_web::test]
async fn deb_publish_refuses_other_bytes_for_a_file_another_suite_lists() {
    let parts = local_registry_app_parts("apt", "deb", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let control = "Package: hello\nVersion: 1.0\nArchitecture: amd64\n";
    let first = make_test_deb(control);
    let stable = "/proxy/apt/deb/pool/stable/main/upload";
    let testing = "/proxy/apt/deb/pool/testing/main/upload";
    assert_eq!(
        put_status(&app, stable, ADMIN_TOKEN, first.clone()).await,
        201
    );
    assert_eq!(put_status(&app, testing, ADMIN_TOKEN, first).await, 201);
    let rebuilt = make_test_deb(&format!("{control}X: rebuilt\n"));
    assert_eq!(put_status(&app, testing, ADMIN_TOKEN, rebuilt).await, 409);
}

/// `(a-b, 1-1)` and `(a, b-1-1)` were the same `a-b-1-1-x86_64` file.
#[actix_web::test]
async fn pacman_publish_refuses_a_version_without_exactly_one_dash() {
    let parts = local_registry_app_parts("arch", "pacman", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let pkg = |name: &str, ver: &str| {
        make_test_pkg(&format!(
            "pkgname = {name}\npkgver = {ver}\narch = x86_64\n"
        ))
    };
    let uri = "/proxy/arch/pacman/upload";
    assert_eq!(
        put_status(&app, uri, USER_TOKEN, pkg("a-b", "1-1")).await,
        201
    );
    assert_eq!(
        put_status(&app, uri, USER_TOKEN, pkg("a", "b-1-1")).await,
        400
    );
    assert_eq!(
        put_status(&app, uri, USER_TOKEN, pkg("a/b", "1-1")).await,
        400
    );
}

fn rpm_built(name: &str, epoch: u32, summary: &str) -> Vec<u8> {
    let pkg = rpm::PackageBuilder::new(name, "1.0", "MIT", "x86_64", summary)
        .epoch(epoch)
        .build()
        .unwrap();
    let mut buf = Vec::new();
    pkg.write(&mut buf).unwrap();
    buf
}

/// `storage_id` prefixes a non-zero epoch, so `(389, ds-base)` and
/// `(0, 389-ds-base)` — a real Fedora name — were one file.
#[actix_web::test]
async fn rpm_publish_refuses_an_epoch_that_collides_with_a_name() {
    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let uri = "/proxy/yum/rpm/upload";
    assert_eq!(
        put_status(&app, uri, ADMIN_TOKEN, rpm_built("ds-base", 389, "s")).await,
        201
    );
    assert_eq!(
        put_status(&app, uri, ADMIN_TOKEN, rpm_built("389-ds-base", 0, "s")).await,
        409
    );
}

/// One control character in one summary made `primary.xml` unparseable for
/// the whole repository.
#[actix_web::test]
async fn rpm_repodata_stays_well_formed_with_a_control_character_in_a_summary() {
    let parts = local_registry_app_parts("yum", "rpm", RegistryMode::Local, None);
    let app = build_local_registry_app(parts, batlehub_web::CargoIndexMap::default(), None).await;
    let uri = "/proxy/yum/rpm/upload";
    // The builder refuses a control character, so patch one into the header
    // the way a hand-crafted upload would (same length, same tag bookkeeping).
    let mut bytes = rpm_built("hello", 0, "sumZsum");
    let at = bytes
        .windows(7)
        .position(|w| w == b"sumZsum")
        .expect("summary in header");
    bytes[at + 3] = 0x01;
    assert_eq!(put_status(&app, uri, USER_TOKEN, bytes).await, 201);

    let primary = call_service(
        &app,
        TestRequest::get()
            .uri("/proxy/yum/rpm/repodata/primary.xml.gz")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .to_request(),
    )
    .await;
    let gz = read_body(primary).await.to_vec();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&gz[..]), &mut xml).unwrap();
    assert!(
        !xml.contains('\u{1}'),
        "a raw control character reached primary.xml"
    );
    assert!(xml.contains("<summary>sum\u{fffd}sum</summary>"), "{xml}");
}
