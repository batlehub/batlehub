//! Ansible Galaxy as a registry kind (RFC 0031).
//!
//! What `ansible-galaxy` does, and what this proxy therefore has to get right:
//!
//! - it resolves **every** candidate through
//!   `v3/collections/{ns}/{name}/versions/` — a pinned requirement included —
//!   and picks from the list it gets back, so a blocked version must be absent
//!   from it or the resolver selects something the download then refuses;
//! - it hashes the tarball as it streams and compares the digest with
//!   `artifact.sha256` from the version document, so the bytes have to be
//!   byte-exact and that field has to be relayed unedited;
//! - it `urljoin`s any pagination link against the configured api_server, and
//!   an absolute-path link replaces the whole path — so every listing served
//!   here carries a null `next`, and nothing else;
//! - it re-reads the collection document on every resolve, uncached, and drops
//!   its day-old copy of the versions list when `updated_at` moves — which is
//!   why a block bumps that field.
//!
//! The fixture is `FixedRegistry`'s three versions (`1.0.0`, `1.1.0` and the
//! pre-release `2.0.0-beta.1`), in the v3 shape; see `tests/common/mod.rs`.

mod common;
#[allow(unused_imports)]
use common::*;

use actix_web::test::{call_service, TestRequest};
use batlehub_config::schema::RegistryMode;

const REG: &str = "galaxy";
const COLLECTION: &str = "acme.util";

async fn app() -> impl TestService {
    proxy_registry_app(REG, "galaxy").await
}

async fn local_app() -> impl TestService {
    registry_app(REG, "galaxy", RegistryMode::Local).await
}

fn api(path: &str) -> String {
    format!("/proxy/{REG}/galaxy/api/{path}")
}

fn versions_url() -> String {
    api("v3/collections/acme/util/versions/")
}

fn collection_url() -> String {
    api("v3/collections/acme/util/")
}

fn version_url(v: &str) -> String {
    api(&format!("v3/collections/acme/util/versions/{v}/"))
}

fn artifact_url(v: &str) -> String {
    api(&format!("v3/artifacts/collections/acme-util-{v}.tar.gz"))
}

