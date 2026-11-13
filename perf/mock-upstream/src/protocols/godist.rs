//! Go's distribution: the release document and the archives under `/godist/`.
//!
//! The proxy verifies every archive against the document's `sha256` before it
//! caches it, so the document names the digest of the very bytes `archive`
//! returns — a made-up digest would have every read refused.

use actix_web::{route, web, HttpResponse};

use crate::support::{artifact_bytes, delay, sha256_hex};
use crate::Args;

const RELEASES: [&str; 2] = ["1.25.4", "1.25.3"];

fn archive_name(version: &str) -> String {
    format!("go{version}.linux-amd64.tar.gz")
}

/// `GET /godist/dl/?mode=json&include=all` — the release document.
#[route("/godist/dl/", method = "GET", method = "HEAD")]
async fn document(args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let size = args.artifact_size_kb * 1024;
    let rows: Vec<_> = RELEASES
        .iter()
        .map(|v| {
            let file = archive_name(v);
            let bytes = artifact_bytes(&file, v, size);
            serde_json::json!({
                "version": format!("go{v}"),
                "stable": true,
                "files": [{
                    "filename": file, "os": "linux", "arch": "amd64",
                    "version": format!("go{v}"), "sha256": sha256_hex(&bytes),
                    "size": bytes.len(), "kind": "archive",
                }],
            })
        })
        .collect();
    HttpResponse::Ok()
        .content_type("application/json")
        .body(serde_json::Value::Array(rows).to_string())
}

/// `GET /godist/{file}` — an archive; the bytes the document's digest names.
#[route("/godist/{file}", method = "GET", method = "HEAD")]
async fn archive(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let file = path.into_inner();
    let version = file
        .strip_prefix("go")
        .and_then(|s| s.strip_suffix(".linux-amd64.tar.gz"))
        .unwrap_or("");
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(&file, version, args.artifact_size_kb * 1024))
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(document).service(archive);
}
