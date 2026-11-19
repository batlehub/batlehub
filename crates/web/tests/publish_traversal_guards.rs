//! Integration tests split from the former monolithic `integration.rs`
//! (see `tests/common/mod.rs` for shared app-factory infrastructure).

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, TestRequest};

use batlehub_config::schema::RegistryMode;

// ── pypi publish traversal ─────────────────────────────────────────────────────

async fn make_local_pypi_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-pypi", "pypi", mode, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

/// Build a `twine upload`-style `multipart/form-data` body for `pypi_publish`,
/// with the `content` part's filename chosen independently of the coordinate —
/// which is what a hostile client does.
fn make_pypi_publish_body_named(name: &str, version: &str, filename: &str) -> (Vec<u8>, String) {
    let boundary = "pypiboundary";
    let mut body = Vec::new();
    for (field_name, value) in [
        (":action", "file_upload"),
        ("name", name),
        ("version", version),
    ] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field_name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"content\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"fake-pypi-sdist-content");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

/// Build a `twine upload`-style `multipart/form-data` body for `pypi_publish`.
fn make_pypi_publish_body(name: &str, version: &str) -> (Vec<u8>, String) {
    let boundary = "pypiboundary";
    let mut body = Vec::new();
    for (field_name, value) in [
        (":action", "file_upload"),
        ("name", name),
        ("version", version),
    ] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field_name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"content\"; filename=\"{name}-{version}.tar.gz\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"fake-pypi-sdist-content");
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

#[actix_web::test]
async fn pypi_publish_traversal_version_returns_400() {
    let app = make_local_pypi_app(RegistryMode::Local).await;
    let (body, content_type) = make_pypi_publish_body("my-pkg", "../../etc/x");
    let req = TestRequest::post()
        .uri("/proxy/local-pypi/legacy/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}

/// The uploaded filename is stored in `index_metadata` and read back by the
/// Simple index, which is `text/html` on the console's own origin. `name` and
/// `version` are validated; the filename was not, and it is the only one of the
/// three an attacker can fill with markup while keeping a valid coordinate.
#[actix_web::test]
async fn pypi_publish_markup_filename_returns_400() {
    let app = make_local_pypi_app(RegistryMode::Local).await;
    for filename in [
        // Parses as a distribution — `<img …>` then `1.0.0` — so only the
        // character set rejects it.
        "<img src=x onerror=alert(1)>-1.0.0.tar.gz",
        // The link-injection half: a second anchor pointing off-host.
        "evil-1.0.tar.gz</a><a href=https://attacker.tld/backdoor.whl>backdoor<a x=",
        "../../etc/passwd-1.0.tar.gz",
        "not-a-distribution.exe",
    ] {
        let (body, content_type) = make_pypi_publish_body_named("my-pkg", "1.0.0", filename);
        let req = TestRequest::post()
            .uri("/proxy/local-pypi/legacy/")
            .insert_header(("Authorization", bearer(USER_TOKEN)))
            .insert_header(("Content-Type", content_type))
            .set_payload(body)
            .to_request();
        assert_eq!(
            call_service(&app, req).await.status(),
            400,
            "accepted a hostile distribution filename: {filename}"
        );
    }
}

/// The guard above must not cost a real `twine upload` its publish.
#[actix_web::test]
async fn pypi_publish_accepts_a_wheel_filename() {
    let app = make_local_pypi_app(RegistryMode::Local).await;
    let (body, content_type) =
        make_pypi_publish_body_named("my-pkg", "1.0.0", "my_pkg-1.0.0-py3-none-any.whl");
    let req = TestRequest::post()
        .uri("/proxy/local-pypi/legacy/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 200);
}

/// The other half of the primitive: the package *name* reaches `<title>` and
/// `<h1>`, and PEP 503 normalisation removes no markup. A name outside PEP 508's
/// character set is now refused at publish, so it is never stored to be
/// rendered (the page also escapes it — `get_pypi_simple_page`).
#[actix_web::test]
async fn pypi_publish_of_a_markup_package_name_is_refused() {
    let app = make_local_pypi_app(RegistryMode::Local).await;

    let hostile = "evil<script>alert(1)<script>";
    let (body, content_type) = make_pypi_publish_body_named(hostile, "1.0.0", "evil-1.0.0.tar.gz");
    let publish = TestRequest::post()
        .uri("/proxy/local-pypi/legacy/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    assert_eq!(call_service(&app, publish).await.status(), 400);
}

/// twine sends the project name as declared (`My_Pkg`), but pip asks for the
/// PEP 503 form (`my-pkg`): the publish must store it under the latter.
#[actix_web::test]
async fn pypi_publish_of_a_non_canonical_name_is_installable() {
    let app = make_local_pypi_app(RegistryMode::Local).await;
    let (body, content_type) =
        make_pypi_publish_body_named("My_Pkg", "1.0.0", "my_pkg-1.0.0.tar.gz");
    let publish = TestRequest::post()
        .uri("/proxy/local-pypi/legacy/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
        .to_request();
    assert_eq!(call_service(&app, publish).await.status(), 200);

    let simple = TestRequest::get()
        .uri("/proxy/local-pypi/simple/my-pkg/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, simple).await;
    assert_eq!(resp.status(), 200);
    let html = String::from_utf8_lossy(&actix_web::test::read_body(resp).await).into_owned();
    assert!(html.contains("my_pkg-1.0.0.tar.gz"), "{html}");

    let file = TestRequest::get()
        .uri("/proxy/local-pypi/packages/my_pkg-1.0.0.tar.gz")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, file).await.status(), 200);
}

fn pypi_upload(name: &str, version: &str, filename: &str, content: &[u8]) -> TestRequest {
    let (mut body, content_type) = make_pypi_publish_body_named(name, version, filename);
    let fake: &[u8] = b"fake-pypi-sdist-content";
    let at = body
        .windows(fake.len())
        .position(|w| w == fake)
        .expect("helper body carries the fake content");
    body.splice(at..at + fake.len(), content.iter().copied());
    TestRequest::post()
        .uri("/proxy/local-pypi/legacy/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .insert_header(("Content-Type", content_type))
        .set_payload(body)
}

async fn pypi_get(
    app: &impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    uri: &str,
) -> (u16, Vec<u8>) {
    let req = TestRequest::get()
        .uri(uri)
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(app, req).await;
    let status = resp.status().as_u16();
    (status, actix_web::test::read_body(resp).await.to_vec())
}

/// `twine upload dist/*` sends the sdist then the wheel of one version: both
/// are files of that version, both listed, each downloadable as itself.
#[actix_web::test]
async fn pypi_sdist_then_wheel_are_both_published_and_served() {
    let app = make_local_pypi_app(RegistryMode::Local).await;
    let sdist = "two_files-1.0.0.tar.gz";
    let wheel = "two_files-1.0.0-py3-none-any.whl";
    let up = |f: &str, c: &[u8]| pypi_upload("two_files", "1.0.0", f, c).to_request();
    assert_eq!(call_service(&app, up(sdist, b"SDIST")).await.status(), 200);
    assert_eq!(call_service(&app, up(wheel, b"WHEEL")).await.status(), 200);

    let (status, html) = pypi_get(&app, "/proxy/local-pypi/simple/two-files/").await;
    assert_eq!(status, 200);
    let html = String::from_utf8_lossy(&html);
    assert!(html.contains(sdist) && html.contains(wheel), "{html}");
    use sha2::Digest as _;
    let wheel_sha = hex::encode(sha2::Sha256::digest(b"WHEEL"));
    assert!(html.contains(&wheel_sha), "the wheel's own digest: {html}");

    assert_eq!(
        pypi_get(&app, &format!("/proxy/local-pypi/packages/{sdist}")).await,
        (200, b"SDIST".to_vec())
    );
    assert_eq!(
        pypi_get(&app, &format!("/proxy/local-pypi/packages/{wheel}")).await,
        (200, b"WHEEL".to_vec())
    );
    // A file the version does not have is not answered with another file's bytes.
    let (status, _) = pypi_get(
        &app,
        "/proxy/local-pypi/packages/two_files-1.0.0-cp311-cp311-linux_x86_64.whl",
    )
    .await;
    assert_eq!(status, 404);
}

/// PyPI files are immutable: the same filename twice is a conflict, whether it
/// is the version's first file or a later one.
#[actix_web::test]
async fn pypi_same_filename_twice_is_409() {
    let app = make_local_pypi_app(RegistryMode::Local).await;
    let sdist = "dup_file-1.0.0.tar.gz";
    let wheel = "dup_file-1.0.0-py3-none-any.whl";
    let up = |f: &str| pypi_upload("dup_file", "1.0.0", f, b"x").to_request();
    assert_eq!(call_service(&app, up(sdist)).await.status(), 200);
    assert_eq!(call_service(&app, up(wheel)).await.status(), 200);
    assert_eq!(call_service(&app, up(sdist)).await.status(), 409);
    assert_eq!(call_service(&app, up(wheel)).await.status(), 409);
}

// ── conda publish traversal ─────────────────────────────────────────────────────

/// Minimal conda `.tar.bz2` package: a bzip2-compressed tar containing
/// `info/index.json`.
fn make_conda_tar_bz2(name: &str, version: &str) -> Vec<u8> {
    use bzip2::write::BzEncoder;
    use bzip2::Compression;
    use std::io::Write as _;

    let index_json = serde_json::json!({
        "name": name,
        "version": version,
        "build": "0",
        "build_number": 0,
        "depends": [],
        "subdir": "linux-64",
    });
    let index_bytes = serde_json::to_vec(&index_json).unwrap();

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

async fn make_local_conda_app(
    mode: RegistryMode,
) -> impl actix_web::dev::Service<
    actix_http::Request,
    Response = actix_web::dev::ServiceResponse<actix_web::body::BoxBody>,
    Error = actix_web::Error,
> {
    build_local_registry_app(
        local_registry_app_parts("local-conda", "conda", mode, None),
        batlehub_web::CargoIndexMap::default(),
        None,
    )
    .await
}

/// A publish must be visible in the compressed channel, not only the plain one.
///
/// `repodata.json.zst` is cached under a key built from the *blocked-set*
/// fingerprint, which a publish does not change — so a client that had probed
/// the channel once kept being served the pre-publish bytes indefinitely, while
/// `repodata.json`, regenerated per request, showed the new package. The two
/// encodings described different channels, and micromamba asks for this one
/// first: measured with micromamba 2.9.0, a just-published package was
/// unresolvable (RFC 0009 §12.13).
///
/// The warm-up read is the whole test. Without it there is nothing cached to be
/// stale, and this passes against the bug.
#[actix_web::test]
async fn a_publish_is_visible_in_the_compressed_channel() {
    let app = make_local_conda_app(RegistryMode::Local).await;

    let warm = TestRequest::get()
        .uri("/proxy/local-conda/linux-64/repodata.json.zst")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    assert_eq!(call_service(&app, warm).await.status(), 200);

    let publish = TestRequest::post()
        .uri("/proxy/local-conda/linux-64/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_conda_tar_bz2("freshpkg", "1.0.0"))
        .to_request();
    assert_eq!(call_service(&app, publish).await.status(), 200);

    let read = TestRequest::get()
        .uri("/proxy/local-conda/linux-64/repodata.json.zst")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .to_request();
    let resp = call_service(&app, read).await;
    assert_eq!(resp.status(), 200);
    let compressed = actix_web::test::read_body(resp).await.to_vec();
    let raw = zstd::decode_all(compressed.as_slice()).expect("valid zstd");
    let channel = String::from_utf8_lossy(&raw);
    assert!(
        channel.contains("freshpkg"),
        "the compressed channel is serving pre-publish bytes: {channel}"
    );
}

#[actix_web::test]
async fn conda_publish_traversal_version_returns_400() {
    let app = make_local_conda_app(RegistryMode::Local).await;
    let req = TestRequest::post()
        .uri("/proxy/local-conda/linux-64/")
        .insert_header(("Authorization", bearer(USER_TOKEN)))
        .set_payload(make_conda_tar_bz2("my-pkg", "../../etc/x"))
        .to_request();
    assert_eq!(call_service(&app, req).await.status(), 400);
}