/// The versions a listing document names, in document order.
fn versions_in(doc: &serde_json::Value) -> Vec<String> {
    doc.get("data")
        .or_else(|| doc.get("results"))
        .and_then(|d| d.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("version")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

// ── the discovery document ───────────────────────────────────────────────────

#[actix_web::test]
async fn discovery_advertises_both_api_versions() {
    let app = app().await;
    let doc = get_json(&app, &api("")).await;
    assert_eq!(doc["available_versions"]["v3"], "v3/");
    assert_eq!(
        doc["available_versions"]["v1"], "v1/",
        "roles default to `proxy`, so v1 is advertised"
    );
}

// ── the listing is the enforcement point ─────────────────────────────────────

#[actix_web::test]
async fn the_versions_list_is_one_page_with_every_link_null() {
    let app = app().await;
    let doc = get_json(&app, &versions_url()).await;
    assert_eq!(versions_in(&doc), ["1.0.0", "1.1.0", "2.0.0-beta.1"]);
    assert_eq!(doc["meta"]["count"], 3);
    for link in ["first", "previous", "next", "last"] {
        assert!(
            doc["links"][link].is_null(),
            "links.{link} must be null: no continuation this instance emits survives the \
             client's own urljoin under a path prefix"
        );
    }
}

#[actix_web::test]
async fn a_blocked_version_is_absent_from_the_versions_list() {
    let app = app().await;
    block_version(&app, REG, COLLECTION, "1.1.0").await;

    let doc = get_json(&app, &versions_url()).await;
    assert_eq!(versions_in(&doc), ["1.0.0", "2.0.0-beta.1"]);
    assert_eq!(
        doc["meta"]["count"], 2,
        "meta.count has to describe the document served, not the one upstream sent"
    );
}

#[actix_web::test]
async fn a_blocked_version_answers_404_on_its_version_document() {
    let app = app().await;
    block_version(&app, REG, COLLECTION, "1.1.0").await;

    let resp = call_service(&app, admin_get(&version_url("1.1.0"))).await;
    assert_eq!(
        resp.status(),
        404,
        "a pin on a blocked version gets ansible's own unsatisfiable-requirements error \
         rather than a download"
    );
    // Its neighbours are untouched.
    let resp = call_service(&app, admin_get(&version_url("1.0.0"))).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn a_blocked_version_is_refused_at_the_tarball() {
    let app = app().await;
    block_version(&app, REG, COLLECTION, "1.1.0").await;

    let resp = call_service(&app, admin_get(&artifact_url("1.1.0"))).await;
    assert_eq!(
        resp.status(),
        403,
        "the last line: a client holding a version document fetched before the block is \
         still refused the bytes"
    );
}

// ── the collection document ──────────────────────────────────────────────────

#[actix_web::test]
async fn the_collection_document_points_at_this_instance() {
    let app = app().await;
    let doc = get_json(&app, &collection_url()).await;
    let href = doc["href"].as_str().unwrap_or_default();
    let versions = doc["versions_url"].as_str().unwrap_or_default();
    assert!(
        href.contains("/proxy/galaxy/galaxy/api/v3/collections/acme/util/"),
        "href must name this instance, got {href:?}"
    );
    assert!(
        versions.ends_with("/proxy/galaxy/galaxy/api/v3/collections/acme/util/versions/"),
        "versions_url must name this instance, got {versions:?}"
    );
    assert!(!href.contains("upstream.invalid"));
}

#[actix_web::test]
async fn blocking_the_highest_version_moves_it_and_bumps_updated_at() {
    let app = app().await;
    let before = get_json(&app, &collection_url()).await;
    assert_eq!(before["highest_version"]["version"], "1.1.0");
    let upstream_updated = before["updated_at"].as_str().unwrap().to_owned();

    block_version(&app, REG, COLLECTION, "1.1.0").await;

    let after = get_json(&app, &collection_url()).await;
    assert_eq!(
        after["highest_version"]["version"], "1.0.0",
        "the newest *stable* survivor; 2.0.0-beta.1 is a pre-release"
    );
    let bumped = after["updated_at"].as_str().unwrap();
    assert!(
        bumped > upstream_updated.as_str(),
        "updated_at must move past upstream's ({upstream_updated}) so the client drops its \
         cached listing, got {bumped}"
    );
}

#[actix_web::test]
async fn the_highest_version_href_moves_with_the_version_it_names() {
    let app = app().await;
    block_version(&app, REG, COLLECTION, "1.1.0").await;
    let doc = get_json(&app, &collection_url()).await;
    let href = doc["highest_version"]["href"].as_str().unwrap_or_default();
    assert!(
        href.ends_with("/versions/1.0.0/"),
        "a href still naming the blocked version would send the resolver to a 404, got {href:?}"
    );
}

// ── the version document: three URLs rewritten, nothing else touched ─────────

#[actix_web::test]
async fn the_version_document_rewrites_exactly_three_urls() {
    let app = app().await;
    let doc = get_json(&app, &version_url("1.0.0")).await;

    let download = doc["download_url"].as_str().unwrap();
    assert_eq!(
        download,
        "http://localhost:8080/proxy/galaxy/galaxy/api/v3/artifacts/collections/acme-util-1.0.0.tar.gz",
        "download_url has to be absolute and end in the upstream filename: `_download_file` \
         names the file it writes by slicing `.tar.gz` off the last path segment"
    );
    assert!(doc["href"]
        .as_str()
        .unwrap()
        .contains("/proxy/galaxy/galaxy/"));
    assert!(doc["collection"]["href"]
        .as_str()
        .unwrap()
        .contains("/proxy/galaxy/galaxy/"));

    // Everything the client verifies is relayed as received.
    assert_eq!(
        doc["artifact"]["sha256"],
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "the digest the client hashes the body against must not be rewritten"
    );
    assert_eq!(doc["artifact"]["filename"], "acme-util-1.0.0.tar.gz");
    assert_eq!(doc["artifact"]["size"], 2866617);
    assert!(doc["signatures"].is_array());
    assert_eq!(doc["created_at"], "2020-01-02T00:00:00Z");
}

// ── the artifact ─────────────────────────────────────────────────────────────

#[actix_web::test]
async fn the_tarball_is_served_as_gzip_under_the_collection_coordinate() {
    let app = app().await;
    assert_content_type(&app, &artifact_url("1.0.0"), "application/gzip").await;
    let body = get_bytes(&app, &artifact_url("1.0.0")).await;
    let body = String::from_utf8(body).unwrap();
    assert!(
        body.contains("galaxy/acme.util/1.0.0/tarball"),
        "the cache key is the dotted package name and the `tarball` sub-coordinate, got {body}"
    );
}

// ── validation at the edge ───────────────────────────────────────────────────

#[actix_web::test]
async fn galaxy_traversal_namespace_returns_400() {
    let app = app().await;
    for uri in [
        api("v3/collections/..%2f..%2fetc/util/versions/"),
        api("v3/collections/Acme/util/versions/"),
        api("v3/collections/acme/Util/"),
    ] {
        let resp = call_service(&app, admin_get(&uri)).await;
        assert!(
            resp.status() == 400 || resp.status() == 404,
            "{uri} should be refused at the edge, got {}",
            resp.status()
        );
    }
}

#[actix_web::test]
async fn galaxy_traversal_version_returns_400() {
    let app = app().await;
    let resp = call_service(
        &app,
        admin_get(&api("v3/collections/acme/util/versions/..%2f..%2fetc%2fx/")),
    )
    .await;
    assert!(
        resp.status() == 400 || resp.status() == 404,
        "a version that walks must never reach a storage key, got {}",
        resp.status()
    );
}

#[actix_web::test]
async fn an_artifact_filename_that_disagrees_with_itself_returns_400() {
    let app = app().await;
    for file in [
        "acme-util.tar.gz",       // no version
        "acme-util-1.0.0.zip",    // not a tarball
        "Acme-Util-1.0.0.tar.gz", // not a galaxy name
    ] {
        let resp = call_service(
            &app,
            admin_get(&api(&format!("v3/artifacts/collections/{file}"))),
        )
        .await;
        assert_eq!(
            resp.status(),
            400,
            "{file} is not a collection artifact name"
        );
    }
}

// ── publish, and the import task ─────────────────────────────────────────────

/// A two-file collection tarball: `MANIFEST.json` and `FILES.json`, gzipped
/// tar, exactly what `ansible-galaxy collection build` produces.
fn build_collection(namespace: &str, name: &str, version: &str) -> Vec<u8> {
    build_collection_with(namespace, name, version, serde_json::json!({}))
}

/// [`build_collection`] with a `dependencies` map.
///
/// The resolver builds its graph from `metadata.dependencies` of the *version
/// document* (`get_collection_version_metadata`, api.py), so a publish that
/// declares dependencies and a document that does not is a collection which
/// installs alone and then fails at run time.
fn build_collection_with(
    namespace: &str,
    name: &str,
    version: &str,
    dependencies: serde_json::Value,
) -> Vec<u8> {
    use std::io::Write;

    let manifest = serde_json::json!({
        "collection_info": {
            "namespace": namespace,
            "name": name,
            "version": version,
            "dependencies": dependencies,
            "tags": ["utility"],
            "license": ["MIT"],
            "readme": "README.md",
            "authors": ["acme"],
        },
        "format": 1,
    });
    let files = serde_json::json!({
        "files": [{ "name": ".", "ftype": "dir" }],
        "format": 1,
    });

    let mut tar = tar::Builder::new(Vec::new());
    for (path, body) in [
        ("MANIFEST.json", manifest.to_string()),
        ("FILES.json", files.to_string()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, body.as_bytes()).unwrap();
    }
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&raw).unwrap();
    gz.finish().unwrap()
}

/// The publish body **as `ansible-galaxy` builds it**: the file part base64
/// encoded, under `Content-Transfer-Encoding: base64`.
///
/// That is `prepare_multipart`'s default encoder for a part read from a file
/// (`email.encoders.encode_base64`), and `publish_collection` does not override
/// it. A plain-binary fixture passes against a server that cannot read the real
/// thing — which is exactly what shipped until `tests/heavy/galaxy.sh` ran.
fn multipart_publish(
    uri: &str,
    filename: &str,
    sha256: &str,
    tarball: &[u8],
) -> actix_http::Request {
    multipart_publish_with(uri, filename, sha256, tarball, true)
}

fn multipart_publish_with(
    uri: &str,
    filename: &str,
    sha256: &str,
    tarball: &[u8],
    base64_encoded: bool,
) -> actix_http::Request {
    publish_request(
        uri,
        filename,
        Some(sha256),
        tarball,
        base64_encoded,
        Some(ADMIN_TOKEN),
    )
}

/// The publish request, with the two things a caller may want to leave out: the
/// `sha256` field (optional in the protocol — the server hashes the bytes
/// either way) and the credential (a publish is a write, and an anonymous one
/// has to be refused before anything is stored).
fn publish_request(
    uri: &str,
    filename: &str,
    sha256: Option<&str>,
    tarball: &[u8],
    base64_encoded: bool,
    token: Option<&str>,
) -> actix_http::Request {
    use base64::Engine as _;
    const BOUNDARY: &str = "----batlehubgalaxy";
    let mut body: Vec<u8> = Vec::new();
    if let Some(sha256) = sha256 {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; \
                 name=\"sha256\"\r\n\r\n{sha256}\r\n"
            )
            .as_bytes(),
        );
    }
    let encoding = if base64_encoded {
        "Content-Transfer-Encoding: base64\r\n"
    } else {
        ""
    };
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\n{encoding}Content-Disposition: form-data; name=\"file\"; \
             filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    if base64_encoded {
        // Wrapped at 76 columns with CRLF, as the encoder that produced it does.
        let encoded = base64::engine::general_purpose::STANDARD.encode(tarball);
        for line in encoded.as_bytes().chunks(76) {
            body.extend_from_slice(line);
            body.extend_from_slice(b"\r\n");
        }
    } else {
        body.extend_from_slice(tarball);
    }
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let mut req = TestRequest::post().uri(uri).insert_header((
        "Content-Type",
        format!("multipart/form-data; boundary={BOUNDARY}"),
    ));
    if let Some(token) = token {
        req = req.insert_header(("Authorization", bearer(token)));
    }
    req.set_payload(body).to_request()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[actix_web::test]
