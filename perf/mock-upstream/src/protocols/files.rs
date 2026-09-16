//! The path-shaped kinds: `generic`, `deb`, `rpm`, `pacman`, `apk` and
//! `jetbrains`.
//!
//! One module for five registry types because the proxy treats them as one: all
//! five are served by `PathProxyRegistryClient`, whose whole contract is
//! "stream `{base_url}/{path}`". There is no metadata API to answer and no
//! document to parse — for these formats the index files *are* the metadata,
//! fetched as ordinary artifacts — so an upstream that returns bytes for any
//! path is a complete upstream.
//!
//! That is why the four beyond `generic` cost the mock one route each rather
//! than a protocol. It is also the limit of what soaking them proves: this
//! exercises the proxy's path validation, cache and streaming for four more
//! registry types, and says nothing about whether a real `apt` would accept the
//! index — `tests/heavy/closed_world.sh` is where that is settled.

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay};
use crate::Args;

/// `GET /generic/{path}` — the kind whose protocol is "a URL is a file".
#[route("/generic/{path:.*}", method = "GET", method = "HEAD")]
async fn generic_file(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    file_response(&path.into_inner(), &args).await
}

/// `GET /deb/{path}` — a pool file or an index, both just bytes from here.
#[route("/deb/{path:.*}", method = "GET", method = "HEAD")]
async fn deb_file(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    file_response(&path.into_inner(), &args).await
}

/// `GET /rpm/{path}` — `repodata/*` and the `.rpm`s alike.
#[route("/rpm/{path:.*}", method = "GET", method = "HEAD")]
async fn rpm_file(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    file_response(&path.into_inner(), &args).await
}

/// `GET /pacman/{path}` — the database tarball and the packages.
#[route("/pacman/{path:.*}", method = "GET", method = "HEAD")]
async fn pacman_file(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    file_response(&path.into_inner(), &args).await
}

/// `GET /apk/{path}` — an Alpine tree: the index, then the packages.
///
/// The only path kind that needs more than `file_response`, because it is the
/// only one the proxy *reads*: `ApkRegistryClient::resolve_metadata` pulls the
/// `APKINDEX.tar.gz` beside a `.apk` to recover its build date for the age
/// gate. An upstream answering random bytes there would make every `.apk`
/// request pay a failed gzip parse and measure the wrong thing.
#[route("/apk/{path:.*}", method = "GET", method = "HEAD")]
async fn apk_file(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    let path = path.into_inner();
    if path.ends_with("APKINDEX.tar.gz") {
        delay(args.delay_ms).await;
        return HttpResponse::Ok()
            .content_type("application/octet-stream")
            .body(apk_index(args.apk_packages));
    }
    file_response(&path, &args).await
}

/// A synthetic `APKINDEX.tar.gz`: two concatenated gzip members, the signature
/// first, exactly as a mirror serves one.
///
/// The entries are named to match the `.apk` paths the soak asks for, so the
/// age gate finds a `t:` and the arm measures a *hit* rather than the
/// undated fall-through.
fn apk_index(packages: usize) -> Vec<u8> {
    use std::io::Write;

    let mut body = String::new();
    for n in 0..packages {
        body.push_str(&format!(
            "C:Q1{n:0>26}=\nP:demo\nV:1.{n}-r0\nA:x86_64\nS:1024\nt:1700000000\n\n"
        ));
    }

    let gz = |bytes: &[u8]| {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    };
    let tar_of = |entries: &[(&str, &str)]| {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_cksum();
            tar.append_data(&mut header, name, content.as_bytes())
                .unwrap();
        }
        tar.into_inner().unwrap()
    };

    [
        gz(&tar_of(&[(".SIGN.RSA.perf.rsa.pub", "sig\n")])),
        gz(&tar_of(&[("DESCRIPTION", "perf\n"), ("APKINDEX", &body)])),
    ]
    .concat()
}

/// `GET /jetbrains/{path}` — an IDE archive on the download CDN.
#[route("/jetbrains/{path:.*}", method = "GET", method = "HEAD")]
async fn jetbrains_file(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    file_response(&path.into_inner(), &args).await
}

async fn file_response(path: &str, args: &Args) -> HttpResponse {
    delay(args.delay_ms).await;
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(path, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(generic_file)
        .service(deb_file)
        .service(rpm_file)
        .service(pacman_file)
        .service(apk_file)
        .service(jetbrains_file);
}
