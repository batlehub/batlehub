//! The path-shaped kinds: `generic`, `deb`, `rpm`, `pacman` and `jetbrains`.
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

use crate::support::*;
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
        .service(jetbrains_file);
}