async fn a_publish_is_installable_straight_afterwards() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball);

    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
        ),
    )
    .await;
    assert_eq!(resp.status(), 202, "publish should be accepted");
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    let task = body["task"].as_str().expect("a task id").to_owned();
    // **A bare id, not a URL and not a path.** RFC 0031 §4.4 specified an
    // absolute path, reasoning about `urljoin(api_server, resp["task"])`. The
    // v3 client does not do that: `wait_import_task` interpolates the value as
    // a single path *segment* —
    // `_urljoin(api_server, v3, "imports/collections", task_id, "/")` — so a
    // path here lands in the middle of the polled URL, and the client spends
    // its whole timeout there because it reads each `404` as "the import has
    // not started yet". Measured against ansible-core 2.19.3.
    assert!(
        !task.is_empty() && !task.contains('/'),
        "the task must be a bare id: {task:?}"
    );

    // The poll the client makes next, on the route it builds itself:
    // `imports/collections/{task_id}/`, not `imports/tasks/`.
    let status = get_json(&app, &api(&format!("v3/imports/collections/{task}/"))).await;
    assert_eq!(status["state"], "completed");
    assert!(status["finished_at"].is_string());

    // …and the collection is installable, with the digest the client will check.
    let listing = get_json(&app, &versions_url()).await;
    assert_eq!(versions_in(&listing), ["1.0.0"]);
    let detail = get_json(&app, &version_url("1.0.0")).await;
    assert_eq!(detail["artifact"]["sha256"], digest);
    assert_eq!(detail["artifact"]["filename"], "acme-util-1.0.0.tar.gz");
    let bytes = get_bytes(&app, &artifact_url("1.0.0")).await;
    assert_eq!(sha256_hex(&bytes), digest, "the bytes must be byte-exact");
}

