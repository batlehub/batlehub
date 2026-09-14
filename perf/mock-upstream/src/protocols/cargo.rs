//! Cargo: the sparse index and the `.crate` download.
//!
//! Two upstreams rather than one, because a cargo registry has two: the API
//! (`api/v1/crates/{name}`) answers metadata, and the **sparse index** — a
//! separate base URL, configured as `index_url` — answers newline-delimited
//! JSON at cargo's own `1/a`, `2/ab`, `3/a/abc`, `ab/cd/abcdef` layout. A mock
//! without the index can serve a download and nothing a client could discover.

use actix_web::{route, web, HttpRequest, HttpResponse};

use crate::support::*;
use crate::Args;

/// Sparse cargo index config: GET /cargo/config.json
#[route("/cargo/config.json", method = "GET", method = "HEAD")]
async fn cargo_index(req: HttpRequest) -> HttpResponse {
    let host = host(&req);
    let body = serde_json::json!({
        "dl": format!("http://{}/cargo/{{crate}}/{{version}}/download", host),
        "api": format!("http://{}/cargo", host)
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// Cargo crate download: GET /cargo/{name}/{version}/download
#[route("/cargo/{name}/{version}/download", method = "GET", method = "HEAD")]
async fn cargo_download(path: web::Path<(String, String)>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let (name, version) = path.into_inner();
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .body(artifact_bytes(
            &name,
            &version,
            args.artifact_size_kb * 1024,
        ))
}

/// `GET /cargo-index/config.json` — where the index says downloads live.
#[route("/cargo-index/config.json", method = "GET", method = "HEAD")]
async fn cargo_index_config(req: HttpRequest) -> HttpResponse {
    let host = host(&req);
    let body = serde_json::json!({
        "dl": format!("http://{}/cargo/{{crate}}/{{version}}/download", host),
        "api": format!("http://{}/cargo", host)
    });
    HttpResponse::Ok()
        .content_type("application/json")
        .body(body.to_string())
}

/// `GET /cargo-index/{path}` — one crate's index entry, newline-delimited JSON.
///
/// The crate name is the last path segment whatever the prefix depth, which is
/// how the proxy reads it back out of the request path too.
#[route("/cargo-index/{path:.*}", method = "GET", method = "HEAD")]
async fn cargo_index_entry(path: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let path = path.into_inner();
    let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
    let mut body = String::new();
    for version in ["1.0.0", "1.1.0", "1.2.0"] {
        let checksum = sha256_hex(&artifact_bytes(
            &name,
            version,
            args.artifact_size_kb * 1024,
        ));
        body.push_str(
            &serde_json::json!({
                "name": name,
                "vers": version,
                "deps": [],
                "cksum": checksum,
                "features": {},
                "yanked": false,
            })
            .to_string(),
        );
        body.push('\n');
    }
    HttpResponse::Ok().content_type("text/plain").body(body)
}

/// `GET /cargo/api/v1/crates/{name}` — the API document `resolve_metadata`
/// reads, which is where a `.crate` download's URL comes from. Without it the
/// index is readable and every download answers "crate not found".
#[route("/cargo/api/v1/crates/{name}", method = "GET", method = "HEAD")]
async fn cargo_crate_info(name: web::Path<String>, args: web::Data<Args>) -> HttpResponse {
    delay(args.delay_ms).await;
    let name = name.into_inner();
    let versions: Vec<_> = ["1.0.0", "1.1.0", "1.2.0"]
        .iter()
        .map(|v| {
            serde_json::json!({
                "num": v,
                // Relative to `base_url`, which the client concatenates without a
                // separator — an absolute-looking `/cargo/…` here becomes
                // `…/cargo/cargo/…` and every download is a 502.
                "dl_path": format!("/{name}/{v}/download"),
                "checksum": sha256_hex(&artifact_bytes(&name, v, args.artifact_size_kb * 1024)),
                "created_at": "2024-01-01T00:00:00+00:00",
                "yanked": false,
            })
        })
        .collect();
    HttpResponse::Ok().content_type("application/json").body(
        serde_json::json!({
            "crate": {
                "max_version": "1.2.0",
                "repository": "https://example.com/perf",
                "homepage": serde_json::Value::Null,
            },
            "versions": versions,
        })
        .to_string(),
    )
}

pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(cargo_crate_info)
        .service(cargo_index)
        .service(cargo_download)
        .service(cargo_index_config)
        .service(cargo_index_entry);
}
