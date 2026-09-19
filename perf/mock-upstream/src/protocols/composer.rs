//! Composer: the Packagist v2 metadata endpoint and a dist zip.
//!
//! `p2/{vendor}/{package}.json` is `{"packages": {name: [version entries]}}`,
//! and each entry's `dist.url` points back here. `dist.shasum` is SHA-1 because
//! the format defines it as one (see `docs/operations/weak-hashes.md`), and it
//! is computed from the bytes `dist` will serve — Composer hashes the
//! downloaded zip and compares, and so does this proxy.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::{artifact_bytes, delay, host, sha1_hex};
use crate::Args;

const VERSIONS: [&str; 3] = ["1.0.0", "1.1.0", "1.2.0"];

/// `GET /composer/packages.json` — the root document, which names p2.
#[route("/composer/packages.json", method = "GET", method = "HEAD")]
async fn packages_json(req: HttpRequest) -> HttpResponse {
    let host = host(&req);
    let body = serde_json::json!({
        "metadata-url": format!("http://{host}/composer/p2/%package%.json"),
        "providers-api": format!("http://{host}/composer/p2/%package%.json"),
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// `GET /composer/p2/{vendor}/{package}.json`
#[route(
    "/composer/p2/{vendor}/{package}.json",
    method = "GET",
    method = "HEAD"
)]
async fn p2(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (vendor, package) = path.into_inner();
    let name = format!("{vendor}/{package}");
    let host = host(&req);
    let entries: Vec<_> = VERSIONS
        .iter()
        .map(|v| {
            let filename = format!("{vendor}-{package}-{v}.zip");
            let bytes = artifact_bytes(&filename, "", args.artifact_size_kb * 1024);
            serde_json::json!({
                "name": name,
                "version": v,
                "time": "2024-01-01T00:00:00+00:00",
                "dist": {
                    "type": "zip",
                    "url": format!("http://{host}/composer/dist/{filename}"),
                    "shasum": sha1_hex(&bytes),
                    "reference": v,
                },
            })
        })
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::json!({ "packages": { name: entries } }).to_string())
}

/// `GET /composer/dist/{filename}` — the zip `dist.url` named.
#[route("/composer/dist/{filename}", method = "GET", method = "HEAD")]
async fn dist(filename: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let filename = filename.into_inner();
    HttpResponse::Ok()
        .content_type("application/zip")
        .body(artifact_bytes(&filename, "", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(packages_json).service(p2).service(dist);
}