#[actix_web::test]
async fn a_publish_whose_sha256_disagrees_with_its_bytes_is_400() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &"0".repeat(64),
            &tarball,
        ),
    )
    .await;
    assert_eq!(
        resp.status(),
        400,
        "a mismatched pair must be refused before anything is stored: the client hashes \
         what it downloads and would fail every install"
    );
}

#[actix_web::test]
async fn galaxy_publish_traversal_version_returns_400() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "../../etc/x");
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
        ),
    )
    .await;
    assert_eq!(resp.status(), 400, "a version that walks is never stored");
}

#[actix_web::test]
async fn galaxy_publish_traversal_namespace_returns_400() {
    let app = local_app().await;
    let tarball = build_collection("../../etc", "util", "1.0.0");
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
        ),
    )
    .await;
    assert_eq!(resp.status(), 400, "a namespace that walks is never stored");
}

#[actix_web::test]
async fn a_tarball_whose_manifest_disagrees_with_its_filename_is_400() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "2.0.0");
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
        ),
    )
    .await;
    assert_eq!(
        resp.status(),
        400,
        "stored under one coordinate and served under another is worse than a refusal"
    );
}

#[actix_web::test]
async fn a_body_that_is_not_a_collection_is_400() {
    let app = local_app().await;
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &sha256_hex(b"not a tarball"),
            b"not a tarball",
        ),
    )
    .await;
    assert_eq!(resp.status(), 400);
}

#[actix_web::test]
async fn publishing_twice_is_a_conflict() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball);
    let publish = || {
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
        )
    };
    assert_eq!(call_service(&app, publish()).await.status(), 202);
    assert_eq!(
        call_service(&app, publish()).await.status(),
        409,
        "`GalaxyError` renders a 409 as \"(HTTP Code: 409, Message: … Code: …)\""
    );
}

#[actix_web::test]
async fn publishing_to_a_proxy_registry_is_404() {
    let app = app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &sha256_hex(&tarball),
            &tarball,
        ),
    )
    .await;
    assert_eq!(resp.status(), 404, "mode = proxy has no publish endpoint");
}

// ── roles (the v1 surface) ───────────────────────────────────────────────────

#[actix_web::test]
async fn a_role_lookup_answers_the_id_the_client_then_uses() {
    let app = app().await;
    let doc = get_json(
        &app,
        &api("v1/roles/?owner__username=geerlingguy&name=docker"),
    )
    .await;
    assert_eq!(doc["results"][0]["id"], 4567);
}

