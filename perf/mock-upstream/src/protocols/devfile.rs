//! A devfile registry (RFC 0035): the v2 and legacy indexes, one OCI manifest
//! per stack version and the layers it names.
//!
//! What the proxy parses, and so what this owes: `links.self` in the v2 index
//! (it becomes the OCI reference), the manifest's layer descriptors, and a
//! `Docker-Content-Digest` equal to the manifest's own SHA-256. Every layer is
//! served by the digest its manifest names, and the proxy verifies both before
//! it stores or serves a byte — so every digest here is computed from the bytes
//! served, never invented.
//!
//! The space is fixed — four stacks of four versions — so the soak's cache
//! stays stationary (the rule `perf/k6/soak_arms.js` opens with).

use actix_web::{route, web, HttpResponse};

use crate::support::{delay, sha256_hex};
use crate::Args;

const STACKS: usize = 4;
const VERSIONS: [&str; 4] = ["1.3.0", "1.2.0", "1.1.0", "1.0.0"];

fn devfile(stack: &str, version: &str) -> Vec<u8> {
    format!(
        "schemaVersion: 2.2.2\nmetadata:\n  name: {stack}\n  version: {version}\n\
         components:\n  - name: runtime\n    container:\n      image: registry.invalid/{stack}:{version}\n"
    )
    .into_bytes()
}

fn manifest(stack: &str, version: &str) -> String {
    let body = devfile(stack, version);
    serde_json::json!({
        "schemaVersion": 2,
        "config": {"mediaType": "application/vnd.devfileio.devfile.config.v2+json",
                   "digest": format!("sha256:{}", sha256_hex(b"{}")), "size": 2},
        "layers": [{
            "mediaType": "application/vnd.devfileio.devfile.layer.v1",
            "digest": format!("sha256:{}", sha256_hex(&body)),
            "size": body.len(),
            "annotations": {"org.opencontainers.image.title": "devfile.yaml"}
        }]
    })
    .to_string()
}

fn stacks() -> impl Iterator<Item = String> {
    (0..STACKS).map(|n| format!("acme{n}"))
}

/// `GET /devfile/v2index[/…]` — every stack, every version, the first default.
#[route("/devfile/v2index{rest:(/all|/stack|/sample)?}", method = "GET", method = "HEAD")]
async fn v2index(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let entries: Vec<_> = stacks()
        .map(|stack| {
            let versions: Vec<_> = VERSIONS
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    serde_json::json!({
                        "version": v,
                        "default": i == 0,
                        "links": {"self": format!("devfile-catalog/{stack}:{v}")},
                        "resources": ["devfile.yaml"],
                        "starterProjects": [],
                    })
                })
                .collect();
            serde_json::json!({"name": stack, "type": "stack", "versions": versions})
        })
        .collect();
    HttpResponse::Ok()
        .content_type("text/plain; charset=utf-8")
        .body(serde_json::Value::Array(entries).to_string())
}

/// `GET /devfile/index[/…]` — the legacy shape: one entry per stack, its default.
#[route("/devfile/index{rest:(/all|/stack|/sample)?}", method = "GET", method = "HEAD")]
async fn index(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let entries: Vec<_> = stacks()
        .map(|stack| {
            serde_json::json!({
                "name": stack, "type": "stack", "version": VERSIONS[0],
                "links": {"self": format!("devfile-catalog/{stack}:{}", VERSIONS[0])},
            })
        })
        .collect();
    HttpResponse::Ok()
        .content_type("text/plain; charset=utf-8")
        .body(serde_json::Value::Array(entries).to_string())
}

/// `GET /devfile/v2/devfile-catalog/{stack}/manifests/{tag}`.
#[route(
    "/devfile/v2/devfile-catalog/{stack}/manifests/{tag}",
    method = "GET",
    method = "HEAD"
)]
async fn manifest_route(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (stack, tag) = path.into_inner();
    let body = manifest(&stack, &tag);
    HttpResponse::Ok()
        .content_type("application/vnd.oci.image.manifest.v1+json")
        .insert_header(("Docker-Content-Digest", format!("sha256:{}", sha256_hex(body.as_bytes()))))
        .body(body)
}

/// `GET /devfile/v2/devfile-catalog/{stack}/blobs/sha256:{hex}` — the layer
/// whose digest this is, found among the stack's versions.
#[route(
    "/devfile/v2/devfile-catalog/{stack}/blobs/{digest}",
    method = "GET",
    method = "HEAD"
)]
async fn blob(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (stack, digest) = path.into_inner();
    let hex = digest.trim_start_matches("sha256:");
    VERSIONS
        .iter()
        .map(|v| devfile(&stack, v))
        .find(|b| sha256_hex(b) == hex)
        .map(|b| {
            HttpResponse::Ok()
                .content_type("application/octet-stream")
                .body(b)
        })
        .unwrap_or_else(|| HttpResponse::NotFound().finish())
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(v2index)
        .service(index)
        .service(manifest_route)
        .service(blob);
}
