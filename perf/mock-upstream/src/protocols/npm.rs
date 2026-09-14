//! npm: a packument and the tarball it advertises.
//!
//! The packument's `dist.shasum` is computed from the bytes `npm_tarball` will
//! serve, not invented: the proxy verifies what an upstream advertises, so a
//! made-up digest turns every artifact read into a 502 (see `support.rs`).

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::*;
use crate::Args;

/// npm packument: GET /{name}
/// Serves a minimal packument with a single version whose tarball points back
/// to this mock server so the proxy fetches it from here too.
#[route("/npm/{name}", method = "GET", method = "HEAD")]
async fn npm_packument(
    req: HttpRequest,
    name: web::Path<String>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;

    let host = host(&req);
    let pkg = name.into_inner();
    let version = "1.0.0";
    let tarball_url = format!("http://{}/npm/{}/-/{}-{}.tgz", host, pkg, pkg, version);
    // The digest of the bytes `npm_tarball` will serve for this coordinate,
    // computed rather than invented — see `artifact_bytes`.
    let shasum = sha1_hex(&artifact_bytes(&pkg, version, args.artifact_size_kb * 1024));

    let body = serde_json::json!({
        "name": pkg,
        "dist-tags": { "latest": version },
        "versions": {
            version: {
                "name": pkg,
                "version": version,
                "description": "mock package for perf tests",
                "dist": {
                    "tarball": tarball_url,
                    "shasum": shasum
                }
            }
        },
        "time": {
            version: "2024-01-01T00:00:00.000Z"
        }
    });

    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// npm tarball: GET /{name}/-/{filename}.tgz
///
/// The bytes are derived from the coordinate, so two fetches of the same
/// tarball are the same tarball and both match the `shasum` the packument
/// advertised. See `artifact_bytes`.
#[route("/npm/{name}/-/{filename}", method = "GET", method = "HEAD")]
async fn npm_tarball(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (name, _filename) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&name, "1.0.0", args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(npm_packument).service(npm_tarball);
}