#[actix_web::test]
async fn role_versions_are_one_page_with_download_urls_on_this_instance() {
    let app = app().await;
    let doc = get_json(&app, &api("v1/roles/4567/versions/")).await;
    assert!(doc["next"].is_null());
    assert!(doc["next_link"].is_null());
    assert_eq!(doc["count"], 3);
    let url = doc["results"][0]["download_url"].as_str().unwrap();
    assert!(
        url.contains("/proxy/galaxy/galaxy/api/v1/roles/4567/download/"),
        "roles = \"proxy\" rewrites download_url so the archive is fetched here, got {url}"
    );
    assert!(!url.contains("github.com"));
}

#[actix_web::test]
async fn a_blocked_role_version_is_absent_from_its_listing() {
    let app = app().await;
    // A role blocks under the name a person types, not the numeric id the
    // client addresses it by.
    block_version(&app, REG, "roles/geerlingguy.docker", "1.1.0").await;

    let doc = get_json(&app, &api("v1/roles/4567/versions/")).await;
    let served: Vec<&str> = doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(served, ["1.0.0", "2.0.0-beta.1"]);
    assert_eq!(doc["count"], 2);
}

#[actix_web::test]
async fn an_unknown_role_id_is_404_rather_than_a_cached_alias() {
    let app = app().await;
    let resp = call_service(&app, admin_get(&api("v1/roles/9999/versions/"))).await;
    assert_eq!(resp.status(), 404);
}

#[actix_web::test]
async fn a_role_id_that_is_not_an_id_is_400() {
    let app = app().await;
    let resp = call_service(&app, admin_get(&api("v1/roles/..%2f..%2fetc/versions/"))).await;
    assert!(
        resp.status() == 400 || resp.status() == 404,
        "got {}",
        resp.status()
    );
}

/// A plain-binary `file` part still publishes.
///
/// `ansible-galaxy` base64-encodes it, and the handler reads
/// `Content-Transfer-Encoding` to decide — it does not sniff. A body with no
/// such header is binary, which is what `curl -F` sends and what the RFC
/// described; both have to work, and the header is what tells them apart.
#[actix_web::test]
async fn a_binary_file_part_publishes_as_well_as_a_base64_one() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        &app,
        multipart_publish_with(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
            false,
        ),
    )
    .await;
    assert_eq!(resp.status(), 202);
    let bytes = get_bytes(&app, &artifact_url("1.0.0")).await;
    assert_eq!(sha256_hex(&bytes), digest);
}

/// A part that *declares* base64 and is not base64 is a `400` naming the field,
/// not a tar-reader error about gzip.
#[actix_web::test]
async fn a_part_that_lies_about_its_encoding_is_400() {
    let app = local_app().await;
    const BOUNDARY: &str = "----batlehubgalaxy";
    let body = format!(
        "--{BOUNDARY}\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: form-data; \
         name=\"file\"; filename=\"acme-util-1.0.0.tar.gz\"\r\n\r\n!!!not base64!!!\r\n\
         --{BOUNDARY}--\r\n"
    );
    let req = TestRequest::post()
        .uri(&api("v3/artifacts/collections/"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .insert_header((
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        ))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("Content-Transfer-Encoding"),
        "the refusal should name the encoding, not the gzip header: {message}"
    );
}

/// `ansible-galaxy` presents `Authorization: Token <token>`, not `Bearer`.
///
/// `GalaxyToken.token_type` is the literal `Token` — Django REST Framework's
/// scheme, which galaxy_ng speaks. Only `KeycloakToken` (Automation Hub, an
/// explicit non-goal) uses `Bearer`. Without the extractor normalising it,
/// every authenticated read and every publish from a configured client arrives
/// anonymous, and a registry closed to anonymous callers refuses the client
/// holding its token.
#[actix_web::test]
async fn the_token_scheme_the_client_sends_is_accepted() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball);

    const BOUNDARY: &str = "----batlehubgalaxy";
    let mut body: Vec<u8> = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"sha256\"\r\n\r\n{digest}\r\n\
         --{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
         filename=\"acme-util-1.0.0.tar.gz\"\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&tarball);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    let req = TestRequest::post()
        .uri(&api("v3/artifacts/collections/"))
        // The scheme the client actually sends.
        .insert_header(("Authorization", format!("Token {ADMIN_TOKEN}")))
        .insert_header((
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        ))
        .set_payload(body)
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(
        resp.status(),
        202,
        "a `Token`-scheme credential must authenticate, or an authenticated \
         galaxy registry works for nobody"
    );
}

