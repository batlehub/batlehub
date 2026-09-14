//! Open VSX: the extension document and the VSIX it links to.
//!
//! One document (`/api/{namespace}/{name}`, optionally with a version) whose
//! `files.download` is the VSIX. That URL is on this mock's own host, which is
//! the shape a self-hosted Open VSX has and the one the proxy's rewriter has to
//! handle: open-vsx.org redirects `files.*` elsewhere, and a mock that did the
//! same would measure the redirect follower instead of the proxy.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::*;
use crate::Args;

const VERSIONS: [&str; 3] = ["1.0.0", "1.1.0", "1.2.0"];

fn extension_doc(host: &str, namespace: &str, name: &str, version: &str) -> serde_json::Value {
    let all: serde_json::Map<String, serde_json::Value> = VERSIONS
        .iter()
        .map(|v| {
            (
                (*v).to_owned(),
                serde_json::Value::String(format!(
                    "http://{host}/openvsx/api/{namespace}/{name}/{v}"
                )),
            )
        })
        .collect();
    serde_json::json!({
        "namespace": namespace,
        "name": name,
        "version": version,
        "timestamp": "2024-01-01T00:00:00Z",
        "displayName": name,
        "description": "mock extension for perf tests",
        "downloadCount": 42,
        "verified": true,
        "allVersions": all,
        "files": {
            "download": format!(
                "http://{host}/openvsx/files/{namespace}/{name}/{version}/{name}-{version}.vsix"
            ),
        },
    })
}

/// `GET /openvsx/api/{namespace}/{name}` — the latest version's document.
#[route("/openvsx/api/{namespace}/{name}", method = "GET", method = "HEAD")]
async fn extension_latest(
    req: HttpRequest,
    path: web::Path<(String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (namespace, name) = path.into_inner();
    let host = host(&req);
    HttpResponse::Ok()
        .content_type("application/json")
        .body(extension_doc(&host, &namespace, &name, VERSIONS[VERSIONS.len() - 1]).to_string())
}

/// `GET /openvsx/api/{namespace}/{name}/{version}` — one version's document.
#[route(
    "/openvsx/api/{namespace}/{name}/{version}",
    method = "GET",
    method = "HEAD"
)]
async fn extension_version(
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (namespace, name, version) = path.into_inner();
    let host = host(&req);
    HttpResponse::Ok()
        .content_type("application/json")
        .body(extension_doc(&host, &namespace, &name, &version).to_string())
}

/// `GET /openvsx/files/{namespace}/{name}/{version}/{filename}` — the VSIX.
#[route(
    "/openvsx/files/{namespace}/{name}/{version}/{filename}",
    method = "GET",
    method = "HEAD"
)]
async fn vsix(
    path: web::Path<(String, String, String, String)>,
    args: web::Data<Args>,
) -> HttpResponse {
    delay(args.delay_ms).await;
    let (_ns, _name, version, filename) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(
            &filename,
            &version,
            args.artifact_size_kb * 1024,
        ))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(extension_latest)
        .service(extension_version)
        .service(vsix);
}