// ── the install a client actually performs ───────────────────────────────────
//
// The tests above prove each document in isolation, against the routes *they*
// construct. That is the shape of gap this section closes: a suite that builds
// every URL itself agrees with itself, and `download_url` — the one URL the
// client follows rather than builds — was asserted only in proxy mode. A local
// registry whose version document still carried the `null` it is composed with
// would have passed every test in this file and failed every real install.
//
// What the client builds, and what it follows, read from ansible-core 2.19.3:
//
// - `api/`, `v3/collections/{ns}/{n}/`, `v3/collections/{ns}/{n}/versions/` and
//   `…/versions/{v}/` are built from `api_server` (`_urljoin`), so a test may
//   build them too — the listing with `?limit=100` on it, which is what
//   `get_collection_versions` sends;
// - `download_url` is *followed* (`urljoin(self.api_server, data['download_url'])`),
//   and refused outright with `Invalid non absolute download_url` when it has
//   neither a scheme nor a leading `/`;
// - `links.next` is followed the same way, under the same check — which is the
//   second reason every listing here carries a null one.

/// The path of an absolute URL a document advertised.
///
/// Asserting the scheme is half the point: `get_collection_version_metadata`
/// raises `Invalid non absolute download_url` on a value with neither a scheme
/// nor a leading `/`, so a document carrying a bare filename fails in the
/// client — where no test of this server would see it.
fn advertised_path(url: &str, field: &str) -> String {
    assert!(
        url.starts_with("http://") || url.starts_with("https://"),
        "{field} has to be absolute or the client refuses it outright, got {url:?}"
    );
    let rest = url.splitn(4, '/').nth(3).unwrap_or_default();
    format!("/{rest}")
}

/// Publish one collection as the client does, and return its bytes and digest.
async fn publish_collection<S: TestService>(
    app: &S,
    namespace: &str,
    name: &str,
    version: &str,
) -> (Vec<u8>, String) {
    publish_tarball(
        app,
        namespace,
        name,
        version,
        build_collection(namespace, name, version),
    )
    .await
}

/// [`publish_collection`] for a tarball the caller built itself.
async fn publish_tarball<S: TestService>(
    app: &S,
    namespace: &str,
    name: &str,
    version: &str,
    tarball: Vec<u8>,
) -> (Vec<u8>, String) {
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            &format!("{namespace}-{name}-{version}.tar.gz"),
            &digest,
            &tarball,
        ),
    )
    .await;
    assert_eq!(
        resp.status(),
        202,
        "publishing {namespace}.{name} {version} should be accepted"
    );
    (tarball, digest)
}

/// The whole install, end to end, over a locally published collection —
/// following `download_url` instead of rebuilding it.
#[actix_web::test]
async fn an_install_walks_the_four_documents_and_follows_download_url() {
    let app = local_app().await;
    let (_, digest) = publish_collection(&app, "acme", "util", "1.0.0").await;

    // 1. `g_connect`, before any action. Composed here, so it answers in local
    //    mode as well — a registry that only serves what it was published to
    //    still has to advertise `v3` or the client stops at the version check.
    let discovery = get_json(&app, &api("")).await;
    assert_eq!(discovery["available_versions"]["v3"], "v3/");

    // 2. the collection document. `get_collection_metadata` reads `updated_at`
    //    as `modified_str` and compares it with what it cached to decide
    //    whether its day-old listing is still good; a null there means the
    //    client never notices a publish for a day.
    let collection = get_json(&app, &collection_url()).await;
    assert_eq!(collection["highest_version"]["version"], "1.0.0");
    assert!(
        collection["updated_at"].as_str().is_some(),
        "updated_at is the client's cache key for the listing: {collection}"
    );
    let advertised_versions = advertised_path(
        collection["versions_url"].as_str().expect("versions_url"),
        "versions_url",
    );
    assert_eq!(advertised_versions, versions_url());

    // 3. the listing, at the URL the client builds — page size and all.
    let listing = get_json(&app, &format!("{}?limit=100", versions_url())).await;
    assert_eq!(versions_in(&listing), ["1.0.0"]);
    assert!(
        listing["links"]["next"].is_null(),
        "a non-null next is followed under the same absolute-URL check: {listing}"
    );

    // 4. the version document, and the one URL in this walk that is *followed*.
    let detail = get_json(&app, &version_url("1.0.0")).await;
    let download = advertised_path(
        detail["download_url"].as_str().expect("download_url"),
        "download_url",
    );
    assert!(
        download.ends_with("/v3/artifacts/collections/acme-util-1.0.0.tar.gz"),
        "`_download_file` names the file it writes by slicing `.tar.gz` off the last path \
         segment, got {download}"
    );
    let bytes = get_bytes(&app, &download).await;
    assert_eq!(
        sha256_hex(&bytes),
        digest,
        "the client hashes the body as it streams and compares it with artifact.sha256"
    );
}

/// The seven fields `get_collection_version_metadata` indexes **by key**.
///
/// ```python
/// return CollectionVersionMetadata(data['namespace']['name'], data['collection']['name'],
///                                  data['version'], download_url, data['artifact']['sha256'],
///                                  data['metadata']['dependencies'], data['href'], signatures)
/// ```
///
/// Not `.get()` — so a field missing from a locally composed document is a
/// `KeyError` inside the client, which surfaces as an unhandled traceback and
/// not as a status this server ever sees.
#[actix_web::test]
async fn the_local_version_document_carries_every_field_the_client_indexes() {
    let app = local_app().await;
    let (_, digest) = publish_collection(&app, "acme", "util", "1.0.0").await;
    let doc = get_json(&app, &version_url("1.0.0")).await;

    assert_eq!(doc["namespace"]["name"], "acme");
    assert_eq!(doc["collection"]["name"], "util");
    assert_eq!(doc["version"], "1.0.0");
    assert_eq!(doc["artifact"]["sha256"], digest);
    assert!(doc["download_url"].as_str().is_some());
    assert!(doc["href"].as_str().is_some());
    assert!(
        doc["metadata"]["dependencies"].is_object(),
        "metadata.dependencies is indexed unconditionally: {doc}"
    );
    // Read with `.get()`, and honest: a locally published collection is
    // unsigned, and an absent key would be read the same way.
    assert_eq!(doc["signatures"], serde_json::json!([]));
}

/// A dependency declared in `MANIFEST.json` reaches the document the resolver
/// reads.
#[actix_web::test]
async fn a_declared_dependency_survives_the_publish_into_the_version_document() {
    let app = local_app().await;
    let deps = serde_json::json!({ "acme.base": ">=1.0.0" });
    let tarball = build_collection_with("acme", "util", "1.0.0", deps.clone());
    publish_tarball(&app, "acme", "util", "1.0.0", tarball).await;

    let doc = get_json(&app, &version_url("1.0.0")).await;
    assert_eq!(
        doc["metadata"]["dependencies"], deps,
        "the resolver builds its graph from this map; an empty one installs a collection \
         whose dependencies are never fetched"
    );
}

/// A second publish moves `highest_version`, and leaves the first installable.
///
/// The second half is the pinned install: `install acme.util:==1.0.0` resolves
/// through the listing and then reads that version's own document, which is a
/// different path from the one a range takes.
#[actix_web::test]
async fn a_second_publish_moves_the_highest_version_and_keeps_the_first_installable() {
    let app = local_app().await;
    let (_, first) = publish_collection(&app, "acme", "util", "1.0.0").await;
    publish_collection(&app, "acme", "util", "1.2.0").await;

    let listing = get_json(&app, &versions_url()).await;
    let mut versions = versions_in(&listing);
    versions.sort();
    assert_eq!(versions, ["1.0.0", "1.2.0"]);
    assert_eq!(listing["meta"]["count"], 2);

    let collection = get_json(&app, &collection_url()).await;
    assert_eq!(
        collection["highest_version"]["version"], "1.2.0",
        "the newest published version, not the first one seen"
    );

    let detail = get_json(&app, &version_url("1.0.0")).await;
    assert_eq!(detail["artifact"]["sha256"], first);
    let download = advertised_path(
        detail["download_url"].as_str().expect("download_url"),
        "download_url",
    );
    assert_eq!(sha256_hex(&get_bytes(&app, &download).await), first);
}

/// A yank leaves the listing, and stays installable by exact pin.
///
/// Galaxy's protocol has no yank of its own — the surface is the admin bulk
/// route — so this is the only place the `yanked` filter in the composed
/// listing is exercised. The two halves are the difference between a yank and a
/// **block**, which the tests above pin at `404` on the version document and
/// `403` on the tarball: a yank is absent from the list the resolver chooses
/// from, so nothing new resolves to it, and stays resolvable by exact pin for
/// the lockfiles that already name it
/// (`docs/guide/admin-policies.md` § deprecate, yank or delete). A test that
/// asserted `404` here would be asserting a block.
#[actix_web::test]
async fn a_yanked_version_leaves_the_listing_and_stays_pinnable() {
    let app = local_app().await;
    publish_collection(&app, "acme", "util", "1.0.0").await;
    publish_collection(&app, "acme", "util", "1.1.0").await;

    let req = TestRequest::post()
        .uri(&format!("/api/v1/admin/registries/{REG}/bulk-yank"))
        .insert_header(("Authorization", bearer(ADMIN_TOKEN)))
        .set_json(serde_json::json!({
            "packages": [{ "name": COLLECTION, "version": "1.0.0" }]
        }))
        .to_request();
    let resp = call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let listing = get_json(&app, &versions_url()).await;
    assert_eq!(
        versions_in(&listing),
        ["1.1.0"],
        "the resolver picks from this document, so a yank has to be absent from it"
    );
    assert_eq!(listing["meta"]["count"], 1);
    let collection = get_json(&app, &collection_url()).await;
    assert_eq!(collection["highest_version"]["version"], "1.1.0");

    // …and the pin still works, bytes and all.
    let detail = get_json(&app, &version_url("1.0.0")).await;
    let download = advertised_path(
        detail["download_url"].as_str().expect("download_url"),
        "download_url",
    );
    let bytes = get_bytes(&app, &download).await;
    assert_eq!(
        sha256_hex(&bytes),
        detail["artifact"]["sha256"].as_str().unwrap(),
        "a yanked version an existing lockfile pins still installs, byte-exact"
    );
}

/// Hybrid mode: the published collection is served from here, everything else
/// falls through upstream.
///
/// The mode a real deployment runs — internal collections beside the public
/// ones — and the one no in-process galaxy test covered.
#[actix_web::test]
async fn a_hybrid_registry_serves_what_was_published_and_proxies_the_rest() {
    let app = registry_app(REG, "galaxy", RegistryMode::Hybrid).await;
    let (_, digest) = publish_collection(&app, "acme", "util", "1.0.0").await;

    // The published collection: the local rows, and only those. Upstream's
    // fixture serves three versions of every collection it is asked for, so a
    // listing that leaked them would name 1.1.0 and 2.0.0-beta.1 as well.
    let listing = get_json(&app, &versions_url()).await;
    assert_eq!(
        versions_in(&listing),
        ["1.0.0"],
        "a hybrid registry answers from its own rows when it has them"
    );
    let detail = get_json(&app, &version_url("1.0.0")).await;
    assert_eq!(detail["artifact"]["sha256"], digest);
    let download = advertised_path(
        detail["download_url"].as_str().expect("download_url"),
        "download_url",
    );
    assert_eq!(sha256_hex(&get_bytes(&app, &download).await), digest);

    // Anything else: upstream's listing, through the same route.
    let other = get_json(&app, &api("v3/collections/other/thing/versions/")).await;
    assert_eq!(
        versions_in(&other),
        ["1.0.0", "1.1.0", "2.0.0-beta.1"],
        "an unpublished collection has to fall through, or a hybrid registry is a local one"
    );
}

// ── the upload path ─────────────────────────────────────────────────────────

/// An anonymous publish is refused before anything is stored.
#[actix_web::test]
async fn an_anonymous_publish_is_refused() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        &app,
        publish_request(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            Some(&digest),
            &tarball,
            true,
            None,
        ),
    )
    .await;
    assert!(
        resp.status() == 401 || resp.status() == 403,
        "an unauthenticated publish must not be accepted, got {}",
        resp.status()
    );
    assert_eq!(
        call_service(&app, admin_get(&versions_url()))
            .await
            .status(),
        404,
        "nothing may have been stored"
    );
}

/// The `sha256` field is optional, and its absence does not mean "unverified":
/// the server hashes the bytes either way and publishes that digest, which is
/// what the client then checks the download against.
#[actix_web::test]
async fn a_publish_without_the_sha256_field_still_publishes_the_real_digest() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        &app,
        publish_request(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            None,
            &tarball,
            true,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(resp.status(), 202);
    let detail = get_json(&app, &version_url("1.0.0")).await;
    assert_eq!(detail["artifact"]["sha256"], digest);
}

/// A digest in upper-case hex is the same digest.
#[actix_web::test]
async fn an_uppercase_sha256_field_is_accepted() {
    let app = local_app().await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball).to_uppercase();
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
        ),
    )
    .await;
    assert_eq!(
        resp.status(),
        202,
        "the comparison is case-insensitive: hex has no case"
    );
}

/// Publishing into a hybrid registry is accepted — `require_local_mode` admits
/// local *and* hybrid, and hybrid is the mode a registry that both proxies and
/// hosts runs in.
#[actix_web::test]
async fn publishing_into_a_hybrid_registry_is_accepted() {
    let app = registry_app(REG, "galaxy", RegistryMode::Hybrid).await;
    let tarball = build_collection("acme", "util", "1.0.0");
    let digest = sha256_hex(&tarball);
    let resp = call_service(
        &app,
        multipart_publish(
            &api("v3/artifacts/collections/"),
            "acme-util-1.0.0.tar.gz",
            &digest,
            &tarball,
        ),
    )
    .await;
    assert_eq!(resp.status(), 202);
}
